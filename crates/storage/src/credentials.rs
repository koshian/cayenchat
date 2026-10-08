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
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use serde::{Deserialize, Deserializer, Serialize};

/// Service name under which system-store entries are filed. Test builds use
/// their own, so they never read or overwrite a user's saved passwords.
#[cfg(not(feature = "test-build"))]
pub const SERVICE: &str = "CayenChat";
#[cfg(feature = "test-build")]
pub const SERVICE: &str = "CayenChat Test Build";
const SECRETS_FILE_VERSION: u32 = 1;
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
    /// Whether secrets live only in this process's memory (tests), so that
    /// nothing reaches the operating system or the disk.
    fn is_memory(&self) -> bool {
        false
    }
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
            CredentialBackendKind::System => Self::with_backend(SystemBackend::shared()),
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

    /// Whether this store keeps secrets in memory only. Tests check it before
    /// storing anything, so a test cannot reach a real store unnoticed.
    pub fn is_memory(&self) -> bool {
        self.backend.is_memory()
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

/// Raw named entries in the operating system store, below the layouts that
/// [`SystemBackend::shared`] picks, so those layouts can be tested with a fake.
pub trait EntryStore: Send + Sync {
    fn read(&self, name: &str) -> Result<Option<String>, CredentialError>;
    fn write(&self, name: &str, value: &str) -> Result<(), CredentialError>;
    /// Removes the entry; a missing entry is not an error.
    fn remove(&self, name: &str) -> Result<(), CredentialError>;
}

/// Entries under [`SERVICE`] through the `keyring` crate.
struct KeyringEntries;

impl EntryStore for KeyringEntries {
    fn read(&self, name: &str) -> Result<Option<String>, CredentialError> {
        match system_entry(name)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(map_keyring_error(error)),
        }
    }

    fn write(&self, name: &str, value: &str) -> Result<(), CredentialError> {
        system_entry(name)?
            .set_password(value)
            .map_err(map_keyring_error)
    }

    fn remove(&self, name: &str) -> Result<(), CredentialError> {
        match system_entry(name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(map_keyring_error(error)),
        }
    }
}

/// The operating system credential store.
pub struct SystemBackend;

impl SystemBackend {
    /// Checks that the store can be reached, by looking up an entry that never
    /// exists. On Linux this fails without a Secret Service provider. Looking
    /// up a missing entry never asks the user for permission.
    pub fn probe() -> Result<(), CredentialError> {
        let entry = system_entry("availability-probe")?;
        match entry.get_password() {
            Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(map_keyring_error(error)),
        }
    }

