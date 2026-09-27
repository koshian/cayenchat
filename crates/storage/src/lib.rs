//! Versioned preferences. Secrets are not part of the preferences file; they
//! live in the [`credentials`] store.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

pub mod credentials;

pub use credentials::{CredentialBackendKind, CredentialError, CredentialStore, Secret, SecretKey};

const SETTINGS_VERSION: u32 = 12;
/// Profile IDs of the IRCnet servers that versions 1–11 always listed. They
/// only matter for migration: profiles keep their IDs, so saved passwords stay
/// attached.
pub const IRCNET_ID: &str = "ircnet";
pub const IRCNET_IPV6_ID: &str = "ircnet-ipv6";

/// A well-known server offered when adding a server. Presets are never
/// stored by themselves; adding one creates an ordinary server profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerPreset {
    pub name: &'static str,
    pub host: &'static str,
}

pub const PRESETS: [ServerPreset; 2] = [
    ServerPreset {
        name: "IRCnet",
        host: "irc.ircnet.ne.jp",
    },
    ServerPreset {
        name: "IRCnet (IPv6)",
        host: "irc6.ircnet.ne.jp",
    },
];

fn default_verify_tls_certificates() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextEncoding {
    #[default]
    Utf8,
    Iso2022Jp,
    ShiftJis,
    EucJp,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

/// Display server for Linux builds, applied at startup. Wayland keeps native
/// input methods and fractional scaling; X11 (XWayland on Wayland desktops)
/// lets the window manager draw a themed title bar where the compositor does
/// not offer server-side decorations, such as GNOME.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxDisplay {
    #[default]
    Wayland,
    X11,
}

/// Modifier for the numbered channel shortcuts on Windows and Linux. Many
/// Linux desktops reserve Ctrl+digit for workspace switching.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelNumberModifier {
    #[default]
    Ctrl,
    Alt,
    Super,
}

/// Draft-editing key bindings on Linux. `Auto` follows the desktop's GTK
/// key theme (`gtk-key-theme`), so Emacs users get Ctrl+A/E/K and friends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextKeyTheme {
    #[default]
    Auto,
    Standard,
    Emacs,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    #[default]
    System,
    Japanese,
    English,
}

impl TextEncoding {
    pub const ALL: [Self; 4] = [Self::Utf8, Self::Iso2022Jp, Self::ShiftJis, Self::EucJp];

    pub fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Iso2022Jp => "ISO-2022-JP",
            Self::ShiftJis => "Shift_JIS",
            Self::EucJp => "EUC-JP",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    #[serde(alias = "background")]
    pub member_list_background: String,
    pub main_log_background: String,
    pub main_log_alternate: String,
    pub channel_event_color: String,
    pub sub_log_background: String,
    pub sub_log_alternate: String,
    pub alternate_rows: bool,
    pub main_log_font: String,
    pub sub_log_font: String,
    pub member_font: String,
    pub channel_font: String,
    pub input_font: String,
    pub time_font: String,
    /// Pane colors used while the dark theme is active; the flat fields above
    /// are the light theme's.
    pub dark: DarkColors,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DarkColors {
    pub member_list_background: String,
    pub main_log_background: String,
    pub main_log_alternate: String,
    pub channel_event_color: String,
    pub sub_log_background: String,
    pub sub_log_alternate: String,
}

impl Default for DarkColors {
    fn default() -> Self {
        Self {
            member_list_background: "#1F2124".into(),
            main_log_background: "#1F2124".into(),
            main_log_alternate: "#272B31".into(),
            channel_event_color: "#6CC46C".into(),
            sub_log_background: "#24272B".into(),
            sub_log_alternate: "#2C3036".into(),
        }
    }
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            member_list_background: "#FFFFFF".into(),
            main_log_background: "#FFFFFF".into(),
            main_log_alternate: "#F2F5FF".into(),
            channel_event_color: "#007D00".into(),
            sub_log_background: "#F9FAFB".into(),
            sub_log_alternate: "#F2F5FF".into(),
            alternate_rows: false,
            main_log_font: String::new(),
            sub_log_font: String::new(),
            member_font: String::new(),
            channel_font: String::new(),
            input_font: String::new(),
            time_font: String::new(),
            dark: DarkColors::default(),
        }
    }
}

impl Appearance {
    pub fn validate(&self) -> Result<(), String> {
        for (label, value) in [
            ("Member list", &self.member_list_background),
            ("Main log", &self.main_log_background),
            ("Main alternate", &self.main_log_alternate),
            ("Channel event", &self.channel_event_color),
            ("Sub log", &self.sub_log_background),
            ("Sub alternate", &self.sub_log_alternate),
            ("Dark member list", &self.dark.member_list_background),
            ("Dark main log", &self.dark.main_log_background),
            ("Dark main alternate", &self.dark.main_log_alternate),
            ("Dark channel event", &self.dark.channel_event_color),
            ("Dark sub log", &self.dark.sub_log_background),
            ("Dark sub alternate", &self.dark.sub_log_alternate),
        ] {
            if color_value(value).is_none() {
                return Err(format!("{label} color must be #RRGGBB."));
            }
        }
        Ok(())
    }
}

