//! The application's credential store as a GPUI global. Every window reads
//! and writes secrets through it; nothing else opens a backend.
//!
//! Startup installs the chosen backend before any window opens, and tests
//! install an in-memory store. Nothing falls back to the system store when
//! the global is missing: on macOS the Keychain asks for the login password
//! the moment a test binary reads the user's "CayenChat" item.

use std::sync::Arc;

use cayenchat_storage::credentials::{MemoryBackend, SystemBackend};
use cayenchat_storage::{CredentialBackendKind, CredentialError, CredentialStore};
use gpui::{App, Global};

use crate::localization::Localizer;

pub struct Credentials(pub CredentialStore);

impl Global for Credentials {}

pub fn install(kind: CredentialBackendKind, cx: &mut App) {
    cx.set_global(Credentials(CredentialStore::open(kind)));
}

/// Installs an empty in-memory store and returns it. Tests call this before
/// anything reads a secret.
#[cfg(test)]
pub fn install_memory(cx: &mut App) -> CredentialStore {
    let store = memory_store(CredentialBackendKind::System);
    cx.set_global(Credentials(store.clone()));
    store
}

/// The installed store. A missing global is a bug, never a reason to open
/// the system store.
pub fn store(cx: &App) -> CredentialStore {
    match cx.try_global::<Credentials>() {
        Some(credentials) => credentials.0.clone(),
        None => panic!("no credential store is installed"),
    }
}

/// Opens `kind` to switch to it. While the installed store is in memory
/// (tests), the new one is in memory too, so a switch reaches neither the
/// operating system nor the disk.
pub fn open(kind: CredentialBackendKind, cx: &App) -> CredentialStore {
    if store(cx).is_memory() {
        memory_store(kind)
    } else {
        CredentialStore::open(kind)
    }
}

/// A check of whether the operating system's store works, to run on any
/// thread. While the installed store is in memory it always succeeds without
/// asking the operating system.
pub fn system_probe(cx: &App) -> impl FnOnce() -> Result<(), CredentialError> + Send + 'static {
    let in_memory = store(cx).is_memory();
    move || {
        if in_memory {
            Ok(())
        } else {
            SystemBackend::probe()
        }
    }
}

fn memory_store(kind: CredentialBackendKind) -> CredentialStore {
    CredentialStore::with_backend(Arc::new(MemoryBackend::new(kind)))
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
    #[should_panic(expected = "no credential store is installed")]
    fn without_an_installed_store_nothing_is_opened(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            store(cx);
        });
    }

    #[gpui::test]
    fn an_in_memory_store_keeps_switches_and_probes_in_memory(cx: &mut gpui::TestAppContext) {
        let key = SecretKey::server_password("secrets-test-never-the-real-store");
        cx.update(|cx| {
            let installed = install_memory(cx);
            // Checked before anything is stored or even read: a real store
            // fails here without being touched.
            assert!(installed.is_memory(), "tests must not use a real store");
            installed.set(&key, &Secret::new("hunter2")).unwrap();
            // Every handle reaches the same store.
            assert!(store(cx).get(&key).unwrap().is_some());

            for kind in [
                CredentialBackendKind::System,
                CredentialBackendKind::LocalFile,
            ] {
                let switched = open(kind, cx);
                assert!(switched.is_memory());
                assert_eq!(switched.kind(), kind);
                assert_eq!(switched.get(&key).unwrap(), None);
            }
            assert_eq!(system_probe(cx)(), Ok(()));
        });
    }
}
