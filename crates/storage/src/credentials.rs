//! Application-wide credential storage.
//!
//! Every secret CayenChat keeps (IRC server and SASL passwords, image uploader
//! tokens) goes through [`CredentialStore`]. The store has two backends:
//!
//! - [`CredentialBackendKind::System`]: the operating system's credential store
//!   (macOS Keychain, Windows Credential Manager, or a freedesktop Secret Service
//!   such as GNOME Keyring or KWallet on Linux and other Unix desktops).
//! - [`CredentialBackendKind::LocalFile`]: an unencrypted JSON file in the user
//!   configuration directory, readable only by the user (`0600` on Unix). It
//!   exists for desktops without a Secret Service and is never chosen silently.
//!
//! Secrets are addressed by [`SecretKey`], built from stable internal IDs, and
//! carried as [`Secret`], whose `Debug` output is redacted.

use std::{
    collections::BTreeMap,
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Deserializer, Serialize};

/// Service name under which system-store entries are filed.
pub const SERVICE: &str = "CayenChat";
const LOCAL_FILE_VERSION: u32 = 1;
/// Account label for uploader tokens; one account per provider for now.
pub const DEFAULT_UPLOADER_ACCOUNT: &str = "default";

/// A credential value. It never appears in `Debug` output and is overwritten
/// when dropped (best effort: copies made by the OS or libraries are not).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The plaintext value, for handing to a protocol or HTTP client.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // SAFETY: zero bytes are valid UTF-8, so the string stays valid.
        unsafe { self.0.as_bytes_mut() }.fill(0);
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self)
    }
}

/// Stable logical name of a secret. Built from internal profile/provider IDs,
/// never from nicknames, hostnames or display labels.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SecretKey {
    /// IRC `PASS` for a server profile.
    ServerPassword { profile: String },
    /// SASL PLAIN password for a server profile.
    SaslPassword { profile: String },
    /// Token authenticating an external image uploader account.
    UploaderToken { provider: String, account: String },
}

impl SecretKey {
    pub fn server_password(profile: &str) -> Self {
        Self::ServerPassword {
            profile: profile.to_owned(),
        }
    }

    pub fn sasl_password(profile: &str) -> Self {
        Self::SaslPassword {
            profile: profile.to_owned(),
        }
    }

    pub fn uploader_token(provider: &str) -> Self {
        Self::UploaderToken {
            provider: provider.to_owned(),
            account: DEFAULT_UPLOADER_ACCOUNT.to_owned(),
        }
    }

    /// The name used in every backend, e.g. `connection/ircnet/sasl-password`.
    pub fn name(&self) -> String {
        match self {
            Self::ServerPassword { profile } => format!("connection/{profile}/server-password"),
            Self::SaslPassword { profile } => format!("connection/{profile}/sasl-password"),
            Self::UploaderToken { provider, account } => {
                format!("uploader/{provider}/{account}/credential")
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialBackendKind {
    /// Keychain, Credential Manager or Secret Service.
    #[default]
    System,
    /// Plain file in the user configuration directory.
    LocalFile,
}

/// Errors carry only sanitized descriptions, never secret values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialError {
    /// The backend cannot be used on this system or session.
    Unavailable(String),
    /// The backend exists but refused access (for example, it is locked).
    Access(String),
    /// Reading or writing the local credential file failed.
    Io(String),
    /// Stored data is not in the expected format.
    Format(String),
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(detail) => write!(f, "Credential storage is unavailable: {detail}"),
            Self::Access(detail) => write!(f, "Credential storage refused access: {detail}"),
            Self::Io(detail) => write!(f, "Could not access the credential file: {detail}"),
            Self::Format(detail) => write!(f, "Stored credentials are unreadable: {detail}"),
        }
    }
}

impl std::error::Error for CredentialError {}

/// A place secrets are kept. Implementations must not log secret values.
pub trait CredentialBackend: Send + Sync {
    fn kind(&self) -> CredentialBackendKind;
    fn get(&self, key: &SecretKey) -> Result<Option<Secret>, CredentialError>;
    fn set(&self, key: &SecretKey, value: &Secret) -> Result<(), CredentialError>;
    /// Removes the secret; a missing secret is not an error.
    fn delete(&self, key: &SecretKey) -> Result<(), CredentialError>;
}

/// The application's credential store.
#[derive(Clone)]
pub struct CredentialStore {
    backend: Arc<dyn CredentialBackend>,
}

impl fmt::Debug for CredentialStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialStore")
            .field("kind", &self.kind())
            .finish()
    }
}