    /// The one system backend of this process. Sharing it keeps its cache
    /// coherent however many [`CredentialStore`]s are opened.
    ///
    /// On macOS every secret lives in one Keychain item ([`VaultBackend`]):
    /// the legacy Keychain asks for the login password once per item whenever
    /// the app's signature is not on the item's access list, which is after
    /// every update of an ad hoc signed build. Elsewhere each secret is its
    /// own entry ([`EntryBackend`]); Credential Manager limits entry size and
    /// Secret Service unlocks a whole collection at once.
    pub fn shared() -> Arc<dyn CredentialBackend> {
        static SHARED: OnceLock<Arc<dyn CredentialBackend>> = OnceLock::new();
        SHARED
            .get_or_init(|| {
                let entries: Arc<dyn EntryStore> = Arc::new(KeyringEntries);
                if cfg!(target_os = "macos") {
                    Arc::new(VaultBackend::new(entries))
                } else {
                    Arc::new(EntryBackend::new(entries))
                }
            })
            .clone()
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

/// One entry per secret, each read from the store at most once per process.
/// Failed reads are not cached, so a refused prompt can be retried.
pub struct EntryBackend {
    entries: Arc<dyn EntryStore>,
    cache: Mutex<BTreeMap<String, Option<String>>>,
}

impl EntryBackend {
    pub fn new(entries: Arc<dyn EntryStore>) -> Self {
        Self {
            entries,
            cache: Mutex::new(BTreeMap::new()),
        }
    }
}

impl CredentialBackend for EntryBackend {
    fn kind(&self) -> CredentialBackendKind {
        CredentialBackendKind::System
    }

    fn get(&self, key: &SecretKey) -> Result<Option<Secret>, CredentialError> {
        let name = key.name();
        // Held across the read so concurrent callers share one prompt.
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(value) = cache.get(&name) {
            return Ok(value.clone().map(Secret::new));
        }
        let value = self.entries.read(&name)?;
        cache.insert(name, value.clone());
        Ok(value.map(Secret::new))
    }

    fn set(&self, key: &SecretKey, value: &Secret) -> Result<(), CredentialError> {
        let name = key.name();
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.get(&name).and_then(Option::as_deref) == Some(value.expose()) {
            return Ok(());
        }
        self.entries.write(&name, value.expose())?;
        cache.insert(name, Some(value.expose().to_owned()));
        Ok(())
    }

    fn delete(&self, key: &SecretKey) -> Result<(), CredentialError> {
        let name = key.name();
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(cache.get(&name), Some(None)) {
            return Ok(());
        }
        self.entries.remove(&name)?;
        cache.insert(name, None);
        Ok(())
    }
}

/// Name of the entry that holds every secret for [`VaultBackend`].
pub const VAULT_ENTRY: &str = "secrets";

/// Every secret in one store entry, read at most once per process, so the
/// store asks for permission once instead of once per secret. Unchanged
/// values are not written back. Secrets saved as separate entries by earlier
/// versions move into the vault the first time they are asked for.
pub struct VaultBackend {
    entries: Arc<dyn EntryStore>,
    state: Mutex<VaultState>,
}

#[derive(Default)]
struct VaultState {
    /// `None` until the vault entry has been read.
    secrets: Option<BTreeMap<String, String>>,
    /// Names already looked up, or cleared, as separate entries.
    separate_checked: BTreeSet<String>,
}

impl VaultBackend {
    pub fn new(entries: Arc<dyn EntryStore>) -> Self {
        Self {
            entries,
            state: Mutex::new(VaultState::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VaultState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn loaded<'a>(
        &self,
        state: &'a mut VaultState,
    ) -> Result<&'a mut BTreeMap<String, String>, CredentialError> {
        if state.secrets.is_none() {
            let secrets = match self.entries.read(VAULT_ENTRY)? {
                Some(text) => SecretsFile::decode(text.as_bytes(), "the stored credentials")?,
                None => BTreeMap::new(),
            };
            state.secrets = Some(secrets);
        }
        Ok(state.secrets.as_mut().expect("vault loaded above"))
    }

    /// Writes `secrets` as the vault; memory changes only after it is stored.
    fn store(
        &self,
        state: &mut VaultState,
        secrets: BTreeMap<String, String>,
    ) -> Result<(), CredentialError> {
        let text = SecretsFile::encode(secrets.clone())?;
        self.entries.write(VAULT_ENTRY, &text)?;
        state.secrets = Some(secrets);
        Ok(())
    }

    /// Removes a separate entry an earlier version may have left, once.
    fn clear_separate(&self, state: &mut VaultState, name: &str) {
        if state.separate_checked.insert(name.to_owned()) {
            let _ = self.entries.remove(name);
        }
    }
}

impl CredentialBackend for VaultBackend {
    fn kind(&self) -> CredentialBackendKind {
        CredentialBackendKind::System
    }

    fn get(&self, key: &SecretKey) -> Result<Option<Secret>, CredentialError> {
        let name = key.name();
        let mut state = self.lock();
        if let Some(value) = self.loaded(&mut state)?.get(&name) {
            return Ok(Some(Secret::new(value.clone())));
        }
        if state.separate_checked.contains(&name) {
            return Ok(None);
        }
        // Looking up a missing entry does not prompt, so this costs nothing
        // once every old entry has moved.
        let Some(value) = self.entries.read(&name)? else {
            state.separate_checked.insert(name);
            return Ok(None);
        };
        let mut secrets = self.loaded(&mut state)?.clone();
        secrets.insert(name.clone(), value.clone());
        if self.store(&mut state, secrets).is_ok() {
            self.clear_separate(&mut state, &name);
        }
        Ok(Some(Secret::new(value)))
    }

    fn set(&self, key: &SecretKey, value: &Secret) -> Result<(), CredentialError> {
        let name = key.name();
        let mut state = self.lock();
        let secrets = self.loaded(&mut state)?;
        if secrets.get(&name).map(String::as_str) != Some(value.expose()) {
            let mut secrets = secrets.clone();
            secrets.insert(name.clone(), value.expose().to_owned());
            self.store(&mut state, secrets)?;
        }
        self.clear_separate(&mut state, &name);
        Ok(())
    }

    fn delete(&self, key: &SecretKey) -> Result<(), CredentialError> {
        let name = key.name();
        let mut state = self.lock();
        let secrets = self.loaded(&mut state)?;
        if secrets.contains_key(&name) {
            let mut secrets = secrets.clone();
            secrets.remove(&name);
            self.store(&mut state, secrets)?;
        }
        if state.separate_checked.insert(name.clone()) {
            self.entries.remove(&name)?;
        }
        Ok(())
    }
}

/// Where the local credential file lives: `$XDG_CONFIG_HOME/cayenchat` (or
/// `~/.config/cayenchat`) on Linux and other Unix desktops, the CayenChat
/// configuration directory elsewhere.
pub fn local_credentials_path() -> Result<PathBuf, String> {
    #[cfg(feature = "test-build")]
    {
        Ok(crate::test_build_directory()?.join("credentials.json"))
    }
    #[cfg(all(unix, not(target_os = "macos"), not(feature = "test-build")))]
    {
        let directory = xdg_config_home(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME").map(PathBuf::from),
        )
        .ok_or("Could not find the user configuration directory.")?;
        Ok(directory.join("cayenchat").join("credentials.json"))
    }
    #[cfg(all(not(all(unix, not(target_os = "macos"))), not(feature = "test-build")))]
    {
        let directory =
            dirs::config_dir().ok_or("Could not find the user configuration directory.")?;
        Ok(directory.join("CayenChat").join("credentials.json"))
    }
}

/// The XDG Base Directory rule: a relative or empty `XDG_CONFIG_HOME` is
/// ignored in favor of `$HOME/.config`.
#[cfg_attr(
    any(not(all(unix, not(target_os = "macos"))), feature = "test-build"),
    allow(dead_code)
)]
fn xdg_config_home(xdg: Option<std::ffi::OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    xdg.map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home.map(|home| home.join(".config")))
}

