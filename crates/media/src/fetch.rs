//! Downloading preview candidates.
//!
//! [`HttpFetcher`] is the production loader: HTTP(S) only, redirects
//! followed by hand so each target is checked (`policy::check_request`), a
//! resolver that drops every non-public address before connecting (covering
//! DNS names and rebinding, not only literal IPs), no proxy, no cookies, no
//! `Referer`, no compression, and bounded time and bytes. Tests inject their
//! own [`Fetcher`].

use std::{io::Read, net::SocketAddr, sync::Arc};

use ureq::{
    config::Config,
    http::Uri,
    unversioned::{
        resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver},
        transport::{DefaultConnector, NextTimeout},
    },
};
use url::Url;

use crate::{
    CancelFlag, Limits, LoadError, MediaRef,
    policy::{self, Rules},
};

/// Produces the bytes of a media reference. Implementations must honor the
/// limits and the cancel flag, and never block indefinitely.
pub trait Fetcher: Send + Sync {
    fn fetch(
        &self,
        source: &MediaRef,
        limits: &Limits,
        cancel: &CancelFlag,
    ) -> Result<Vec<u8>, LoadError>;
}

const ACCEPT: &str = "image/png,image/jpeg,image/gif,image/webp;q=0.9";
const READ_CHUNK: usize = 16 * 1024;

pub struct HttpFetcher {
    agent: ureq::Agent,
    rules: Rules,
}

impl HttpFetcher {
    pub fn new(limits: &Limits) -> Self {
        Self::with_rules(limits, Rules::default())
    }

    /// A fetcher that may also reach `127.0.0.1` on any port, for tests with
    /// a local fixture server. Everything else is the production policy.
    #[cfg(test)]
    pub(crate) fn for_local_fixture(limits: &Limits) -> Self {
        Self::with_rules(
            limits,
            Rules {
                loopback_fixture: true,
            },
        )
    }

    fn with_rules(limits: &Limits, rules: Rules) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_connect(Some(limits.connect_timeout))
            .timeout_global(Some(limits.total_timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .proxy(None)
            .https_only(false)
            .max_idle_connections(2)
            .max_idle_connections_per_host(1)
            .user_agent(concat!(
                "CayenChat/",
                env!("CARGO_PKG_VERSION"),
                " (image preview)"
            ))
            .build();
        let agent =
            ureq::Agent::with_parts(config, DefaultConnector::new(), CheckedResolver { rules });
        Self { agent, rules }
    }

    fn get(&self, url: &Url, limits: &Limits, cancel: &CancelFlag) -> Result<Step, LoadError> {
        let response = self
            .agent
            .get(url.as_str())
            .header("Accept", ACCEPT)
            .call()
            .map_err(|error| match error {
                ureq::Error::Other(_) => LoadError::Blocked,
                _ => LoadError::Network,
            })?;
        let status = response.status().as_u16();
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let location = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .ok_or(LoadError::Status(status))?;
            let next = url.join(location).map_err(|_| LoadError::Blocked)?;
            return Ok(Step::Redirect(next));
        }
        if status != 200 {
            return Err(LoadError::Status(status));
        }
        let body = response.into_body();
        // The extension was only a hint; a page or any non-image type ends here.
        let declared = body.mime_type().unwrap_or_default().to_ascii_lowercase();
        if !declared.starts_with("image/") || declared.contains("svg") {
            return Err(LoadError::NotImage);
        }
        let max = limits.max_response_bytes;
        if body
            .content_length()
            .is_some_and(|length| length > max as u64)
        {
            return Err(LoadError::TooLarge);
        }
        let mut reader = body.into_with_config().limit(max as u64 + 1).reader();
        let mut bytes = Vec::new();
        let mut chunk = vec![0; READ_CHUNK];
        loop {
            if cancel.is_cancelled() {
                return Err(LoadError::Cancelled);
            }
            let read = reader.read(&mut chunk).map_err(|error| {
                if error.to_string().contains("limit") {
                    LoadError::TooLarge
                } else {
                    LoadError::Network
                }
            })?;
            if read == 0 {
                return Ok(Step::Body(bytes));
            }
            if bytes.len() + read > max {
                return Err(LoadError::TooLarge);
            }
            bytes.extend_from_slice(&chunk[..read]);
        }
    }
}

enum Step {
    Redirect(Url),
    Body(Vec<u8>),
}