impl CredentialStore {
    /// Opens the chosen backend. Opening never falls back to another backend.
    pub fn open(kind: CredentialBackendKind) -> Self {
        match kind {
            CredentialBackendKind::System => Self::with_backend(Arc::new(SystemBackend)),
            CredentialBackendKind::LocalFile => Self::with_backend(Arc::new(
                LocalFileBackend::new(local_credentials_path().unwrap_or_default()),
            )),
        }
    }

    pub fn with_backend(backend: Arc<dyn CredentialBackend>) -> Self {
        Self { backend }
    }

    pub fn kind(&self) -> CredentialBackendKind {
        self.backend.kind()
    }

    pub fn get(&self, key: &SecretKey) -> Result<Option<Secret>, CredentialError> {
        self.backend.get(key)
    }

    pub fn contains(&self, key: &SecretKey) -> Result<bool, CredentialError> {
        Ok(self.get(key)?.is_some())
    }

    /// Stores `value`, or deletes the secret when `value` is empty.
    pub fn set(&self, key: &SecretKey, value: &Secret) -> Result<(), CredentialError> {
        if value.is_empty() {
            self.backend.delete(key)
        } else {
            self.backend.set(key, value)
        }
    }

    pub fn delete(&self, key: &SecretKey) -> Result<(), CredentialError> {
        self.backend.delete(key)
    }
}

/// Result of moving secrets between backends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Migration {
    pub moved: usize,
    /// The old backend could not be read, so nothing was moved from it.
    pub source_unavailable: bool,
}

/// Copies `keys` from `from` to `to`, then deletes them from `from`. Nothing is
/// deleted unless every copy succeeded. An unusable source is reported rather
/// than blocking a switch away from it.
pub fn migrate(
    from: &CredentialStore,
    to: &CredentialStore,
    keys: &[SecretKey],
) -> Result<Migration, CredentialError> {
    let mut found = Vec::new();
    for key in keys {
        match from.get(key) {
            Ok(Some(value)) => found.push((key, value)),
            Ok(None) => {}
            Err(CredentialError::Unavailable(_)) => {
                return Ok(Migration {
                    moved: 0,
                    source_unavailable: true,
                });
            }
            Err(error) => return Err(error),
        }
    }
    for (key, value) in &found {
        to.set(key, value)?;
    }
    for (key, _) in &found {
        // The copies exist; a leftover original is harmless compared with
        // failing the whole switch.
        let _ = from.delete(key);
    }
    Ok(Migration {
        moved: found.len(),
        source_unavailable: false,
    })
}

/// The operating system credential store through the `keyring` crate.
pub struct SystemBackend;

impl SystemBackend {
    /// Checks that the store can be reached, by looking up an entry that never
    /// exists. On Linux this fails without a Secret Service provider.
    pub fn probe() -> Result<(), CredentialError> {
        let entry = system_entry("availability-probe")?;
        match entry.get_password() {
            Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(map_keyring_error(error)),
        }
    }
}

fn system_entry(name: &str) -> Result<keyring::Entry, CredentialError> {
    keyring::Entry::new(SERVICE, name).map_err(map_keyring_error)
}

/// Keeps platform detail but drops any payload, which may hold secret bytes.
fn map_keyring_error(error: keyring::Error) -> CredentialError {
    use keyring::Error as E;
    match error {
        E::NoDefaultStore => {
            CredentialError::Unavailable("no system credential store was found".into())
        }
        E::NotSupportedByStore(detail) => CredentialError::Unavailable(detail),
        E::PlatformFailure(detail) => CredentialError::Unavailable(detail.to_string()),
        E::NoStorageAccess(detail) => CredentialError::Access(detail.to_string()),
        E::BadEncoding(_) | E::BadDataFormat(..) => {
            CredentialError::Format("the stored value is not valid text".into())
        }
        E::NoEntry => CredentialError::Format("missing entry".into()),
        E::BadStoreFormat(detail) => CredentialError::Format(detail),
        E::TooLong(name, limit) => {
            CredentialError::Format(format!("{name} exceeds the platform limit of {limit}"))
        }
        E::Invalid(name, reason) => CredentialError::Format(format!("{name}: {reason}")),
        E::Ambiguous(_) => CredentialError::Format("several matching entries exist".into()),
        _ => CredentialError::Unavailable("unexpected credential store error".into()),
    }
}

