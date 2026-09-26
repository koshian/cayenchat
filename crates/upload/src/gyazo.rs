//! Gyazo upload API.
//!
//! `POST https://upload.gyazo.com/api/upload` with multipart form data:
//! `access_token` (the user's token) and `imagedata` (the file, whose
//! Content-Disposition must carry a filename). The JSON reply has `url`, the
//! direct image link. Images default to `access_policy=anyone`: anyone with
//! the link can view them. Error statuses per the API docs: 400 invalid
//! parameter, 401 authentication required, 402 Pro required, 403 no
//! permission, 422 unprocessable, 429 rate limited, 500 internal error.
//! Tokens have no expiry; they stop working when the user deletes the app.
//!
//! The token travels in the request body, not a header or query string, so
//! it cannot appear in logged URLs or headers.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cayenchat_model::attachment::Attachment;
use cayenchat_storage::Secret;

use crate::{ExternalUploader, ProviderInfo, UploadError, UploadedImage, checked_url};

pub const ID: &str = "gyazo";
pub const INFO: ProviderInfo = ProviderInfo {
    id: ID,
    name: "Gyazo",
    setup_url: "https://gyazo.com/oauth/applications",
};
const ENDPOINT: &str = "https://upload.gyazo.com/api/upload";
/// Replies are small JSON documents; anything larger is not trusted.
const RESPONSE_LIMIT: u64 = 64 * 1024;

pub struct Gyazo {
    token: Secret,
    agent: ureq::Agent,
}

impl Gyazo {
    pub fn new(token: Secret) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_global(Some(Duration::from_secs(120)))
            .http_status_as_error(false)
            // An upload is never redirected; a 3xx is reported as a rejection.
            .max_redirects(0)
            .max_redirects_will_error(false)
            .user_agent(concat!("CayenChat/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self { token, agent }
    }
}

impl ExternalUploader for Gyazo {
    fn provider(&self) -> &'static ProviderInfo {
        &INFO
    }

    fn upload(&self, attachment: &Attachment) -> Result<UploadedImage, UploadError> {
        let (content_type, body) = multipart_body(&self.token, attachment);
        let mut response = self
            .agent
            .post(ENDPOINT)
            .header("Content-Type", &content_type)
            .header("Accept", "application/json")
            .send(&body[..])
            .map_err(|error| UploadError::Network(network_error(&error)))?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .with_config()
            .limit(RESPONSE_LIMIT)
            .read_to_string()
            .map_err(|error| UploadError::Network(network_error(&error)))?;
        interpret(status, &text)
    }
}

fn network_error(error: &ureq::Error) -> String {
    match error {
        ureq::Error::Timeout(_) => "the request timed out".into(),
        ureq::Error::HostNotFound => "the server name could not be resolved".into(),
        ureq::Error::ConnectionFailed => "the connection failed".into(),
        ureq::Error::BodyExceedsLimit(_) => "the reply was too large".into(),
        // Other errors describe the transport; the URL carries no secret.
        other => other.to_string(),
    }
}

