//! Which links are previewed, and which hosts may be contacted.
//!
//! A link is a preview candidate only when it looks like a *direct* image
//! link: `http` or `https`, no credentials, the default port, a public host,
//! and a last path segment ending in `.png`, `.jpg`, `.jpeg`, `.gif` or
//! `.webp` (any case; query and fragment are ignored). ImgBB links
//! (`https://i.ibb.co/<id>/<name>.png`) qualify. The extension is only a
//! hint: the response type and the content decide (see `fetch` and
//! `decode`). Web pages are never fetched to discover images.
//!
//! Every request, including each redirect target, must pass
//! [`check_request`], and the resolver only connects to addresses for which
//! [`is_public_ip`] holds, so names that resolve to loopback, private or
//! link-local addresses are refused too.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use url::{Host, Url};

use crate::{LoadError, MediaRef};

/// Extensions of the formats previews decode.
pub const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

/// Longer links are not previewed.
const MAX_URL_LEN: usize = 2048;

/// Relaxations used only by tests against a local fixture server.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Rules {
    /// Allow 127.0.0.1 on any port.
    pub(crate) loopback_fixture: bool,
}

/// Returns the media reference for a link recognized in message text, or
/// `None` when the link is not a direct image link that may be previewed.
pub fn image_link(text: &str) -> Option<MediaRef> {
    if text.len() > MAX_URL_LEN {
        return None;
    }
    let url = Url::parse(text).ok()?;
    check_request(&url, None, Rules::default()).ok()?;
    let segment = url.path_segments()?.next_back()?;
    let (_, extension) = segment.rsplit_once('.')?;
    IMAGE_EXTENSIONS
        .iter()
        .any(|known| extension.eq_ignore_ascii_case(known))
        .then_some(MediaRef::Link(url))
}

/// The size placeholder of the IRCv3 registry's `avatar` metadata key.
pub const AVATAR_SIZE_PLACEHOLDER: &str = "{size}";

/// Returns the media reference for an avatar URL a user explicitly
/// published (for IRC, the `avatar` metadata value), with `{size}` replaced
/// by `size` pixels, or `None` when it may not be fetched.
///
/// Unlike [`image_link`], no file extension is required: avatar endpoints
/// often have none, and the URL was given as an image rather than found in
/// chat text. Everything that keeps fetching safe still applies, here and
/// for every redirect ([`check_request`]), and the response must still be
/// a PNG, JPEG, GIF or WebP image.
pub fn avatar_url(template: &str, size: u32) -> Option<MediaRef> {
    if template.len() > MAX_URL_LEN {
        return None;
    }
    let text = template.replace(AVATAR_SIZE_PLACEHOLDER, &size.to_string());
    if text.len() > MAX_URL_LEN {
        return None;
    }
    let url = Url::parse(&text).ok()?;
    check_request(&url, None, Rules::default()).ok()?;
    Some(MediaRef::Link(url))
}

/// Why a URL may not be published as our own avatar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishProblem {
    /// Empty, too long, not a URL, or containing spaces or controls.
    Invalid,
    /// A user name, password or token-like query parameter: publishing it
    /// would hand a credential to everyone on the network.
    Credentials,
    /// Not something avatars may be fetched from (scheme, port, a private
    /// or local host); other users could not load it either.
    Blocked,
}

/// Query or fragment parameters that usually carry a credential. Compared
/// case-insensitively, with `-` treated as `_`.
const SECRET_PARAMETERS: [&str; 17] = [
    "access_token",
    "api_key",
    "apikey",
    "auth",
    "code",
    "jwt",
    "key",
    "pass",
    "passwd",
    "password",
    "secret",
    "session",
    "sid",
    "sig",
    "signature",
    "token",
    "x_amz_credential",
];

/// Checks a URL the user wants to publish as their avatar (for IRC, the
/// value of the `avatar` metadata key, which may contain `{size}`). The
/// rules are those of [`avatar_url`], so what we publish is what other
/// CayenChat users would load, plus a refusal of anything that looks like
/// a credential. Nothing is fetched.
pub fn publishable_avatar_url(template: &str) -> Result<(), PublishProblem> {
    if template.is_empty()
        || template.len() > MAX_URL_LEN
        || template
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(PublishProblem::Invalid);
    }
    let parsed = Url::parse(&template.replace(AVATAR_SIZE_PLACEHOLDER, "32"))
        .map_err(|_| PublishProblem::Invalid)?;
    let secret = |name: &str| {
        let name = name.to_ascii_lowercase().replace('-', "_");
        SECRET_PARAMETERS.contains(&name.as_str())
    };
    let fragment_pairs = parsed
        .fragment()
        .map(|fragment| url::form_urlencoded::parse(fragment.as_bytes()))
        .into_iter()
        .flatten();
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed
            .query_pairs()
            .chain(fragment_pairs)
            .any(|(name, _)| secret(&name))
    {
        return Err(PublishProblem::Credentials);
    }
    avatar_url(template, 32)
        .map(|_| ())
        .ok_or(PublishProblem::Blocked)
}