pub fn color_value(value: &str) -> Option<u32> {
    let hex = value.strip_prefix('#')?;
    (hex.len() == 6)
        .then(|| u32::from_str_radix(hex, 16).ok())
        .flatten()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerProfile {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub use_tls: bool,
    #[serde(default = "default_verify_tls_certificates")]
    pub verify_tls_certificates: bool,
    pub encoding: TextEncoding,
    /// Keep this profile's server and SASL passwords in the credential store.
    #[serde(default)]
    pub remember_passwords: bool,
    /// Identity and channels belong to each server (version 12); earlier
    /// versions kept one application-wide set, copied here on migration.
    #[serde(default)]
    pub nickname: String,
    /// IRC `USER` username (ident); independent of the nickname.
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub channels: String,
    #[serde(default)]
    pub sasl_enabled: bool,
    /// SASL account name; independent of both nickname and username.
    #[serde(default)]
    pub sasl_username: String,
    #[serde(default)]
    pub connect_on_startup: bool,
    /// Plaintext passwords saved by version 10 and earlier. Read only for
    /// migration into the credential store; never written back.
    #[serde(rename = "server_password", default, skip_serializing)]
    legacy_server_password: Option<Secret>,
    #[serde(rename = "sasl_password", default, skip_serializing)]
    legacy_sasl_password: Option<Secret>,
}

/// An empty profile, for forms shown before any server exists.
impl Default for ServerProfile {
    fn default() -> Self {
        Self::new(String::new(), "")
    }
}

impl ServerProfile {
    fn new(id: String, host: &str) -> Self {
        Self {
            id,
            host: host.into(),
            port: 6667,
            use_tls: false,
            verify_tls_certificates: true,
            encoding: TextEncoding::Utf8,
            remember_passwords: false,
            nickname: String::new(),
            username: String::new(),
            channels: String::new(),
            sasl_enabled: false,
            sasl_username: String::new(),
            connect_on_startup: false,
            legacy_server_password: None,
            legacy_sasl_password: None,
        }
    }

    pub fn channels(&self) -> Vec<String> {
        self.channels
            .split(',')
            .map(str::trim)
            .filter(|channel| !channel.is_empty())
            .map(str::to_owned)
            .collect()
    }

    pub fn server_password_key(&self) -> SecretKey {
        SecretKey::server_password(&self.id)
    }

    pub fn sasl_password_key(&self) -> SecretKey {
        SecretKey::sasl_password(&self.id)
    }
}

/// External image hosting for IRC. Disabled until the user picks a provider.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageUpload {
    /// Provider ID from the uploader registry, or `None` when disabled.
    pub provider: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub selected_server: String,
    pub servers: Vec<ServerProfile>,
    pub language: Language,
    pub theme: ThemeMode,
    pub linux_display: LinuxDisplay,
    pub channel_number_modifier: ChannelNumberModifier,
    pub text_key_theme: TextKeyTheme,
    pub appearance: Appearance,
    pub credential_backend: CredentialBackendKind,
    pub image_upload: ImageUpload,
    /// Application-wide identity of versions 1–11, read only to migrate it
    /// into every server profile; never written back.
    #[serde(flatten, skip_serializing)]
    legacy: LegacyIdentity,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
struct LegacyIdentity {
    nickname: String,
    username: String,
    channels: String,
    sasl_enabled: bool,
    sasl_username: String,
    connect_on_startup: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            // No servers until the user adds one; see `PRESETS`.
            selected_server: String::new(),
            servers: Vec::new(),
            language: Language::System,
            theme: ThemeMode::System,
            linux_display: LinuxDisplay::Wayland,
            channel_number_modifier: ChannelNumberModifier::Ctrl,
            text_key_theme: TextKeyTheme::Auto,
            appearance: Appearance::default(),
            credential_backend: CredentialBackendKind::System,
            image_upload: ImageUpload::default(),
            legacy: LegacyIdentity::default(),
        }
    }
}

impl Settings {
    /// Every connection secret key this configuration may have stored.
    pub fn connection_secret_keys(&self) -> Vec<SecretKey> {
        self.servers
            .iter()
            .flat_map(|server| [server.server_password_key(), server.sasl_password_key()])
            .collect()
    }

    /// Takes plaintext passwords loaded from an older settings file.
    pub fn take_legacy_secrets(&mut self) -> Vec<(SecretKey, Secret)> {
        let mut secrets = Vec::new();
        for server in &mut self.servers {
            if let Some(value) = server.legacy_server_password.take()
                && !value.is_empty()
            {
                secrets.push((server.server_password_key(), value));
            }
            if let Some(value) = server.legacy_sasl_password.take()
                && !value.is_empty()
            {
                secrets.push((server.sasl_password_key(), value));
            }
        }
        secrets
    }

    pub fn has_legacy_secrets(&self) -> bool {
        self.servers.iter().any(|server| {
            server.legacy_server_password.is_some() || server.legacy_sasl_password.is_some()
        })
    }

    /// The server shown in the settings form; `None` when there is none.
    pub fn selected_profile(&self) -> Option<&ServerProfile> {
        self.profile(&self.selected_server)
    }

    pub fn selected_profile_mut(&mut self) -> Option<&mut ServerProfile> {
        self.servers
            .iter_mut()
            .find(|s| s.id == self.selected_server)
    }

    /// Servers in display order (the order they were added).
    pub fn ordered_servers(&self) -> impl Iterator<Item = &ServerProfile> {
        self.servers.iter()
    }

    /// Adds a server with `host` (empty for a blank one, or a preset's host)
    /// and selects it.
    pub fn add_server(&mut self, host: &str) -> &mut ServerProfile {
        // Credentials outlive unsaved form edits and may survive failed deletes.
        // A new profile must never reuse a removed profile's credential keys.
        let id = format!("custom-{}", uuid::Uuid::new_v4());
        self.selected_server = id.clone();
        self.servers.push(ServerProfile::new(id, host));
        self.servers.last_mut().expect("just added")
    }