impl Fetcher for HttpFetcher {
    fn fetch(
        &self,
        source: &MediaRef,
        limits: &Limits,
        cancel: &CancelFlag,
    ) -> Result<Vec<u8>, LoadError> {
        let MediaRef::Link(link) = source;
        let mut url = link.clone();
        let mut previous: Option<Url> = None;
        for _ in 0..=limits.max_redirects {
            if cancel.is_cancelled() {
                return Err(LoadError::Cancelled);
            }
            policy::check_request(&url, previous.as_ref(), self.rules)?;
            match self.get(&url, limits, cancel)? {
                Step::Body(bytes) => return Ok(bytes),
                Step::Redirect(next) => previous = Some(std::mem::replace(&mut url, next)),
            }
        }
        Err(LoadError::TooManyRedirects)
    }
}

/// Resolves with the system resolver, then keeps only addresses previews
/// may connect to. The connection uses exactly these addresses, so a name
/// cannot pass the check and then connect somewhere else.
#[derive(Debug)]
struct CheckedResolver {
    rules: Rules,
}

impl Resolver for CheckedResolver {
    fn resolve(
        &self,
        uri: &Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let resolved = DefaultResolver::default().resolve(uri, config, timeout)?;
        let mut allowed = self.empty();
        for address in resolved.iter().filter(|address| self.permits(address)) {
            allowed.push(*address);
        }
        if allowed.is_empty() {
            return Err(ureq::Error::Other(Box::new(BlockedAddress)));
        }
        Ok(allowed)
    }
}

impl CheckedResolver {
    fn permits(&self, address: &SocketAddr) -> bool {
        policy::is_public_ip(address.ip())
            || (self.rules.loopback_fixture && address.ip() == std::net::Ipv4Addr::LOCALHOST)
    }
}

#[derive(Debug)]
struct BlockedAddress;

impl std::fmt::Display for BlockedAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the host resolves to an address previews may not contact")
    }
}

impl std::error::Error for BlockedAddress {}

