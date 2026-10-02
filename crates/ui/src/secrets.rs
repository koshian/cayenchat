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
        #[cfg(not(test))]
        None => CredentialStore::open(CredentialBackendKind::System),
        // Unit tests never open the real store: on macOS the Keychain asks
        // for the login password the moment a test binary reads the user's
        // "CayenChat" item. Each test (its own thread) gets an empty
        // in-memory store instead.
        #[cfg(test)]
        None => test_store(),
    }
}

#[cfg(test)]
fn test_store() -> CredentialStore {
    use cayenchat_storage::credentials::MemoryBackend;
    use std::sync::Arc;

    thread_local! {
        static STORE: CredentialStore = CredentialStore::with_backend(Arc::new(
            MemoryBackend::new(CredentialBackendKind::System),
        ));
    }
    STORE.with(Clone::clone)
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

#[cfg(test)]
mod tests {
    use super::*;
    use cayenchat_storage::{Secret, SecretKey};

    #[gpui::test]
    fn without_an_installed_store_tests_get_a_private_in_memory_one(cx: &mut gpui::TestAppContext) {
        let key = SecretKey::server_password("secrets-test-never-the-real-store");
        cx.update(|cx| {
            assert!(cx.try_global::<Credentials>().is_none());
            let first = store(cx);
            // Checked before anything is stored or even read: if the fallback
            // ever opened the real store, this fails without touching it
            // (a test that wrote its key would put it in the user's Keychain).
            assert!(first.is_memory(), "tests must not use a real store");
            assert_eq!(first.get(&key).unwrap(), None);
            first.set(&key, &Secret::new("hunter2")).unwrap();
            // The same test sees what it stored, through any handle.
            let second = store(cx);
            assert!(second.get(&key).unwrap().is_some());
            second.delete(&key).unwrap();
            assert_eq!(first.get(&key).unwrap(), None);
        });
    }
}