/// Builds the multipart request. The boundary is regenerated until it does
/// not occur in the image data.
fn multipart_body(token: &Secret, attachment: &Attachment) -> (String, Vec<u8>) {
    let mut seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_nanos())
        .unwrap_or_default();
    let boundary = loop {
        let candidate = format!("----CayenChatBoundary{seed:032x}");
        if !contains(&attachment.bytes, candidate.as_bytes()) {
            break candidate;
        }
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    };
    let filename: String = attachment
        .name
        .chars()
        .map(|ch| if matches!(ch, '"' | '\\') { '_' } else { ch })
        .collect();
    let mut body = Vec::with_capacity(attachment.bytes.len() + 512);
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"access_token\"\r\n\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(token.expose().as_bytes());
    body.extend_from_slice(
        format!(
            "\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"imagedata\"; filename=\"{filename}\"\r\nContent-Type: {}\r\n\r\n",
            attachment.format.media_type()
        )
        .as_bytes(),
    );
    body.extend_from_slice(&attachment.bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Maps a reply to a result. Provider messages are shortened and stripped of
/// control characters before they reach the UI.
fn interpret(status: u16, body: &str) -> Result<UploadedImage, UploadError> {
    let json: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let message = json
        .as_ref()
        .and_then(|value| value.get("message"))
        .and_then(serde_json::Value::as_str)
        .map(|text| {
            text.chars()
                .filter(|ch| !ch.is_control())
                .take(160)
                .collect::<String>()
        });
    let rejected = |fallback: &str| {
        UploadError::Rejected(match &message {
            Some(message) if !message.is_empty() => format!("{fallback}: {message}"),
            _ => fallback.to_owned(),
        })
    };
    match status {
        200..=299 => json
            .as_ref()
            .and_then(|value| value.get("url"))
            .and_then(serde_json::Value::as_str)
            .and_then(checked_url)
            .map(|url| UploadedImage { url })
            .ok_or_else(|| UploadError::Rejected("the reply did not contain an image link".into())),
        401 => Err(UploadError::Authentication),
        402 => Err(rejected("a Gyazo Pro plan is required")),
        403 => Err(rejected("the account is not allowed to upload")),
        429 => Err(rejected("too many uploads; try again later")),
        400 | 413 | 415 | 422 => Err(rejected(&format!("HTTP {status}"))),
        300..=399 => Err(UploadError::Rejected(format!(
            "unexpected redirect (HTTP {status})"
        ))),
        _ => Err(rejected(&format!("server error (HTTP {status})"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cayenchat_model::attachment::AttachmentSource;

    fn png() -> Attachment {
        Attachment::image(
            Some("a\"b.png"),
            b"\x89PNG\r\n\x1a\nDATA".to_vec(),
            AttachmentSource::Clipboard,
        )
        .unwrap()
    }

    #[test]
    fn multipart_request_carries_token_and_named_image() {
        let (content_type, body) = multipart_body(&Secret::new("tok"), &png());
        let boundary = content_type
            .strip_prefix("multipart/form-data; boundary=")
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.starts_with(&format!("--{boundary}\r\n")));
        assert!(text.contains("name=\"access_token\"\r\n\r\ntok\r\n"));
        assert!(text.contains("name=\"imagedata\"; filename=\"a_b.png\""));
        assert!(text.contains("Content-Type: image/png\r\n\r\n\u{fffd}PNG"));
        assert!(text.ends_with(&format!("\r\n--{boundary}--\r\n")));
    }

    #[test]
    fn replies_map_to_links_or_errors() {
        let ok = r#"{"image_id":"x","permalink_url":"http://gyazo.com/x","url":"https://i.gyazo.com/x.png","type":"png"}"#;
        assert_eq!(interpret(200, ok).unwrap().url, "https://i.gyazo.com/x.png");
        assert_eq!(
            interpret(401, r#"{"message":"This method requires authentication"}"#),
            Err(UploadError::Authentication)
        );
        assert!(matches!(
            interpret(402, "{}"),
            Err(UploadError::Rejected(_))
        ));
        assert!(matches!(
            interpret(429, "Too Many Requests"),
            Err(UploadError::Rejected(message)) if message.contains("too many")
        ));
        assert!(matches!(
            interpret(422, r#"{"message":"bad\r\nimage"}"#),
            Err(UploadError::Rejected(message)) if message == "HTTP 422: badimage"
        ));
        assert!(matches!(interpret(302, ""), Err(UploadError::Rejected(_))));
        assert!(matches!(
            interpret(500, "<html>"),
            Err(UploadError::Rejected(_))
        ));
        // A success reply without a usable HTTPS link is not accepted.
        assert!(interpret(200, r#"{"url":"javascript:alert(1)"}"#).is_err());
        assert!(interpret(200, "not json").is_err());
    }
}
