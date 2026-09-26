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

const SETTINGS_VERSION: u32 = 11;
pub const IRCNET_ID: &str = "ircnet";
pub const IRCNET_IPV6_ID: &str = "ircnet-ipv6";

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
    pub custom: bool,
    /// Keep this profile's server and SASL passwords in the credential store.
    #[serde(default)]
    pub remember_passwords: bool,
    /// Plaintext passwords saved by version 10 and earlier. Read only for
    /// migration into the credential store; never written back.
    #[serde(rename = "server_password", default, skip_serializing)]
    legacy_server_password: Option<Secret>,
    #[serde(rename = "sasl_password", default, skip_serializing)]
    legacy_sasl_password: Option<Secret>,
}

impl ServerProfile {
    fn preset(id: &str, host: &str) -> Self {
        Self {
            id: id.into(),
            host: host.into(),
            port: 6667,
            use_tls: false,
            verify_tls_certificates: true,
            encoding: TextEncoding::Utf8,
            custom: false,
            remember_passwords: false,
            legacy_server_password: None,
            legacy_sasl_password: None,
        }
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
    pub nickname: String,
    /// IRC `USER` username (ident); independent of the nickname.
    pub username: String,
    pub channels: String,
    pub sasl_enabled: bool,
    /// SASL account name; independent of both nickname and username.
    pub sasl_username: String,
    pub connect_on_startup: bool,
    pub language: Language,
    pub theme: ThemeMode,
    pub linux_display: LinuxDisplay,
    pub channel_number_modifier: ChannelNumberModifier,
    pub text_key_theme: TextKeyTheme,
    pub appearance: Appearance,
    pub credential_backend: CredentialBackendKind,
    pub image_upload: ImageUpload,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            selected_server: IRCNET_ID.into(),
            servers: vec![
                ServerProfile::preset(IRCNET_ID, "irc.ircnet.ne.jp"),
                ServerProfile::preset(IRCNET_IPV6_ID, "irc6.ircnet.ne.jp"),
            ],
            nickname: String::new(),
            username: String::new(),
            channels: String::new(),
            sasl_enabled: false,
            sasl_username: String::new(),
            connect_on_startup: false,
            language: Language::System,
            theme: ThemeMode::System,
            linux_display: LinuxDisplay::Wayland,
            channel_number_modifier: ChannelNumberModifier::Ctrl,
            text_key_theme: TextKeyTheme::Auto,
            appearance: Appearance::default(),
            credential_backend: CredentialBackendKind::System,
            image_upload: ImageUpload::default(),
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

    pub fn selected_profile(&self) -> &ServerProfile {
        self.servers
            .iter()
            .find(|s| s.id == self.selected_server)
            .expect("settings must contain the selected server")
    }

    pub fn selected_profile_mut(&mut self) -> &mut ServerProfile {
        self.servers
            .iter_mut()
            .find(|s| s.id == self.selected_server)
            .expect("settings must contain the selected server")
    }

    pub fn ordered_servers(&self) -> impl Iterator<Item = &ServerProfile> {
        self.servers
            .iter()
            .filter(|s| s.custom)
            .chain(self.servers.iter().filter(|s| !s.custom))
    }

    pub fn add_custom_server(&mut self) {
        let mut number = 1;
        let id = loop {
            let id = format!("custom-{number}");
            if !self.servers.iter().any(|s| s.id == id) {
                break id;
            }
            number += 1;
        };
        self.selected_server = id.clone();
        self.servers.push(ServerProfile {
            id,
            host: String::new(),
            port: 6667,
            use_tls: false,
            verify_tls_certificates: true,
            encoding: TextEncoding::Utf8,
            custom: true,
            remember_passwords: false,
            legacy_server_password: None,
            legacy_sasl_password: None,
        });
    }

    pub fn remove_selected_custom_server(&mut self) {
        self.servers
            .retain(|s| !s.custom || s.id != self.selected_server);
        let next = self
            .ordered_servers()
            .next()
            .map(|s| s.id.clone())
            .unwrap_or_else(|| IRCNET_ID.into());
        self.selected_server = next;
    }

    pub fn channels(&self) -> Vec<String> {
        self.channels
            .split(',')
            .map(str::trim)
            .filter(|channel| !channel.is_empty())
            .map(str::to_owned)
            .collect()
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
        if self.version <= 10 && self.username.is_empty() {
            // Earlier versions sent the nickname as the USER username; keep
            // that as the initial value so existing connections do not change.
            self.username = self.nickname.clone();
        }
        self.version = SETTINGS_VERSION;
        let mut seen = HashSet::new();
        self.servers
            .retain(|s| !s.id.is_empty() && seen.insert(s.id.clone()) && s.port != 0);
        for (id, host) in [
            (IRCNET_ID, "irc.ircnet.ne.jp"),
            (IRCNET_IPV6_ID, "irc6.ircnet.ne.jp"),
        ] {
            match self.servers.iter_mut().find(|s| s.id == id) {
                Some(s) => {
                    s.host = host.into();
                    s.custom = false;
                }
                None => self.servers.push(ServerProfile::preset(id, host)),
            }
        }
        self.servers
            .retain(|s| !s.custom || !s.host.trim().is_empty());
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
            self.selected_server = IRCNET_ID.into();
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
            nickname: old.nickname,
            channels: old.channels,
            sasl_enabled: old.sasl_enabled,
            sasl_username: old.sasl_username,
            ..Settings::default()
        };
        match old.server {
            OldServerChoice::Ircnet => settings.selected_server = IRCNET_ID.into(),
            OldServerChoice::IrcnetIpv6 => settings.selected_server = IRCNET_IPV6_ID.into(),
            OldServerChoice::Custom => {
                settings.add_custom_server();
                settings.selected_profile_mut().host = old.custom_host;
            }
        }
        let selected = settings.selected_profile_mut();
        selected.port = old.port;
        selected.use_tls = old.use_tls;
        settings.normalize()
    }
}

