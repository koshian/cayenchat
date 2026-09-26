//! HTTP pieces shared by providers: an agent with upload-appropriate limits,
//! multipart encoding, and sanitized transport errors.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cayenchat_model::attachment::Attachment;

use crate::UploadError;

/// Replies are small JSON documents; anything larger is not trusted.
pub(crate) const RESPONSE_LIMIT: u64 = 64 * 1024;

/// An agent that never follows redirects (a 3xx is reported to the provider
/// code) and treats HTTP error statuses as ordinary responses.
pub(crate) fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(120)))
        .http_status_as_error(false)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .user_agent(concat!("CayenChat/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Posts a multipart form and returns the status and the (bounded) body.
pub(crate) fn post_form(
    agent: &ureq::Agent,
    url: &str,
    (content_type, body): (String, Vec<u8>),
) -> Result<(u16, String), UploadError> {
    let mut response = agent
        .post(url)
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
    Ok((status, text))
}

fn network_error(error: &ureq::Error) -> String {
    match error {
        ureq::Error::Timeout(_) => "the request timed out".into(),
        ureq::Error::HostNotFound => "the server name could not be resolved".into(),
        ureq::Error::ConnectionFailed => "the connection failed".into(),
        ureq::Error::BodyExceedsLimit(_) => "the reply was too large".into(),
        // Other errors describe the transport. Providers keep credentials in
        // the request body, so URLs carry no secret.
        other => other.to_string(),
    }
}

/// Encodes text `fields` followed by the attachment as file field `file`.
/// Returns the Content-Type header and the body. The boundary is chosen so it
/// does not occur in the image data.
pub(crate) fn multipart(
    fields: &[(&str, &str)],
    file: &str,
    attachment: &Attachment,
) -> (String, Vec<u8>) {
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
    let quoted = |value: &str| -> String {
        value
            .chars()
            .map(|ch| {
                if matches!(ch, '"' | '\\') || ch.is_control() {
                    '_'
                } else {
                    ch
                }
            })
            .collect()
    };
    let mut body = Vec::with_capacity(attachment.bytes.len() + 512);
    for (name, value) in fields {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n",
                quoted(name)
            )
            .as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n",
            quoted(file),
            quoted(&attachment.name),
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

/// A provider message made safe to show: no control characters, bounded.
pub(crate) fn display_message(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control())
        .take(160)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cayenchat_model::attachment::AttachmentSource;

    #[test]
    fn multipart_carries_fields_and_named_file() {
        let attachment = Attachment::image(
            Some("a\"b.png"),
            b"\x89PNG\r\n\x1a\nDATA".to_vec(),
            AttachmentSource::Clipboard,
        )
        .unwrap();
        let (content_type, body) = multipart(&[("key", "k1")], "image", &attachment);
        let boundary = content_type
            .strip_prefix("multipart/form-data; boundary=")
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.starts_with(&format!("--{boundary}\r\n")));
        assert!(text.contains("name=\"key\"\r\n\r\nk1\r\n"));
        assert!(text.contains("name=\"image\"; filename=\"a_b.png\""));
        assert!(text.contains("Content-Type: image/png\r\n\r\n\u{fffd}PNG"));
        assert!(text.ends_with(&format!("\r\n--{boundary}--\r\n")));
        assert_eq!(display_message("a\r\nb"), "ab");
    }
}