/// The JSON layout shared by the local credential file and the vault entry.
#[derive(Serialize, Deserialize)]
struct SecretsFile {
    version: u32,
    secrets: BTreeMap<String, String>,
}

impl SecretsFile {
    /// `what` names the source in errors, which never include its contents.
    fn decode(bytes: &[u8], what: &str) -> Result<BTreeMap<String, String>, CredentialError> {
        let file: SecretsFile = serde_json::from_slice(bytes)
            .map_err(|_| CredentialError::Format(format!("{what} are not valid")))?;
        if file.version != SECRETS_FILE_VERSION {
            return Err(CredentialError::Format(format!(
                "unsupported credential file version {}",
                file.version
            )));
        }
        Ok(file.secrets)
    }

    fn encode(secrets: BTreeMap<String, String>) -> Result<String, CredentialError> {
        serde_json::to_string_pretty(&SecretsFile {
            version: SECRETS_FILE_VERSION,
            secrets,
        })
        .map_err(|_| CredentialError::Format("could not encode credentials".into()))
    }
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
        SecretsFile::decode(&bytes, "the credential file contents")
    }

    fn write(&self, secrets: BTreeMap<String, String>) -> Result<(), CredentialError> {
        let bytes = SecretsFile::encode(secrets)?.into_bytes();
        crate::private_file::write(&self.path, &bytes, crate::private_file::Flush::Disk)
            .map_err(|error| CredentialError::Io(error.kind().to_string()))
    }
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

    fn is_memory(&self) -> bool {
        true
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

    /// OS entries in memory. `prompts` counts reads of existing entries, the
    /// reads that make macOS ask for the login password.
    #[derive(Default)]
    struct FakeEntries {
        entries: Mutex<BTreeMap<String, String>>,
        prompts: Mutex<usize>,
        writes: Mutex<usize>,
    }

    impl FakeEntries {
        fn prompts(&self) -> usize {
            *self.prompts.lock().unwrap()
        }

        fn writes(&self) -> usize {
            *self.writes.lock().unwrap()
        }

        fn names(&self) -> Vec<String> {
            self.entries.lock().unwrap().keys().cloned().collect()
        }
    }

    impl EntryStore for FakeEntries {
        fn read(&self, name: &str) -> Result<Option<String>, CredentialError> {
            let value = self.entries.lock().unwrap().get(name).cloned();
            if value.is_some() {
                *self.prompts.lock().unwrap() += 1;
            }
            Ok(value)
        }

        fn write(&self, name: &str, value: &str) -> Result<(), CredentialError> {
            *self.writes.lock().unwrap() += 1;
            self.entries
                .lock()
                .unwrap()
                .insert(name.to_owned(), value.to_owned());
            Ok(())
        }

        fn remove(&self, name: &str) -> Result<(), CredentialError> {
            self.entries.lock().unwrap().remove(name);
            Ok(())
        }
    }

    fn vault(entries: &Arc<FakeEntries>) -> CredentialStore {
        CredentialStore::with_backend(Arc::new(VaultBackend::new(entries.clone())))
    }

    #[test]
    fn vault_reads_the_store_once_for_all_secrets() {
        let entries = Arc::new(FakeEntries::default());
        let keys = [
            SecretKey::server_password("ircnet"),
            SecretKey::sasl_password("ircnet"),
            SecretKey::uploader_token("imgbb"),
        ];
        let first = vault(&entries);
        for (index, key) in keys.iter().enumerate() {
            first
                .set(key, &Secret::new(format!("secret-{index}")))
                .unwrap();
        }
        assert_eq!(entries.names(), [VAULT_ENTRY]);

        // A new process: every lookup, present or not, costs one prompt.
        let second = vault(&entries);
        for (index, key) in keys.iter().enumerate() {
            let value = second.get(key).unwrap().unwrap();
            assert_eq!(value.expose(), format!("secret-{index}"));
            assert!(second.contains(key).unwrap());
        }
        assert!(!second.contains(&SecretKey::sasl_password("other")).unwrap());
        assert!(!second.contains(&SecretKey::sasl_password("other")).unwrap());
        assert_eq!(entries.prompts(), 1);
    }

    #[test]
    fn vault_skips_unchanged_writes_and_deletes() {
        let entries = Arc::new(FakeEntries::default());
        let store = vault(&entries);
        let key = SecretKey::sasl_password("ircnet");
        store.set(&key, &Secret::new("pass")).unwrap();
        store.set(&key, &Secret::new("pass")).unwrap();
        store.delete(&SecretKey::server_password("ircnet")).unwrap();
        assert_eq!(entries.writes(), 1);
        store.delete(&key).unwrap();
        assert_eq!(entries.writes(), 2);
        assert!(vault(&entries).get(&key).unwrap().is_none());
    }

    #[test]
    fn vault_moves_separate_entries_in_once() {
        let entries = Arc::new(FakeEntries::default());
        let old = SecretKey::sasl_password("ircnet");
        let stale = SecretKey::server_password("ircnet");
        entries.write(&old.name(), "old-sasl").unwrap();
        entries.write(&stale.name(), "old-pass").unwrap();

        let store = vault(&entries);
        assert_eq!(store.get(&old).unwrap().unwrap().expose(), "old-sasl");
        // Replacing or deleting a secret also drops its old separate entry.
        store.delete(&stale).unwrap();
        assert_eq!(entries.names(), [VAULT_ENTRY]);
        assert!(store.get(&stale).unwrap().is_none());

        let prompts = entries.prompts();
        let next = vault(&entries);
        assert_eq!(next.get(&old).unwrap().unwrap().expose(), "old-sasl");
        assert!(next.get(&stale).unwrap().is_none());
        assert_eq!(entries.prompts(), prompts + 1);
    }

    #[test]
    fn vault_rejects_unreadable_data_without_leaking_it() {
        let entries = Arc::new(FakeEntries::default());
        entries
            .write(VAULT_ENTRY, "{\"secrets\":\"leak-me\"")
            .unwrap();
        let error = vault(&entries)
            .get(&SecretKey::sasl_password("a"))
            .unwrap_err();
        assert!(matches!(error, CredentialError::Format(_)));
        assert!(!format!("{error} {error:?}").contains("leak-me"));
    }

    #[test]
    fn entry_backend_reads_each_entry_once() {
        let entries = Arc::new(FakeEntries::default());
        let key = SecretKey::sasl_password("ircnet");
        entries.write(&key.name(), "pass").unwrap();
        let store = CredentialStore::with_backend(Arc::new(EntryBackend::new(entries.clone())));
        assert!(store.contains(&key).unwrap());
        assert_eq!(store.get(&key).unwrap().unwrap().expose(), "pass");
        store.set(&key, &Secret::new("pass")).unwrap();
        assert_eq!((entries.prompts(), entries.writes()), (1, 1));
        store.delete(&key).unwrap();
        assert!(store.get(&key).unwrap().is_none());
        assert!(entries.names().is_empty());
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

    // XDG paths are Unix paths; `/xdg` is not absolute on Windows.
    #[cfg(unix)]
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
