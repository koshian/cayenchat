//! Versioned connection preferences. Credentials are optional plaintext values.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

const SETTINGS_VERSION: u32 = 4;
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
pub struct ServerProfile {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub use_tls: bool,
    #[serde(default = "default_verify_tls_certificates")]
    pub verify_tls_certificates: bool,
    pub encoding: TextEncoding,
    pub custom: bool,
    #[serde(default)]
    pub remember_passwords: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sasl_password: Option<String>,
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
            server_password: None,
            sasl_password: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub selected_server: String,
    pub servers: Vec<ServerProfile>,
    pub nickname: String,
    pub channels: String,
    pub sasl_enabled: bool,
    pub sasl_username: String,
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
            channels: String::new(),
            sasl_enabled: false,
            sasl_username: String::new(),
        }
    }
}

impl Settings {
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
            server_password: None,
            sasl_password: None,
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
                server.server_password = None;
                server.sasl_password = None;
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

pub fn clear_saved_passwords(server_id: &str) -> Result<(), String> {
    let path = settings_path()?;
    clear_saved_passwords_from(&path, server_id)
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
        server.server_password = None;
        server.sasl_password = None;
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
        Some(2..=4) => serde_json::from_value::<Settings>(value)
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
        assert_eq!(settings.version, 4);
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
        assert_eq!(settings.version, 4);
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
        assert_eq!(settings.version, 4);
        assert!(
            settings
                .servers
                .iter()
                .all(|server| server.verify_tls_certificates)
        );
    }

    #[test]
    fn saved_passwords_are_opt_in_and_cleared_immediately() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut settings = Settings::default();
        let server = settings.selected_profile_mut();
        server.server_password = Some("server-secret".into());
        server.sasl_password = Some("sasl-secret".into());
        save_to(&path, &settings).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("server-secret"));
        assert!(!text.contains("sasl-secret"));

        settings.selected_profile_mut().remember_passwords = true;
        save_to(&path, &settings).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("server-secret"));
        assert!(text.contains("sasl-secret"));

        clear_saved_passwords_from(&path, IRCNET_ID).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("server-secret"));
        assert!(!text.contains("sasl-secret"));
        assert!(
            !load_from(&path)
                .unwrap()
                .unwrap()
                .selected_profile()
                .remember_passwords
        );
    }
}
