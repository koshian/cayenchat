//! A scripted uploader for tests; it never touches the network.

use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

use cayenchat_model::attachment::Attachment;

use crate::{ExternalUploader, ProviderInfo, UploadError, UploadedImage};

pub const FAKE_PROVIDER: ProviderInfo = ProviderInfo {
    id: "fake",
    name: "Fake Host",
    setup_url: "https://example.invalid/setup",
    max_bytes: 64,
};

/// Returns `result` for every upload and counts the calls.
pub struct FakeUploader {
    result: Mutex<Result<UploadedImage, UploadError>>,
    calls: AtomicUsize,
    names: Mutex<Vec<String>>,
}

impl FakeUploader {
    pub fn succeeding(url: &str) -> Self {
        Self::with_result(Ok(UploadedImage { url: url.into() }))
    }

    pub fn failing(error: UploadError) -> Self {
        Self::with_result(Err(error))
    }

    fn with_result(result: Result<UploadedImage, UploadError>) -> Self {
        Self {
            result: Mutex::new(result),
            calls: AtomicUsize::new(0),
            names: Mutex::new(Vec::new()),
        }
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Names of the attachments received, in order.
    pub fn uploaded_names(&self) -> Vec<String> {
        self.names.lock().unwrap().clone()
    }
}

impl ExternalUploader for FakeUploader {
    fn provider(&self) -> &'static ProviderInfo {
        &FAKE_PROVIDER
    }

    fn upload(&self, attachment: &Attachment) -> Result<UploadedImage, UploadError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.names.lock().unwrap().push(attachment.name.clone());
        self.result.lock().unwrap().clone()
    }
}
