//! External image hosting for IRC.
//!
//! IRC has no attachments, so an image is uploaded to a hosting account the
//! user owns and the resulting link goes into the draft as ordinary text.
//! This crate is that path only: it does not know about IRC wire commands,
//! and protocols with native media uploads (such as a future Matrix client)
//! must not route through it.
//!
//! Providers implement [`ExternalUploader`]; the UI selects them by the
//! string IDs in [`providers`] and never sees provider-specific types. To add
//! a provider, add a module using the `http` helpers, list its [`ProviderInfo`]
//! in [`providers`], construct it in [`connect`], and add its setup text
//! (`image_setup_<id>`) to the locale catalogs.

use std::{fmt, sync::Arc};

use cayenchat_model::attachment::Attachment;
use cayenchat_storage::Secret;

mod http;
mod imgbb;
pub mod testing;

/// Static description of a hosting provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderInfo {
    /// Stable ID stored in settings and in credential keys.
    pub id: &'static str,
    /// Name shown to the user.
    pub name: &'static str,
    /// Page where the user creates the credential CayenChat needs.
    pub setup_url: &'static str,
    /// Largest upload the provider accepts, in bytes.
    pub max_bytes: usize,
}

/// Providers available for selection.
pub fn providers() -> &'static [ProviderInfo] {
    &[imgbb::INFO]
}

pub fn provider(id: &str) -> Option<&'static ProviderInfo> {
    providers().iter().find(|provider| provider.id == id)
}

/// An uploader for `provider` authenticated with `credential`.
pub fn connect(provider: &str, credential: Secret) -> Option<Arc<dyn ExternalUploader>> {
    match provider {
        imgbb::ID => Some(Arc::new(imgbb::ImgBb::new(credential))),
        _ => None,
    }
}

/// Where the uploaded image can be fetched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadedImage {
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadError {
    /// The provider did not accept the credential (expired, revoked, wrong).
    Authentication,
    /// The provider refused the upload; the text is safe to show.
    Rejected(String),
    /// The request did not complete.
    Network(String),
}

impl fmt::Display for UploadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authentication => f.write_str("the account credential was not accepted"),
            Self::Rejected(reason) => f.write_str(reason),
            Self::Network(reason) => f.write_str(reason),
        }
    }
}

impl std::error::Error for UploadError {}

/// Uploads an image to an external host. Calls block until the provider
/// answers, so callers run them off the UI thread.
pub trait ExternalUploader: Send + Sync {
    fn provider(&self) -> &'static ProviderInfo;
    fn upload(&self, attachment: &Attachment) -> Result<UploadedImage, UploadError>;
}

/// Accepts only an `https` URL without whitespace or control characters, as
/// the link is inserted into IRC text.
pub(crate) fn checked_url(url: &str) -> Option<String> {
    let valid = url.starts_with("https://")
        && url.len() > "https://".len()
        && url.len() <= 2048
        && !url.chars().any(|ch| ch.is_whitespace() || ch.is_control());
    valid.then(|| url.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_resolves_known_providers_only() {
        assert_eq!(provider("imgbb").unwrap().name, "ImgBB");
        assert!(provider("gyazo").is_none());
        assert!(connect("imgbb", Secret::new("key")).is_some());
        assert!(connect("unknown", Secret::new("token")).is_none());
    }

    #[test]
    fn inserted_links_must_be_plain_https() {
        assert_eq!(
            checked_url("https://i.ibb.co/a.png").as_deref(),
            Some("https://i.ibb.co/a.png")
        );
        for bad in [
            "http://i.ibb.co/a.png",
            "file:///etc/passwd",
            "https://",
            "https://a b",
            "https://a\r\nPRIVMSG",
        ] {
            assert!(checked_url(bad).is_none(), "{bad}");
        }
    }
}
