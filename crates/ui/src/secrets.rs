//! The application's credential store as a GPUI global. Every window reads
//! and writes secrets through it; nothing else opens a backend.

use cayenchat_storage::{CredentialBackendKind, CredentialError, CredentialStore};
use gpui::{App, Global};

use crate::localization::Localizer;

pub struct Credentials(pub CredentialStore);

impl Global for Credentials {}

pub fn install(kind: CredentialBackendKind, cx: &mut App) {
    cx.set_global(Credentials(CredentialStore::open(kind)));
}

pub fn store(cx: &App) -> CredentialStore {
    match cx.try_global::<Credentials>() {
        Some(credentials) => credentials.0.clone(),
        None => CredentialStore::open(CredentialBackendKind::System),
    }
}

/// User-facing text for a credential failure. Errors never contain secrets.
pub fn error_text(i18n: &Localizer, error: &CredentialError) -> String {
    let detail = error.to_string();
    match error {
        CredentialError::Unavailable(_) => {
            i18n.format("credential_error_unavailable", &[("error", &detail)])
        }
        _ => i18n.format("credential_error", &[("error", &detail)]),
    }
}

/// Where the local credential file is, for explanations.
pub fn local_path_text() -> String {
    cayenchat_storage::credentials::local_credentials_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|error| error)
}

/// Localization key describing this platform's secure store.
pub fn system_store_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "credential_system_macos"
    } else if cfg!(target_os = "windows") {
        "credential_system_windows"
    } else {
        "credential_system_linux"
    }
}