    pub fn profile(&self, id: &str) -> Option<&ServerProfile> {
        self.servers.iter().find(|server| server.id == id)
    }

    pub fn remove_selected_server(&mut self) {
        self.servers.retain(|s| s.id != self.selected_server);
        self.selected_server = self
            .servers
            .first()
            .map(|s| s.id.clone())
            .unwrap_or_default();
    }

    fn normalize(mut self) -> Self {
        if self.version <= 7
            && self
                .appearance
                .member_list_background
                .eq_ignore_ascii_case("#ECECEC")
        {
            self.appearance.member_list_background = "#FFFFFF".into();
        }
        if self.version <= 8
            && self
                .appearance
                .channel_event_color
                .eq_ignore_ascii_case("#3B7655")
        {
            self.appearance.channel_event_color = "#007D00".into();
        }
        if self.version <= 10 && self.legacy.username.is_empty() {
            // Earlier versions sent the nickname as the USER username; keep
            // that as the initial value so existing connections do not change.
            self.legacy.username = self.legacy.nickname.clone();
        }
        let migrate_identity = self.version <= 11;
        self.version = SETTINGS_VERSION;
        let mut seen = HashSet::new();
        self.servers
            .retain(|s| !s.id.is_empty() && seen.insert(s.id.clone()) && s.port != 0);
        if migrate_identity {
            // Versions 1–11 always listed both IRCnet servers after the
            // user-added ones. Keep a preset only if it was in use: selected
            // (created if the file omitted it) or holding saved passwords.
            for (index, id) in [IRCNET_ID, IRCNET_IPV6_ID].into_iter().enumerate() {
                let host = PRESETS[index].host;
                match self.servers.iter_mut().find(|s| s.id == id) {
                    Some(server) => server.host = host.into(),
                    None if self.selected_server == id => {
                        self.servers.push(ServerProfile::new(id.into(), host))
                    }
                    None => {}
                }
            }
            let selected = self.selected_server.clone();
            let is_preset = |s: &ServerProfile| s.id == IRCNET_ID || s.id == IRCNET_IPV6_ID;
            self.servers
                .retain(|s| !is_preset(s) || s.id == selected || s.remember_passwords);
            self.servers.sort_by_key(is_preset);
        }
        self.servers.retain(|s| !s.host.trim().is_empty());
        for server in &mut self.servers {
            if !server.use_tls {
                server.verify_tls_certificates = true;
            }
            if !server.remember_passwords {
                server.legacy_server_password = None;
                server.legacy_sasl_password = None;
            }
        }
        if !self.servers.iter().any(|s| s.id == self.selected_server) {
            self.selected_server = self
                .servers
                .first()
                .map(|s| s.id.clone())
                .unwrap_or_default();
        }
        let legacy = std::mem::take(&mut self.legacy);
        if migrate_identity {
            // Every server starts from the old shared identity. SASL stays on
            // only where TLS is, because credentials require TLS; startup
            // connection stays with the server that used to be connected.
            for server in &mut self.servers {
                server.nickname = legacy.nickname.clone();
                server.username = legacy.username.clone();
                server.channels = legacy.channels.clone();
                server.sasl_username = legacy.sasl_username.clone();
                server.sasl_enabled = legacy.sasl_enabled && server.use_tls;
                server.connect_on_startup =
                    legacy.connect_on_startup && server.id == self.selected_server;
            }
        }
        for server in &mut self.servers {
            if !server.use_tls {
                server.sasl_enabled = false;
            }
        }
        self
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OldServerChoice {
    #[default]
    Ircnet,
    IrcnetIpv6,
    Custom,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct OldSettings {
    server: OldServerChoice,
    custom_host: String,
    port: u16,
    use_tls: bool,
    nickname: String,
    channels: String,
    sasl_enabled: bool,
    sasl_username: String,
}

impl From<OldSettings> for Settings {
    fn from(old: OldSettings) -> Self {
        let mut settings = Settings {
            version: 1,
            legacy: LegacyIdentity {
                nickname: old.nickname,
                channels: old.channels,
                sasl_enabled: old.sasl_enabled,
                sasl_username: old.sasl_username,
                ..LegacyIdentity::default()
            },
            ..Settings::default()
        };
        let (id, host) = match old.server {
            OldServerChoice::Ircnet => (IRCNET_ID.to_owned(), PRESETS[0].host),
            OldServerChoice::IrcnetIpv6 => (IRCNET_IPV6_ID.to_owned(), PRESETS[1].host),
            OldServerChoice::Custom => ("custom-1".to_owned(), old.custom_host.as_str()),
        };
        let mut server = ServerProfile::new(id, host);
        server.port = old.port;
        server.use_tls = old.use_tls;
        settings.selected_server = server.id.clone();
        settings.servers.push(server);
        settings.normalize()
    }
}

pub fn settings_path() -> Result<PathBuf, String> {
    #[cfg(feature = "test-build")]
    {
        Ok(test_build_directory()?.join("settings.json"))
    }
    #[cfg(not(feature = "test-build"))]
    {
        let directory =
            dirs::config_dir().ok_or("Could not find the user configuration directory.")?;
        Ok(directory.join("CayenChat").join("settings.json"))
    }
}

/// A test build's configuration directory: new and empty for every launch,
/// under the system temporary directory, readable only by the user.
#[cfg(feature = "test-build")]
pub fn test_build_directory() -> Result<PathBuf, String> {
    static DIRECTORY: std::sync::OnceLock<Result<PathBuf, String>> = std::sync::OnceLock::new();
    DIRECTORY
        .get_or_init(|| {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_millis())
                .unwrap_or_default();
            let directory =
                std::env::temp_dir().join(format!("cayenchat-test-{}-{stamp}", std::process::id()));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder
                .create(&directory)
                .map(|()| directory)
                .map_err(|error| format!("Could not create the test settings directory: {error}"))
        })
        .clone()
}

pub fn load() -> Result<Option<Settings>, String> {
    load_from(&settings_path()?)
}

pub fn save(settings: &Settings) -> Result<(), String> {
    save_to(&settings_path()?, settings)
}

/// Turns off password saving for a profile in the saved file. The caller
/// deletes the secrets from the credential store.
pub fn clear_saved_passwords(server_id: &str) -> Result<(), String> {
    let path = settings_path()?;
    clear_saved_passwords_from(&path, server_id)
}

/// Outcome of moving pre-version-11 plaintext passwords out of settings.json.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LegacyMigration {
    pub moved: usize,
    /// The system store was unavailable, so the passwords, which the user had
    /// already agreed to keep as plaintext, went to the local credential file.
    pub used_local_file: bool,
}

/// Moves plaintext passwords from older settings into the credential store and
/// rewrites settings.json without them. `open` supplies the backends.
pub fn migrate_legacy_secrets(
    settings: &mut Settings,
    open: &dyn Fn(CredentialBackendKind) -> CredentialStore,
) -> Result<Option<LegacyMigration>, String> {
    migrate_legacy_secrets_at(&settings_path()?, settings, open)
}

fn migrate_legacy_secrets_at(
    path: &Path,
    settings: &mut Settings,
    open: &dyn Fn(CredentialBackendKind) -> CredentialStore,
) -> Result<Option<LegacyMigration>, String> {
    if !settings.has_legacy_secrets() {
        return Ok(None);
    }
    let secrets = settings.take_legacy_secrets();
    let store_all = |store: &CredentialStore| {
        secrets
            .iter()
            .try_for_each(|(key, value)| store.set(key, value))
    };
    let mut used_local_file = false;
    match store_all(&open(settings.credential_backend)) {
        Ok(()) => {}
        Err(CredentialError::Unavailable(_))
            if settings.credential_backend == CredentialBackendKind::System =>
        {
            store_all(&open(CredentialBackendKind::LocalFile)).map_err(|e| e.to_string())?;
            settings.credential_backend = CredentialBackendKind::LocalFile;
            used_local_file = true;
        }
        Err(error) => return Err(error.to_string()),
    }
    save_to(path, settings)?;
    Ok(Some(LegacyMigration {
        moved: secrets.len(),
        used_local_file,
    }))
}

fn clear_saved_passwords_from(path: &Path, server_id: &str) -> Result<(), String> {
    let Some(mut settings) = load_from(path)? else {
        return Ok(());
    };
    if let Some(server) = settings
        .servers
        .iter_mut()
        .find(|server| server.id == server_id)
    {
        server.remember_passwords = false;
        save_to(path, &settings)?;
    }
    Ok(())
}

fn load_from(path: &Path) -> Result<Option<Settings>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Could not read settings: {error}")),
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Could not parse settings: {error}"))?;
    let version = value.get("version").and_then(serde_json::Value::as_u64);
    let settings = match version {
        Some(1) => serde_json::from_value::<OldSettings>(value)
            .map(Settings::from)
            .map_err(|error| format!("Could not parse settings: {error}"))?,
        Some(version) if (2..=u64::from(SETTINGS_VERSION)).contains(&version) => {
            serde_json::from_value::<Settings>(value)
                .map(Settings::normalize)
                .map_err(|error| format!("Could not parse settings: {error}"))?
        }
        Some(version) if version > u64::from(SETTINGS_VERSION) => {
            return Err(format!(
                "Settings version {version} is newer than this app supports (1–{SETTINGS_VERSION}). \
                 Update CayenChat to a version that supports these settings. \
                 Settings were left unchanged at {}.",
                path.display()
            ));
        }
        _ => {
            return Err(format!(
                "The settings version is missing or invalid; this app supports versions 1–{SETTINGS_VERSION}. \
                 Restore a valid settings file from a backup. Settings were left unchanged at {}.",
                path.display()
            ));
        }
    };
    Ok(Some(settings))
}