impl CredentialBackend for SystemBackend {
    fn kind(&self) -> CredentialBackendKind {
        CredentialBackendKind::System
    }

    fn get(&self, key: &SecretKey) -> Result<Option<Secret>, CredentialError> {
        match system_entry(&key.name())?.get_password() {
            Ok(value) => Ok(Some(Secret::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(map_keyring_error(error)),
        }
    }

    fn set(&self, key: &SecretKey, value: &Secret) -> Result<(), CredentialError> {
        system_entry(&key.name())?
            .set_password(value.expose())
            .map_err(map_keyring_error)
    }

    fn delete(&self, key: &SecretKey) -> Result<(), CredentialError> {
        match system_entry(&key.name())?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(map_keyring_error(error)),
        }
    }
}

/// Where the local credential file lives: `$XDG_CONFIG_HOME/cayenchat` (or
/// `~/.config/cayenchat`) on Linux and other Unix desktops, the CayenChat
/// configuration directory elsewhere.
pub fn local_credentials_path() -> Result<PathBuf, String> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let directory = xdg_config_home(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME").map(PathBuf::from),
        )
        .ok_or("Could not find the user configuration directory.")?;
        Ok(directory.join("cayenchat").join("credentials.json"))
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        let directory =
            dirs::config_dir().ok_or("Could not find the user configuration directory.")?;
        Ok(directory.join("CayenChat").join("credentials.json"))
    }
}

/// The XDG Base Directory rule: a relative or empty `XDG_CONFIG_HOME` is
/// ignored in favor of `$HOME/.config`.
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
fn xdg_config_home(xdg: Option<std::ffi::OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    xdg.map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home.map(|home| home.join(".config")))
}

#[derive(Serialize, Deserialize)]
struct LocalFile {
    version: u32,
    secrets: BTreeMap<String, String>,
}

/// Unencrypted secrets in a user-only file. Encrypting them with a key kept
/// beside them would add no real protection, so this does not pretend to.
pub struct LocalFileBackend {
    path: PathBuf,
    lock: Mutex<()>,
}

impl LocalFileBackend {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> Result<BTreeMap<String, String>, CredentialError> {
        if self.path.as_os_str().is_empty() {
            return Err(CredentialError::Unavailable(
                "no user configuration directory".into(),
            ));
        }
        #[cfg(unix)]
        restrict_existing_file(&self.path)?;
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
            Err(error) => return Err(CredentialError::Io(error.kind().to_string())),
        };
        let file: LocalFile = serde_json::from_slice(&bytes)
            .map_err(|_| CredentialError::Format("the credential file is not valid".into()))?;
        if file.version != LOCAL_FILE_VERSION {
            return Err(CredentialError::Format(format!(
                "unsupported credential file version {}",
                file.version
            )));
        }
        Ok(file.secrets)
    }

    fn write(&self, secrets: BTreeMap<String, String>) -> Result<(), CredentialError> {
        let io = |error: std::io::Error| CredentialError::Io(error.kind().to_string());
        let parent = self
            .path
            .parent()
            .ok_or_else(|| CredentialError::Io("the credential path has no directory".into()))?;
        create_private_dir(parent).map_err(io)?;
        let bytes = serde_json::to_vec_pretty(&LocalFile {
            version: LOCAL_FILE_VERSION,
            secrets,
        })
        .map_err(|_| CredentialError::Format("could not encode credentials".into()))?;
        // Write a fresh user-only file, then atomically replace the old one, so
        // the secrets are never in a file with broader permissions.
        let temporary = self
            .path
            .with_extension(format!("tmp{}", std::process::id()));
        let _ = fs::remove_file(&temporary);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| {
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(io)
    }
}

/// Creates missing directories as user-only (`0700`) on Unix. Existing
/// directories keep their permissions.
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// Removes group/other access from an existing credential file.
#[cfg(unix)]
fn restrict_existing_file(path: &Path) -> Result<(), CredentialError> {
    use std::os::unix::fs::PermissionsExt;
    match fs::metadata(path) {
        Ok(metadata) if metadata.permissions().mode() & 0o077 != 0 => {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .map_err(|error| CredentialError::Io(error.kind().to_string()))
        }
        _ => Ok(()),
    }
}