/// A shared fetcher, as the UI keeps it.
pub type SharedFetcher = Arc<dyn Fetcher>;

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, time::Duration};

    use image::ImageFormat;

    use super::*;
    use crate::testing::{FixtureServer, Route, encode};

    fn fixture(routes: &[(&str, Route)]) -> FixtureServer {
        FixtureServer::start(
            routes
                .iter()
                .map(|(path, route)| (path.to_string(), route.clone()))
                .collect::<HashMap<_, _>>(),
        )
    }

    fn fetch(server: &FixtureServer, path: &str, limits: &Limits) -> Result<Vec<u8>, LoadError> {
        let source = MediaRef::Link(Url::parse(&server.url(path)).unwrap());
        HttpFetcher::for_local_fixture(limits).fetch(&source, limits, &CancelFlag::default())
    }

    #[test]
    fn fetches_images_without_credentials_cookies_or_referrer() {
        let png = encode(ImageFormat::Png, 10, 10);
        let server = fixture(&[("/a.png", Route::image("image/png", png.clone()))]);
        assert_eq!(fetch(&server, "/a.png", &Limits::default()).unwrap(), png);
        let request = server.requests.lock().unwrap()[0].to_ascii_lowercase();
        assert!(request.starts_with("get /a.png http/1.1"));
        assert!(request.contains("accept: image/png"));
        assert!(request.contains("user-agent: cayenchat/"));
        for header in ["cookie", "referer", "authorization", "accept-encoding"] {
            assert!(
                !request.contains(&format!("\n{header}:")),
                "{header} sent: {request}"
            );
        }
    }

    #[test]
    fn rejects_non_image_responses_and_error_statuses() {
        let png = encode(ImageFormat::Png, 4, 4);
        let server = fixture(&[
            (
                "/page.png",
                Route::image("text/html; charset=utf-8", b"<html></html>".to_vec()),
            ),
            (
                "/vector.png",
                Route::image("image/svg+xml", b"<svg/>".to_vec()),
            ),
            (
                "/untyped.png",
                Route {
                    headers: Vec::new(),
                    ..Route::image("", png)
                },
            ),
            (
                "/busy.png",
                Route {
                    status: 503,
                    ..Route::image("text/plain", Vec::new())
                },
            ),
        ]);
        let limits = Limits::default();
        assert_eq!(
            fetch(&server, "/page.png", &limits),
            Err(LoadError::NotImage)
        );
        assert_eq!(
            fetch(&server, "/vector.png", &limits),
            Err(LoadError::NotImage)
        );
        assert_eq!(
            fetch(&server, "/untyped.png", &limits),
            Err(LoadError::NotImage)
        );
        let missing = fetch(&server, "/missing.png", &limits).unwrap_err();
        assert_eq!(missing, LoadError::Status(404));
        assert!(!missing.is_transient());
        let busy = fetch(&server, "/busy.png", &limits).unwrap_err();
        assert!(busy.is_transient());
    }

    #[test]
    fn response_size_is_bounded_with_or_without_a_declared_length() {
        let body = vec![0x89; 5000];
        let server = fixture(&[
            ("/declared.png", Route::image("image/png", body.clone())),
            (
                "/streamed.png",
                Route {
                    omit_length: true,
                    ..Route::image("image/png", body.clone())
                },
            ),
            (
                "/exact.png",
                Route {
                    omit_length: true,
                    ..Route::image("image/png", body[..1000].to_vec())
                },
            ),
        ]);
        let limits = Limits {
            max_response_bytes: 1000,
            ..Limits::default()
        };
        assert_eq!(
            fetch(&server, "/declared.png", &limits),
            Err(LoadError::TooLarge)
        );
        assert_eq!(
            fetch(&server, "/streamed.png", &limits),
            Err(LoadError::TooLarge)
        );
        assert_eq!(fetch(&server, "/exact.png", &limits).unwrap().len(), 1000);
    }

    #[test]
    fn redirects_are_limited_and_every_target_is_checked() {
        let png = encode(ImageFormat::Png, 4, 4);
        let server = fixture(&[
            ("/image.png", Route::image("image/png", png.clone())),
            ("/one", Route::redirect("/image.png")),
            ("/two", Route::redirect("/one")),
            ("/three", Route::redirect("/two")),
            ("/four", Route::redirect("/three")),
            ("/private", Route::redirect("http://10.0.0.1/image.png")),
            (
                "/metadata",
                Route::redirect("http://169.254.169.254/latest/meta-data"),
            ),
            ("/ipv6-loopback", Route::redirect("http://[::1]/image.png")),
            ("/ftp", Route::redirect("ftp://example.com/image.png")),
            (
                "/credentials",
                Route::redirect("https://user:pw@example.com/image.png"),
            ),
        ]);
        let named_loopback = format!("http://localhost:{}/image.png", server.port);
        let named = fixture(&[("/named", Route::redirect(&named_loopback))]);
        let limits = Limits::default();
        assert_eq!(fetch(&server, "/three", &limits).unwrap(), png);
        assert_eq!(
            fetch(&server, "/four", &limits),
            Err(LoadError::TooManyRedirects)
        );
        for path in [
            "/private",
            "/metadata",
            "/ipv6-loopback",
            "/ftp",
            "/credentials",
        ] {
            assert_eq!(
                fetch(&server, path, &limits),
                Err(LoadError::Blocked),
                "{path}"
            );
        }
        assert_eq!(fetch(&named, "/named", &limits), Err(LoadError::Blocked));
        assert_eq!(named.hits(), 1, "the redirect target was never requested");
    }

    #[test]
    fn production_policy_never_contacts_local_addresses() {
        let server = fixture(&[(
            "/a.png",
            Route::image("image/png", encode(ImageFormat::Png, 4, 4)),
        )]);
        let limits = Limits::default();
        let fetcher = HttpFetcher::new(&limits);
        for url in [
            server.url("/a.png"),
            format!("http://localhost:{}/a.png", server.port),
        ] {
            let source = MediaRef::Link(Url::parse(&url).unwrap());
            assert_eq!(
                fetcher.fetch(&source, &limits, &CancelFlag::default()),
                Err(LoadError::Blocked)
            );
        }
        assert_eq!(server.hits(), 0);

        // Names are checked after resolution too.
        let resolver = CheckedResolver {
            rules: Rules::default(),
        };
        let uri: Uri = "http://localhost:80/a.png".parse().unwrap();
        let config = Config::default();
        let timeout = NextTimeout {
            after: ureq::unversioned::transport::time::Duration::NotHappening,
            reason: ureq::Timeout::Global,
        };
        assert!(matches!(
            resolver.resolve(&uri, &config, timeout),
            Err(ureq::Error::Other(_))
        ));
    }

    #[test]
    fn slow_responses_time_out_and_cancelled_reads_stop() {
        let slow = Route {
            delay: Duration::from_millis(150),
            ..Route::image("image/png", vec![0; 8 * 1024])
        };
        let server = fixture(&[("/slow.png", slow)]);
        let limits = Limits {
            total_timeout: Duration::from_millis(400),
            ..Limits::default()
        };
        assert_eq!(
            fetch(&server, "/slow.png", &limits),
            Err(LoadError::Network)
        );

        let cancel = CancelFlag::default();
        let source = MediaRef::Link(Url::parse(&server.url("/slow.png")).unwrap());
        let fetcher = HttpFetcher::for_local_fixture(&Limits::default());
        let canceller = {
            let cancel = cancel.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(200));
                cancel.cancel();
            })
        };
        assert_eq!(
            fetcher.fetch(&source, &Limits::default(), &cancel),
            Err(LoadError::Cancelled)
        );
        canceller.join().unwrap();
    }
}