fn save_to(path: &Path, settings: &Settings) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or("Settings path has no parent directory.")?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Could not create settings directory: {error}"))?;
    let bytes = serde_json::to_vec_pretty(&settings.clone().normalize())
        .map_err(|error| format!("Could not serialize settings: {error}"))?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("Could not open settings file: {error}"))?;
    #[cfg(unix)]
    fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
        .map_err(|error| format!("Could not protect settings file: {error}"))?;
    use std::io::Write;
    file.write_all(&bytes)
        .map_err(|error| format!("Could not write settings: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_without_secrets() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut settings = Settings::default();
        settings.add_server("");
        settings.selected_profile_mut().unwrap().host = "irc.example.net".into();
        settings.selected_profile_mut().unwrap().port = 6697;
        settings.selected_profile_mut().unwrap().use_tls = true;
        settings
            .selected_profile_mut()
            .unwrap()
            .verify_tls_certificates = false;
        settings.selected_profile_mut().unwrap().encoding = TextEncoding::Iso2022Jp;
        let profile = settings.selected_profile_mut().unwrap();
        profile.nickname = "alice".into();
        profile.channels = "#one, #two".into();
        profile.sasl_enabled = true;
        profile.sasl_username = "account".into();
        profile.connect_on_startup = true;
        save_to(&path, &settings).unwrap();
        assert_eq!(load_from(&path).unwrap(), Some(settings.clone()));
        assert_eq!(
            settings.selected_profile().unwrap().channels(),
            ["#one", "#two"]
        );
        assert_eq!(settings.selected_profile().unwrap().host, "irc.example.net");
        assert_eq!(
            settings.ordered_servers().next().unwrap().host,
            "irc.example.net"
        );
        let file_text = fs::read_to_string(path).unwrap();
        assert!(!file_text.contains("server_password"));
        assert!(!file_text.contains("sasl_password"));
    }

    #[test]
    fn supported_profile_versions_preserve_connection_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        for version in 2..SETTINGS_VERSION {
            let original = serde_json::json!({
                "version": version,
                "selected_server": "custom-1",
                "servers": [{
                    "id": "custom-1", "custom": true, "host": "irc.example.net",
                    "port": 6697, "use_tls": true, "encoding": "utf8",
                    "remember_passwords": true
                }],
                "nickname": "alice", "username": "ident",
                "channels": "#test", "sasl_enabled": true, "sasl_username": "account",
                "credential_backend": "system"
            });
            let bytes = serde_json::to_vec(&original).unwrap();
            fs::write(&path, &bytes).unwrap();
            let settings = load_from(&path).unwrap().unwrap();
            assert_eq!(settings.version, SETTINGS_VERSION);
            assert_eq!(settings.selected_profile().unwrap().id, "custom-1");
            assert_eq!(settings.selected_profile().unwrap().host, "irc.example.net");
            assert_eq!(settings.selected_profile().unwrap().port, 6697);
            assert!(settings.selected_profile().unwrap().use_tls);
            assert!(settings.selected_profile().unwrap().remember_passwords);
            let profile = settings.selected_profile().unwrap();
            assert_eq!(profile.nickname, "alice");
            assert_eq!(profile.username, "ident");
            assert_eq!(profile.channels, "#test");
            assert!(profile.sasl_enabled);
            assert_eq!(profile.sasl_username, "account");
            assert_eq!(fs::read(&path).unwrap(), bytes);
            save_to(&path, &settings).unwrap();
            assert_eq!(load_from(&path).unwrap(), Some(settings));
        }
    }

    #[test]
    fn version_eleven_identity_moves_into_every_server() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let old = serde_json::json!({
            "version": 11,
            "selected_server": "custom-1",
            "servers": [
                {"id": "custom-1", "custom": true, "host": "irc.example.net",
                 "port": 6697, "use_tls": true, "encoding": "utf8"},
                {"id": "custom-2", "custom": true, "host": "irc.example.org",
                 "port": 6667, "use_tls": false, "encoding": "utf8"}
            ],
            "nickname": "alice", "username": "ident", "channels": "#a,#b",
            "sasl_enabled": true, "sasl_username": "account",
            "connect_on_startup": true
        });
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        assert_eq!(
            settings.servers.len(),
            2,
            "the unused IRCnet presets are dropped"
        );
        for server in &settings.servers {
            assert_eq!(server.nickname, "alice");
            assert_eq!(server.username, "ident");
            assert_eq!(server.channels(), ["#a", "#b"]);
            assert_eq!(server.sasl_username, "account");
            // Credentials need TLS, and only the formerly connected server
            // keeps connecting at startup.
            assert_eq!(server.sasl_enabled, server.use_tls);
            assert_eq!(server.connect_on_startup, server.id == "custom-1");
        }

        // Version 12 keeps each server's values and writes no shared identity.
        settings.servers[1].nickname = "bob".into();
        settings.servers[1].channels = "#c".into();
        save_to(&path, &settings).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["version"], SETTINGS_VERSION);
        for key in [
            "nickname",
            "username",
            "channels",
            "sasl_enabled",
            "connect_on_startup",
        ] {
            assert!(saved.get(key).is_none(), "{key} is per server now");
        }
        let loaded = load_from(&path).unwrap().unwrap();
        assert_eq!(loaded, settings);
        assert_eq!(loaded.profile("custom-1").unwrap().nickname, "alice");
        assert_eq!(loaded.profile("custom-2").unwrap().nickname, "bob");
        assert_eq!(loaded.profile("custom-2").unwrap().channels(), ["#c"]);
    }

    #[test]
    fn new_settings_have_no_servers_and_presets_are_only_suggestions() {
        let mut settings = Settings::default();
        assert!(settings.servers.is_empty());
        assert!(settings.selected_profile().is_none());
        let added = settings.add_server(PRESETS[1].host);
        assert_eq!(added.host, "irc6.ircnet.ne.jp");
        assert!(
            added.id.starts_with("custom-"),
            "a fresh ID, not a preset's"
        );
        settings.remove_selected_server();
        assert!(settings.servers.is_empty());
        assert_eq!(settings.selected_server, "");
        // An empty list survives a round trip.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        save_to(&path, &settings).unwrap();
        assert!(load_from(&path).unwrap().unwrap().servers.is_empty());
    }

    #[test]
    fn migration_keeps_only_irc_net_presets_in_use() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let server = |id: &str, custom: bool, host: &str, remember: bool| {
            serde_json::json!({"id": id, "custom": custom, "host": host, "port": 6667,
                "use_tls": false, "encoding": "utf8", "remember_passwords": remember})
        };
        for (selected, remember_ipv6, expected) in [
            // Selected preset kept after the user's servers; the other dropped.
            (IRCNET_ID, false, vec!["custom-1", IRCNET_ID]),
            // A preset with saved passwords is kept even when not selected.
            ("custom-1", true, vec!["custom-1", IRCNET_IPV6_ID]),
            // Neither preset in use.
            ("custom-1", false, vec!["custom-1"]),
        ] {
            let old = serde_json::json!({
                "version": 11,
                "selected_server": selected,
                "servers": [
                    server(IRCNET_ID, false, "irc.ircnet.ne.jp", false),
                    server(IRCNET_IPV6_ID, false, "irc6.ircnet.ne.jp", remember_ipv6),
                    server("custom-1", true, "irc.example.org", false),
                ],
                "nickname": "alice"
            });
            fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
            let settings = load_from(&path).unwrap().unwrap();
            let ids: Vec<_> = settings.servers.iter().map(|s| s.id.as_str()).collect();
            assert_eq!(ids, expected, "selected {selected}");
            assert_eq!(settings.selected_server, selected);
        }
        // A version 9 file without servers selected IRCnet implicitly.
        fs::write(
            &path,
            r#"{"version":9,"selected_server":"ircnet","servers":[]}"#,
        )
        .unwrap();
        let settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.servers.len(), 1);
        assert_eq!(
            settings.selected_profile().unwrap().host,
            "irc.ircnet.ne.jp"
        );
    }

    #[test]
    fn future_settings_explain_how_to_recover_without_changing_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let future = SETTINGS_VERSION + 1;
        let bytes = format!(r#"{{"version":{future},"future_field":"keep-me"}}"#);
        fs::write(&path, &bytes).unwrap();
        let error = load_from(&path).unwrap_err();
        assert!(error.contains(&format!("Settings version {future}")));
        assert!(error.contains(&format!("1–{SETTINGS_VERSION}")));
        assert!(error.contains("Update CayenChat"));
        assert!(error.contains(path.to_str().unwrap()));
        assert_eq!(fs::read_to_string(&path).unwrap(), bytes);
        assert!(clear_saved_passwords_from(&path, IRCNET_ID).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), bytes);
    }

    #[test]
    fn invalid_versions_do_not_suggest_an_app_update_or_change_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        for bytes in [
            r#"{}"#,
            r#"{"version":null}"#,
            r#"{"version":0}"#,
            r#"{"version":-1}"#,
            r#"{"version":1.5}"#,
            r#"{"version":"private-invalid-value"}"#,
        ] {
            fs::write(&path, bytes).unwrap();
            let error = load_from(&path).unwrap_err();
            assert!(error.contains("missing or invalid"));
            assert!(!error.contains("Update CayenChat"));
            assert!(!error.contains("private-invalid-value"));
            assert!(error.contains(path.to_str().unwrap()));
            assert_eq!(fs::read_to_string(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn custom_profile_ids_survive_reload_without_reusing_removed_ids() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut settings = Settings::default();
        settings.add_server("");
        settings.selected_profile_mut().unwrap().host = "legacy.example.org".into();
        settings.selected_profile_mut().unwrap().id = "custom-1".into();
        settings.selected_server = "custom-1".into();
        settings.add_server("");
        settings.selected_profile_mut().unwrap().host = "new.example.org".into();
        let removed = settings.selected_profile().unwrap().clone();
        save_to(&path, &settings).unwrap();

        let mut loaded = load_from(&path).unwrap().unwrap();
        assert_eq!(loaded.selected_profile().unwrap().id, removed.id);
        assert!(loaded.servers.iter().any(|server| server.id == "custom-1"));
        loaded.remove_selected_server();
        save_to(&path, &loaded).unwrap();
        let mut reloaded = load_from(&path).unwrap().unwrap();
        reloaded.add_server("");
        assert_ne!(reloaded.selected_profile().unwrap().id, removed.id);
        assert_ne!(reloaded.selected_profile().unwrap().id, "custom-1");
    }

    #[test]
    fn migrates_version_one_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(&path, r##"{"version":1,"server":"custom","custom_host":"irc.example.net","port":6697,"use_tls":true,"nickname":"alice","channels":"#日本語","sasl_enabled":false,"sasl_username":""}"##).unwrap();
        let settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.version, SETTINGS_VERSION);
        assert_eq!(settings.selected_profile().unwrap().host, "irc.example.net");
        assert_eq!(settings.selected_profile().unwrap().port, 6697);
        assert!(settings.selected_profile().unwrap().verify_tls_certificates);
        assert_eq!(settings.selected_profile().unwrap().channels(), ["#日本語"]);
        assert_eq!(settings.selected_profile().unwrap().username, "alice");
    }

    #[test]
    fn migrates_version_two_without_enabling_password_storage() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old["version"] = 2.into();
        for server in old["servers"].as_array_mut().unwrap() {
            server.as_object_mut().unwrap().remove("remember_passwords");
            server
                .as_object_mut()
                .unwrap()
                .remove("verify_tls_certificates");
        }
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.version, SETTINGS_VERSION);
        assert!(
            settings
                .servers
                .iter()
                .all(|server| !server.remember_passwords)
        );
        assert!(
            settings
                .servers
                .iter()
                .all(|server| server.verify_tls_certificates)
        );
    }

    #[test]
    fn migrates_version_three_with_certificate_verification_enabled() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old["version"] = 3.into();
        for server in old["servers"].as_array_mut().unwrap() {
            server
                .as_object_mut()
                .unwrap()
                .remove("verify_tls_certificates");
        }
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.version, SETTINGS_VERSION);
        assert!(
            settings
                .servers
                .iter()
                .all(|server| server.verify_tls_certificates)
        );
    }

    #[test]
    fn migrates_version_four_and_persists_appearance() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old["version"] = 4.into();
        old.as_object_mut().unwrap().remove("appearance");
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.version, SETTINGS_VERSION);
        assert_eq!(settings.appearance, Appearance::default());
        settings.appearance.alternate_rows = true;
        settings.appearance.main_log_background = "#123ABC".into();
        settings.appearance.main_log_font = "Menlo".into();
        settings.appearance.validate().unwrap();
        save_to(&path, &settings).unwrap();
        assert_eq!(load_from(&path).unwrap(), Some(settings));
        assert_eq!(color_value("#123ABC"), Some(0x123abc));
        assert!(color_value("#123ABZ").is_none());
    }

    #[test]
    fn channel_event_color_defaults_for_existing_settings_and_round_trips() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut existing = serde_json::to_value(Settings::default()).unwrap();
        existing["appearance"]
            .as_object_mut()
            .unwrap()
            .remove("channel_event_color");
        fs::write(&path, serde_json::to_vec(&existing).unwrap()).unwrap();

        let mut settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.appearance.channel_event_color, "#007D00");
        settings.appearance.channel_event_color = "#246843".into();
        settings.appearance.validate().unwrap();
        save_to(&path, &settings).unwrap();
        assert_eq!(load_from(&path).unwrap(), Some(settings.clone()));

        settings.appearance.channel_event_color = "green".into();
        assert!(settings.appearance.validate().is_err());
    }

    #[test]
    fn migrates_previous_channel_event_default_without_changing_custom_color() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old["version"] = 8.into();
        old["appearance"]["channel_event_color"] = "#3B7655".into();
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.version, SETTINGS_VERSION);
        assert_eq!(settings.appearance.channel_event_color, "#007D00");

        old["appearance"]["channel_event_color"] = "#246843".into();
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.appearance.channel_event_color, "#246843");

        settings.appearance.channel_event_color = "#3B7655".into();
        save_to(&path, &settings).unwrap();
        assert_eq!(load_from(&path).unwrap(), Some(settings));
    }

    #[test]
    fn startup_connection_is_opt_in_after_version_five_migration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut old = old_default(5);
        old.as_object_mut().unwrap().remove("connect_on_startup");
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();

        let mut settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.version, SETTINGS_VERSION);
        assert!(!settings.selected_profile().unwrap().connect_on_startup);
        settings.selected_profile_mut().unwrap().connect_on_startup = true;
        save_to(&path, &settings).unwrap();
        assert!(
            load_from(&path)
                .unwrap()
                .unwrap()
                .selected_profile()
                .unwrap()
                .connect_on_startup
        );
    }

    #[test]
    fn migrates_version_six_to_system_language_and_persists_choice() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old["version"] = 6.into();
        old.as_object_mut().unwrap().remove("language");
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();

        let mut settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.version, SETTINGS_VERSION);
        assert_eq!(settings.language, Language::System);
        settings.language = Language::English;
        save_to(&path, &settings).unwrap();
        assert_eq!(
            load_from(&path).unwrap().unwrap().language,
            Language::English
        );
    }

    #[test]
    fn migrates_legacy_background_to_member_list_color() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old["version"] = 7.into();
        old["appearance"]
            .as_object_mut()
            .unwrap()
            .remove("member_list_background");
        old["appearance"]["background"] = "#ECECEC".into();
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.appearance.member_list_background, "#FFFFFF");

        old["appearance"]["background"] = "#123ABC".into();
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.appearance.member_list_background, "#123ABC");
        save_to(&path, &settings).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(saved["appearance"]["member_list_background"], "#123ABC");
        assert!(saved["appearance"].get("background").is_none());
    }

    /// Default settings as a version 1–11 file wrote them: the IRCnet
    /// server selected and listed.
    fn old_default(version: u32) -> serde_json::Value {
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old["version"] = version.into();
        old["selected_server"] = IRCNET_ID.into();
        old["servers"] = serde_json::json!([
            {"id": IRCNET_ID, "custom": false, "host": "irc.ircnet.ne.jp",
             "port": 6667, "use_tls": false, "encoding": "utf8"},
            {"id": IRCNET_IPV6_ID, "custom": false, "host": "irc6.ircnet.ne.jp",
             "port": 6667, "use_tls": false, "encoding": "utf8"}
        ]);
        old
    }

    fn version_ten_with_passwords(remember: bool) -> serde_json::Value {
        let mut old = old_default(10);
        old["nickname"] = "alice".into();
        old.as_object_mut().unwrap().remove("username");
        old.as_object_mut().unwrap().remove("credential_backend");
        let server = &mut old["servers"][0];
        server["remember_passwords"] = remember.into();
        server["server_password"] = "server-secret".into();
        server["sasl_password"] = "sasl-secret".into();
        old
    }

    fn memory_stores() -> (
        CredentialStore,
        CredentialStore,
        impl Fn(CredentialBackendKind) -> CredentialStore,
    ) {
        use credentials::MemoryBackend;
        use std::sync::Arc;
        let system = CredentialStore::with_backend(Arc::new(MemoryBackend::new(
            CredentialBackendKind::System,
        )));
        let local = CredentialStore::with_backend(Arc::new(MemoryBackend::new(
            CredentialBackendKind::LocalFile,
        )));
        let (s, l) = (system.clone(), local.clone());
        (system, local, move |kind| match kind {
            CredentialBackendKind::System => s.clone(),
            CredentialBackendKind::LocalFile => l.clone(),
        })
    }

    #[test]
    fn secrets_never_enter_the_settings_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut settings = Settings::default();
        settings.add_server(PRESETS[0].host).remember_passwords = true;
        save_to(&path, &settings).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("server_password"));
        assert!(!text.contains("sasl_password"));

        fs::write(
            &path,
            serde_json::to_vec(&version_ten_with_passwords(true)).unwrap(),
        )
        .unwrap();
        let mut loaded = load_from(&path).unwrap().unwrap();
        assert!(!format!("{loaded:?}").contains("server-secret"));
        save_to(&path, &loaded).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("server-secret"));
        assert!(!text.contains("sasl-secret"));
        assert!(loaded.has_legacy_secrets());
        assert_eq!(loaded.take_legacy_secrets().len(), 2);

        clear_saved_passwords_from(&path, IRCNET_ID).unwrap();
        assert!(
            !load_from(&path)
                .unwrap()
                .unwrap()
                .selected_profile()
                .unwrap()
                .remember_passwords
        );
    }

    #[test]
    fn legacy_plaintext_passwords_move_to_the_credential_store() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(
            &path,
            serde_json::to_vec(&version_ten_with_passwords(true)).unwrap(),
        )
        .unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        assert_eq!(
            settings.selected_profile().unwrap().username,
            "alice",
            "USER keeps the old nickname"
        );
        let (system, local, open) = memory_stores();
        let report = migrate_legacy_secrets_at(&path, &mut settings, &open)
            .unwrap()
            .unwrap();
        assert_eq!(report.moved, 2);
        assert!(!report.used_local_file);
        let key = SecretKey::sasl_password(IRCNET_ID);
        assert_eq!(system.get(&key).unwrap().unwrap().expose(), "sasl-secret");
        assert!(local.get(&key).unwrap().is_none());
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("secret"), "{text}");
        let mut reloaded = load_from(&path).unwrap().unwrap();
        assert!(
            migrate_legacy_secrets_at(&path, &mut reloaded, &open)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn legacy_passwords_use_local_file_when_system_store_is_unavailable() {
        use credentials::MemoryBackend;
        use std::sync::Arc;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(
            &path,
            serde_json::to_vec(&version_ten_with_passwords(true)).unwrap(),
        )
        .unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        let (_, local, _) = memory_stores();
        let local_clone = local.clone();
        let open = move |kind| match kind {
            CredentialBackendKind::System => CredentialStore::with_backend(Arc::new(
                MemoryBackend::unavailable(CredentialBackendKind::System),
            )),
            CredentialBackendKind::LocalFile => local_clone.clone(),
        };
        let report = migrate_legacy_secrets_at(&path, &mut settings, &open)
            .unwrap()
            .unwrap();
        assert!(report.used_local_file);
        assert_eq!(
            settings.credential_backend,
            CredentialBackendKind::LocalFile
        );
        assert_eq!(
            load_from(&path).unwrap().unwrap().credential_backend,
            CredentialBackendKind::LocalFile
        );
        assert_eq!(
            local
                .get(&SecretKey::server_password(IRCNET_ID))
                .unwrap()
                .unwrap()
                .expose(),
            "server-secret"
        );
    }

    #[test]
    fn unsaved_legacy_passwords_are_dropped_and_username_is_independent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(
            &path,
            serde_json::to_vec(&version_ten_with_passwords(false)).unwrap(),
        )
        .unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        assert!(!settings.has_legacy_secrets());
        settings.selected_profile_mut().unwrap().username = "ident".into();
        settings.selected_profile_mut().unwrap().nickname = "bob".into();
        save_to(&path, &settings).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert_eq!(loaded.selected_profile().unwrap().username, "ident");
        assert_eq!(loaded.selected_profile().unwrap().nickname, "bob");
        assert_eq!(loaded.image_upload.provider, None);
        assert_eq!(loaded.credential_backend, CredentialBackendKind::System);
    }

    #[test]
    fn version_nine_gains_theme_dark_colors_and_display_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(
            &path,
            r##"{"version":9,"selected_server":"ircnet","servers":[],"appearance":{"main_log_background":"#FAFAFA"}}"##,
        )
        .unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.version, SETTINGS_VERSION);
        assert_eq!(settings.theme, ThemeMode::System);
        assert_eq!(settings.linux_display, LinuxDisplay::Wayland);
        assert_eq!(settings.appearance.main_log_background, "#FAFAFA");
        assert_eq!(settings.appearance.dark, DarkColors::default());

        settings.theme = ThemeMode::Dark;
        settings.linux_display = LinuxDisplay::X11;
        settings.appearance.dark.main_log_background = "#101010".into();
        save_to(&path, &settings).unwrap();
        assert_eq!(load_from(&path).unwrap(), Some(settings.clone()));

        settings.appearance.dark.sub_log_alternate = "gray".into();
        assert!(settings.appearance.validate().is_err());
    }

    #[test]
    fn channel_number_modifier_defaults_to_ctrl_and_round_trips() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(
            &path,
            r#"{"version":10,"selected_server":"ircnet","servers":[]}"#,
        )
        .unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        assert_eq!(
            settings.channel_number_modifier,
            ChannelNumberModifier::Ctrl
        );

        assert_eq!(settings.text_key_theme, TextKeyTheme::Auto);

        settings.channel_number_modifier = ChannelNumberModifier::Super;
        settings.text_key_theme = TextKeyTheme::Emacs;
        save_to(&path, &settings).unwrap();
        assert_eq!(load_from(&path).unwrap(), Some(settings));
    }
}