impl CredentialBackend for LocalFileBackend {
    fn kind(&self) -> CredentialBackendKind {
        CredentialBackendKind::LocalFile
    }

    fn get(&self, key: &SecretKey) -> Result<Option<Secret>, CredentialError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        Ok(self.read()?.remove(&key.name()).map(Secret::new))
    }

    fn set(&self, key: &SecretKey, value: &Secret) -> Result<(), CredentialError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut secrets = self.read()?;
        secrets.insert(key.name(), value.expose().to_owned());
        self.write(secrets)
    }

    fn delete(&self, key: &SecretKey) -> Result<(), CredentialError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut secrets = self.read()?;
        if secrets.remove(&key.name()).is_some() {
            self.write(secrets)?;
        }
        Ok(())
    }
}

/// In-memory backend for tests and fakes. `unavailable` makes every call fail
/// the way a missing Secret Service does.
pub struct MemoryBackend {
    kind: CredentialBackendKind,
    unavailable: bool,
    secrets: Mutex<BTreeMap<String, String>>,
}

impl MemoryBackend {
    pub fn new(kind: CredentialBackendKind) -> Self {
        Self {
            kind,
            unavailable: false,
            secrets: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn unavailable(kind: CredentialBackendKind) -> Self {
        Self {
            unavailable: true,
            ..Self::new(kind)
        }
    }

    fn check(&self) -> Result<(), CredentialError> {
        if self.unavailable {
            Err(CredentialError::Unavailable("test backend".into()))
        } else {
            Ok(())
        }
    }
}

impl CredentialBackend for MemoryBackend {
    fn kind(&self) -> CredentialBackendKind {
        self.kind
    }

    fn get(&self, key: &SecretKey) -> Result<Option<Secret>, CredentialError> {
        self.check()?;
        let secrets = self.secrets.lock().unwrap_or_else(|e| e.into_inner());
        Ok(secrets.get(&key.name()).cloned().map(Secret::new))
    }

    fn set(&self, key: &SecretKey, value: &Secret) -> Result<(), CredentialError> {
        self.check()?;
        let mut secrets = self.secrets.lock().unwrap_or_else(|e| e.into_inner());
        secrets.insert(key.name(), value.expose().to_owned());
        Ok(())
    }

    fn delete(&self, key: &SecretKey) -> Result<(), CredentialError> {
        self.check()?;
        let mut secrets = self.secrets.lock().unwrap_or_else(|e| e.into_inner());
        secrets.remove(&key.name());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(kind: CredentialBackendKind) -> CredentialStore {
        CredentialStore::with_backend(Arc::new(MemoryBackend::new(kind)))
    }

    #[test]
    fn keys_use_stable_ids() {
        assert_eq!(
            SecretKey::server_password("custom-1").name(),
            "connection/custom-1/server-password"
        );
        assert_eq!(
            SecretKey::sasl_password("ircnet").name(),
            "connection/ircnet/sasl-password"
        );
        assert_eq!(
            SecretKey::uploader_token("imgbb").name(),
            "uploader/imgbb/default/credential"
        );
    }

    #[test]
    fn secrets_are_redacted_in_debug_output() {
        let secret = Secret::new("hunter2");
        assert_eq!(format!("{secret:?}"), "Secret([redacted])");
        assert!(!format!("{:?}", Some(secret)).contains("hunter2"));
    }

    #[test]
    fn store_sets_gets_and_deletes() {
        let store = memory(CredentialBackendKind::System);
        let server = SecretKey::server_password("ircnet");
        let sasl = SecretKey::sasl_password("ircnet");
        store.set(&server, &Secret::new("pass")).unwrap();
        store.set(&sasl, &Secret::new("sasl")).unwrap();
        assert_eq!(store.get(&server).unwrap().unwrap().expose(), "pass");
        assert_eq!(store.get(&sasl).unwrap().unwrap().expose(), "sasl");
        store.delete(&server).unwrap();
        assert!(store.get(&server).unwrap().is_none());
        // An empty value deletes rather than storing an empty password.
        store.set(&sasl, &Secret::default()).unwrap();
        assert!(!store.contains(&sasl).unwrap());
        store.delete(&sasl).unwrap();
    }

    #[test]
    fn migration_moves_secrets_between_backends() {
        let system = memory(CredentialBackendKind::System);
        let local = memory(CredentialBackendKind::LocalFile);
        let keys = [
            SecretKey::server_password("ircnet"),
            SecretKey::sasl_password("ircnet"),
            SecretKey::uploader_token("imgbb"),
        ];
        system.set(&keys[0], &Secret::new("pass")).unwrap();
        system.set(&keys[2], &Secret::new("token")).unwrap();
        let report = migrate(&system, &local, &keys).unwrap();
        assert_eq!(report.moved, 2);
        assert!(!report.source_unavailable);
        assert!(system.get(&keys[0]).unwrap().is_none());
        assert_eq!(local.get(&keys[0]).unwrap().unwrap().expose(), "pass");
        assert_eq!(local.get(&keys[2]).unwrap().unwrap().expose(), "token");
        assert!(local.get(&keys[1]).unwrap().is_none());
    }

    #[test]
    fn unavailable_backend_fails_without_fallback() {
        let broken = CredentialStore::with_backend(Arc::new(MemoryBackend::unavailable(
            CredentialBackendKind::System,
        )));
        let key = SecretKey::sasl_password("ircnet");
        assert!(matches!(
            broken.set(&key, &Secret::new("x")),
            Err(CredentialError::Unavailable(_))
        ));
        let local = memory(CredentialBackendKind::LocalFile);
        let report = migrate(&broken, &local, &[key]).unwrap();
        assert!(report.source_unavailable);
        assert_eq!(report.moved, 0);
    }

    #[test]
    fn local_file_round_trip_and_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cayenchat").join("credentials.json");
        let backend = LocalFileBackend::new(path.clone());
        let store = CredentialStore::with_backend(Arc::new(backend));
        let key = SecretKey::server_password("custom-1");
        assert!(store.get(&key).unwrap().is_none());
        store.set(&key, &Secret::new("local-secret")).unwrap();
        assert_eq!(store.get(&key).unwrap().unwrap().expose(), "local-secret");
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("connection/custom-1/server-password"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            let dir_mode = fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(dir_mode & 0o077, 0);
            // A file made readable by others is restricted again before use.
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            store.get(&key).unwrap();
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        store.delete(&key).unwrap();
        assert!(store.get(&key).unwrap().is_none());
        assert!(!fs::read_to_string(&path).unwrap().contains("local-secret"));
        // No temporary files are left behind.
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn local_file_errors_do_not_contain_secrets() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("credentials.json");
        fs::write(&path, "{\"version\":1,\"secrets\":{\"a\":\"leak-me\"").unwrap();
        let store = CredentialStore::with_backend(Arc::new(LocalFileBackend::new(path)));
        let error = store.get(&SecretKey::sasl_password("a")).unwrap_err();
        assert!(!error.to_string().contains("leak-me"));
        assert!(!format!("{error:?}").contains("leak-me"));
    }

    /// Touches the real OS store, so it only runs on request:
    /// `cargo test -p cayenchat-storage -- --ignored`. Without a Secret
    /// Service (for example in a container without D-Bus) the probe must
    /// report the store as unavailable instead of failing another way.
    #[test]
    #[ignore]
    fn system_store_probe_reports_availability() {
        match SystemBackend::probe() {
            Ok(()) => {}
            Err(error) => {
                assert!(
                    matches!(
                        error,
                        CredentialError::Unavailable(_) | CredentialError::Access(_)
                    ),
                    "{error:?}"
                );
                eprintln!("system store unavailable: {error}");
            }
        }
    }

    #[test]
    fn xdg_config_home_is_respected() {
        assert_eq!(
            xdg_config_home(Some("/xdg".into()), Some("/home/u".into())),
            Some(PathBuf::from("/xdg"))
        );
        assert_eq!(
            xdg_config_home(Some("relative".into()), Some("/home/u".into())),
            Some(PathBuf::from("/home/u/.config"))
        );
        assert_eq!(
            xdg_config_home(None, Some("/home/u".into())),
            Some(PathBuf::from("/home/u/.config"))
        );
        assert_eq!(xdg_config_home(None, None), None);
    }
}