/// Checks a URL before it is requested. `previous` is the URL that
/// redirected here; a redirect may not go from HTTPS to plain HTTP.
pub(crate) fn check_request(
    url: &Url,
    previous: Option<&Url>,
    rules: Rules,
) -> Result<(), LoadError> {
    let secure_before = previous.is_some_and(|url| url.scheme() == "https");
    let allowed_scheme = match url.scheme() {
        "https" => true,
        "http" => !secure_before,
        _ => false,
    };
    if !allowed_scheme || !url.username().is_empty() || url.password().is_some() {
        return Err(LoadError::Blocked);
    }
    let fixture = |ip: IpAddr| rules.loopback_fixture && ip == IpAddr::V4(Ipv4Addr::LOCALHOST);
    // `Url` drops the port when it is the scheme's default.
    let host_ok = match url.host() {
        Some(Host::Domain(domain)) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            url.port().is_none()
                && domain.contains('.')
                && domain != "localhost"
                && !domain.ends_with(".localhost")
        }
        Some(Host::Ipv4(ip)) => {
            fixture(ip.into()) || (url.port().is_none() && is_public_ip(ip.into()))
        }
        Some(Host::Ipv6(ip)) => url.port().is_none() && is_public_ip(ip.into()),
        None => false,
    };
    if host_ok {
        Ok(())
    } else {
        Err(LoadError::Blocked)
    }
}