pub fn settings_path() -> Result<PathBuf, String> {
    let directory = dirs::config_dir().ok_or("Could not find the user configuration directory.")?;
    Ok(directory.join("CayenChat").join("settings.json"))
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
        Some(2..=11) => serde_json::from_value::<Settings>(value)
            .map(Settings::normalize)
            .map_err(|error| format!("Could not parse settings: {error}"))?,
        _ => return Err(format!("Unsupported settings version: {version:?}")),
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
        settings.add_custom_server();
        settings.selected_profile_mut().host = "irc.example.net".into();
        settings.selected_profile_mut().port = 6697;
        settings.selected_profile_mut().use_tls = true;
        settings.selected_profile_mut().verify_tls_certificates = false;
        settings.selected_profile_mut().encoding = TextEncoding::Iso2022Jp;
        settings.nickname = "alice".into();
        settings.channels = "#one, #two".into();
        settings.sasl_enabled = true;
        settings.sasl_username = "account".into();
        save_to(&path, &settings).unwrap();
        assert_eq!(load_from(&path).unwrap(), Some(settings.clone()));
        assert_eq!(settings.channels(), ["#one", "#two"]);
        assert_eq!(settings.selected_profile().host, "irc.example.net");
        assert_eq!(
            settings.ordered_servers().next().unwrap().host,
            "irc.example.net"
        );
        let file_text = fs::read_to_string(path).unwrap();
        assert!(!file_text.contains("server_password"));
        assert!(!file_text.contains("sasl_password"));
    }

    #[test]
    fn migrates_version_one_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(&path, r##"{"version":1,"server":"custom","custom_host":"irc.example.net","port":6697,"use_tls":true,"nickname":"alice","channels":"#日本語","sasl_enabled":false,"sasl_username":""}"##).unwrap();
        let settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.version, 11);
        assert_eq!(settings.selected_profile().host, "irc.example.net");
        assert_eq!(settings.selected_profile().port, 6697);
        assert!(settings.selected_profile().verify_tls_certificates);
        assert_eq!(settings.channels(), ["#日本語"]);
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
        assert_eq!(settings.version, 11);
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
        assert_eq!(settings.version, 11);
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
        assert_eq!(settings.version, 11);
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
        assert_eq!(settings.version, 11);
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
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old["version"] = 5.into();
        old.as_object_mut().unwrap().remove("connect_on_startup");
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();

        let mut settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.version, 11);
        assert!(!settings.connect_on_startup);
        settings.connect_on_startup = true;
        save_to(&path, &settings).unwrap();
        assert!(load_from(&path).unwrap().unwrap().connect_on_startup);
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
        assert_eq!(settings.version, 11);
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

    fn version_ten_with_passwords(remember: bool) -> serde_json::Value {
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old["version"] = 10.into();
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
        settings.selected_profile_mut().remember_passwords = true;
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
        assert_eq!(settings.username, "alice", "USER keeps the old nickname");
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
        settings.username = "ident".into();
        settings.nickname = "bob".into();
        save_to(&path, &settings).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert_eq!(loaded.username, "ident");
        assert_eq!(loaded.nickname, "bob");
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
        assert_eq!(settings.version, 11);
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
