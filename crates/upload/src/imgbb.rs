//! ImgBB upload API v1 (<https://api.imgbb.com/>).
//!
//! `POST https://api.imgbb.com/1/upload` with `key` (the account's API key)
//! and `image` (a file up to 32 MB). An optional `expiration` (60–15552000 s)
//! deletes the upload later; CayenChat does not set it. A success reply is
//! `{"data": {"url": ..., "url_viewer": ..., "delete_url": ...}, "success":
//! true, "status": 200}`; `url` is the direct image link. An invalid or
//! missing key is answered with HTTP 400 and `{"error": {"message": "Invalid
//! API v1 key.", "code": 100}}`.
//!
//! The key travels in the multipart body rather than the URL the docs'
//! example uses, so it cannot appear in logged URLs.

use cayenchat_model::attachment::Attachment;
use cayenchat_storage::Secret;

use crate::{
    ExternalUploader, ProviderInfo, UploadError, UploadedImage, checked_url,
    http::{self, display_message},
};

pub const ID: &str = "imgbb";
pub const INFO: ProviderInfo = ProviderInfo {
    id: ID,
    name: "ImgBB",
    setup_url: "https://api.imgbb.com/",
    // The API documents "up to 32 MB"; imgbb.com's configuration states
    // 32,000,000 bytes.
    max_bytes: 32_000_000,
};
const ENDPOINT: &str = "https://api.imgbb.com/1/upload";
/// ImgBB's error code for an invalid API key.
const INVALID_KEY: i64 = 100;

pub struct ImgBb {
    key: Secret,
    agent: ureq::Agent,
}

impl ImgBb {
    pub fn new(key: Secret) -> Self {
        Self {
            key,
            agent: http::agent(),
        }
    }
}

impl ExternalUploader for ImgBb {
    fn provider(&self) -> &'static ProviderInfo {
        &INFO
    }

    fn upload(&self, attachment: &Attachment) -> Result<UploadedImage, UploadError> {
        let form = http::multipart(&[("key", self.key.expose())], "image", attachment);
        let (status, body) = http::post_form(&self.agent, ENDPOINT, form)?;
        interpret(status, &body)
    }
}

fn interpret(status: u16, body: &str) -> Result<UploadedImage, UploadError> {
    let json: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let error = json.as_ref().and_then(|value| value.get("error"));
    let code = error
        .and_then(|error| error.get("code"))
        .and_then(serde_json::Value::as_i64);
    let message = error
        .and_then(|error| error.get("message"))
        .and_then(serde_json::Value::as_str)
        .map(display_message)
        .filter(|message| !message.is_empty());
    if code == Some(INVALID_KEY) || status == 401 || status == 403 {
        return Err(UploadError::Authentication);
    }
    let rejected = |fallback: String| {
        UploadError::Rejected(match &message {
            Some(message) => format!("{fallback}: {message}"),
            None => fallback,
        })
    };
    match status {
        200..=299 => json
            .as_ref()
            .filter(|value| value.get("success") == Some(&serde_json::Value::Bool(true)))
            .and_then(|value| value.pointer("/data/url"))
            .and_then(serde_json::Value::as_str)
            .and_then(checked_url)
            .map(|url| UploadedImage { url })
            .ok_or_else(|| UploadError::Rejected("the reply did not contain an image link".into())),
        429 => Err(rejected("too many uploads; try again later".into())),
        300..=399 => Err(UploadError::Rejected(format!(
            "unexpected redirect (HTTP {status})"
        ))),
        400..=499 => Err(rejected(format!("HTTP {status}"))),
        _ => Err(rejected(format!("server error (HTTP {status})"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_map_to_links_or_errors() {
        let ok = r#"{"data":{"id":"2ndCYJK","url_viewer":"https://ibb.co/2ndCYJK","url":"https://i.ibb.co/w04Prt6/c1f64245afb2.gif","display_url":"https://i.ibb.co/98W13PY/c1f64245afb2.gif","delete_url":"https://ibb.co/2ndCYJK/670a"},"success":true,"status":200}"#;
        assert_eq!(
            interpret(200, ok).unwrap().url,
            "https://i.ibb.co/w04Prt6/c1f64245afb2.gif"
        );
        // Observed reply to an invalid key (2026-09-26).
        let bad_key = r#"{"status_code":400,"error":{"message":"Invalid API v1 key.","code":100},"status_txt":"Bad Request"}"#;
        assert_eq!(interpret(400, bad_key), Err(UploadError::Authentication));
        assert!(matches!(
            interpret(400, r#"{"status_code":400,"error":{"message":"Invalid\r\nimage","code":310}}"#),
            Err(UploadError::Rejected(message)) if message == "HTTP 400: Invalidimage"
        ));
        assert!(matches!(
            interpret(429, ""),
            Err(UploadError::Rejected(message)) if message.contains("too many")
        ));
        assert!(matches!(interpret(302, ""), Err(UploadError::Rejected(_))));
        assert!(matches!(
            interpret(503, "<html>"),
            Err(UploadError::Rejected(_))
        ));
        assert!(
            interpret(
                200,
                r#"{"data":{"url":"http://i.ibb.co/x.png"},"success":true}"#
            )
            .is_err()
        );
        assert!(
            interpret(
                200,
                r#"{"data":{"url":"https://i.ibb.co/x.png"},"success":false}"#
            )
            .is_err()
        );
        assert!(interpret(200, "not json").is_err());
    }
}