/// Whether previews may connect to `ip`: a globally routable unicast
/// address. Loopback, private, shared (CGNAT), link-local, multicast,
/// documentation, benchmarking and reserved ranges are refused, including
/// IPv4 addresses embedded in IPv6 (mapped, NAT64, 6to4).
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_v4(ip),
        IpAddr::V6(ip) => is_public_v6(ip),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..128).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..32).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 88 && c == 99)
        || (a == 192 && b == 168)
        || (a == 198 && (18..20).contains(&b))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    let embedded_v4 = |high: u16, low: u16| {
        let [a, b] = high.to_be_bytes();
        let [c, d] = low.to_be_bytes();
        Ipv4Addr::new(a, b, c, d)
    };
    // IPv4-mapped (::ffff:0:0/96) and NAT64 (64:ff9b::/96) reach IPv4 hosts.
    if segments[..5] == [0, 0, 0, 0, 0] && segments[5] == 0xffff
        || segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0]
    {
        return is_public_v4(embedded_v4(segments[6], segments[7]));
    }
    // 6to4 (2002::/16) embeds the IPv4 address in the next 32 bits.
    if segments[0] == 0x2002 {
        return is_public_v4(embedded_v4(segments[1], segments[2]));
    }
    // Only global unicast (2000::/3), without Teredo (2001::/32), ORCHID and
    // documentation (2001:db8::/32) ranges.
    (segments[0] & 0xe000) == 0x2000
        && segments[..2] != [0x2001, 0]
        && !(segments[0] == 0x2001 && (0x10..0x40).contains(&segments[1]))
        && segments[..2] != [0x2001, 0xdb8]
        && segments[0] != 0x3fff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_direct_image_links_including_imgbb() {
        for link in [
            "https://i.ibb.co/Zx8Yb3k/screenshot.png",
            "https://example.com/a/b/photo.JPG",
            "http://example.com/x.jpeg?size=large#frag",
            "https://cdn.example.org/anim.gif",
            "https://cdn.example.org/pic.webp",
            "https://example.com:443/pic.png",
            "http://93.184.216.34/pic.png",
        ] {
            assert!(image_link(link).is_some(), "{link}");
        }
    }

    #[test]
    fn rejects_non_image_links_and_unsafe_targets() {
        for link in [
            // Not a direct image link: pages are never fetched.
            "https://ibb.co/Zx8Yb3k",
            "https://example.com/gallery",
            "https://example.com/image.png/",
            "https://example.com/pic.svg",
            "https://example.com/pic.bmp",
            "https://example.com/pic.png.html",
            "https://example.com/?file=pic.png",
            // Other schemes and credentials.
            "ftp://example.com/pic.png",
            "file:///tmp/pic.png",
            "data:image/png;base64,AAAA",
            "https://user:secret@example.com/pic.png",
            "https://user@example.com/pic.png",
            // Non-default ports and local or private hosts.
            "https://example.com:8443/pic.png",
            "http://localhost/pic.png",
            "http://LOCALHOST./pic.png",
            "http://printer.localhost/pic.png",
            "http://intranet/pic.png",
            "http://127.0.0.1/pic.png",
            "http://10.1.2.3/pic.png",
            "http://172.16.0.1/pic.png",
            "http://192.168.1.1/pic.png",
            "http://169.254.169.254/latest/pic.png",
            "http://100.64.0.1/pic.png",
            "http://0.0.0.0/pic.png",
            "http://[::1]/pic.png",
            "http://[fe80::1]/pic.png",
            "http://[fd00::1]/pic.png",
            "http://[::ffff:127.0.0.1]/pic.png",
            "http://2130706433/pic.png",
        ] {
            assert!(image_link(link).is_none(), "{link}");
        }
        assert!(image_link(&format!("https://example.com/{}.png", "a".repeat(2100))).is_none());
    }

    #[test]
    fn classifies_addresses() {
        for public in [
            "93.184.216.34",
            "8.8.8.8",
            "2606:4700::1111",
            "2a00:1450::1",
        ] {
            assert!(is_public_ip(public.parse().unwrap()), "{public}");
        }
        for private in [
            "127.0.0.1",
            "10.0.0.1",
            "172.31.255.255",
            "192.168.0.1",
            "169.254.1.1",
            "100.100.100.100",
            "192.0.2.1",
            "198.18.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "0.1.2.3",
            "::",
            "::1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "2001::1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
            "2002:c0a8:0101::1",
            "::127.0.0.1",
        ] {
            assert!(!is_public_ip(private.parse().unwrap()), "{private}");
        }
        assert!(is_public_ip("::ffff:8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn redirects_may_not_downgrade_to_plain_http() {
        let https = Url::parse("https://example.com/a.png").unwrap();
        let http = Url::parse("http://example.org/b.png").unwrap();
        assert!(check_request(&http, None, Rules::default()).is_ok());
        assert_eq!(
            check_request(&http, Some(&https), Rules::default()),
            Err(LoadError::Blocked)
        );
        assert!(check_request(&https, Some(&http), Rules::default()).is_ok());
    }
    #[test]
    fn published_avatar_urls_are_fetchable_and_carry_no_credentials() {
        for ok in [
            "https://example.com/avatar.png",
            "https://example.com/u/42/{size}",
            "http://example.com/a?s=64&v=2",
            "https://example.com/a#crop",
        ] {
            assert_eq!(publishable_avatar_url(ok), Ok(()), "{ok}");
        }
        for (url, problem) in [
            ("", PublishProblem::Invalid),
            ("not a url", PublishProblem::Invalid),
            ("https://example.com/a\r\nQUIT", PublishProblem::Invalid),
            ("https://example.com/\u{0}", PublishProblem::Invalid),
            (
                "https://user:pw@example.com/a.png",
                PublishProblem::Credentials,
            ),
            (
                "https://user@example.com/a.png",
                PublishProblem::Credentials,
            ),
            (
                "https://example.com/a.png?token=abc",
                PublishProblem::Credentials,
            ),
            (
                "https://example.com/a.png?X-Amz-Credential=x",
                PublishProblem::Credentials,
            ),
            (
                "https://example.com/a.png?API-KEY=x",
                PublishProblem::Credentials,
            ),
            (
                "https://example.com/a.png#access_token=x",
                PublishProblem::Credentials,
            ),
            ("ftp://example.com/a.png", PublishProblem::Blocked),
            ("https://127.0.0.1/a.png", PublishProblem::Blocked),
            ("https://localhost/a.png", PublishProblem::Blocked),
            ("https://example.com:8443/a.png", PublishProblem::Blocked),
            ("https://192.168.1.2/a.png", PublishProblem::Blocked),
        ] {
            assert_eq!(publishable_avatar_url(url), Err(problem), "{url}");
        }
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_LEN));
        assert_eq!(publishable_avatar_url(&long), Err(PublishProblem::Invalid));
    }

    #[test]
    fn avatar_urls_need_no_extension_but_stay_safe() {
        let url = |text: &str| avatar_url(text, 32).map(|MediaRef::Link(url)| url.to_string());
        assert_eq!(
            url("https://example.com/avatar/{size}/abc").as_deref(),
            Some("https://example.com/avatar/32/abc")
        );
        assert_eq!(
            url("https://example.com/u/abc?s={size}&v={size}").as_deref(),
            Some("https://example.com/u/abc?s=32&v=32")
        );
        assert_eq!(
            url("https://i.ibb.co/Zx8Yb3k/me.png").as_deref(),
            Some("https://i.ibb.co/Zx8Yb3k/me.png")
        );
        for unsafe_link in [
            "http://127.0.0.1/avatar",
            "http://localhost/avatar",
            "https://user:pw@example.com/avatar",
            "https://example.com:8443/avatar",
            "file:///etc/passwd",
            "data:image/png;base64,AAAA",
            "http://[::1]/{size}",
            "not a url",
            "",
        ] {
            assert_eq!(url(unsafe_link), None, "{unsafe_link}");
        }
        assert_eq!(
            url(&format!("https://example.com/{}", "a".repeat(2100))),
            None
        );
        // Chat links keep their stricter recognition.
        assert!(image_link("https://example.com/avatar/32/abc").is_none());
    }
}
