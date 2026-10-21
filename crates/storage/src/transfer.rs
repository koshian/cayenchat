// SPDX-License-Identifier: MPL-2.0
//! Exporting the settings to a file of the user's choice and importing them
//! again (#520, D059).
//!
//! The file is the versioned `settings.json` format. Secrets are not in it:
//! passwords and upload tokens live in the credential store, and the legacy
//! plaintext fields are never written. An import replaces the whole
//! configuration, but never carries credentials: every imported server gets
//! a fresh profile id, so no stored credential of this machine can be taken
//! for the imported server's.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use crate::{ConversationLog, Experimental, Settings, parse_settings, private_file, save_to};

/// Largest file an import reads. A real settings file is a few kilobytes.
pub const MAX_IMPORT_BYTES: u64 = 1024 * 1024;
/// Most servers an import accepts.
pub const MAX_IMPORT_SERVERS: usize = 256;

/// A settings file read for import, ready to replace the current settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    /// The imported settings with fresh profile ids and this machine's own
    /// credential backend, diagnostics file and log folder.
    pub settings: Settings,
    /// The `version` the file declared, before migration.
    pub source_version: u32,
    /// The file named a log folder or a diagnostics file of the machine it
    /// came from; neither is imported.
    pub dropped_local_paths: bool,
}

/// Writes `settings` as a copy to `path`, replacing a file there whole or
/// not at all. The current settings file is not touched.
pub fn export_to(path: &Path, settings: &Settings) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(&settings.clone().normalize())
        .map_err(|error| format!("Could not serialize settings: {error}"))?;
    private_file::write(path, &bytes, private_file::Flush::Skip)
        .map_err(|error| format!("Could not write the exported settings: {error}"))
}

/// Reads and checks the file at `path` without changing anything: it must be
/// a regular file of at most [`MAX_IMPORT_BYTES`], a settings file of a
/// supported version (older ones are migrated, newer ones refused) with its
/// server list, and hold valid values.
pub fn read_import(path: &Path) -> Result<Import, String> {
    let file = fs::File::open(path).map_err(|error| format!("Could not read the file: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("Could not read the file: {error}"))?;
    if !metadata.is_file() {
        return Err("The chosen item is not a file.".into());
    }
    if metadata.len() > MAX_IMPORT_BYTES {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    file.take(MAX_IMPORT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Could not read the file: {error}"))?;
    if bytes.len() as u64 > MAX_IMPORT_BYTES {
        return Err(too_large());
    }
    import_from_bytes(&bytes)
}

fn too_large() -> String {
    format!(
        "The file is larger than {} KiB, so it is not a settings file.",
        MAX_IMPORT_BYTES / 1024
    )
}

fn import_from_bytes(bytes: &[u8]) -> Result<Import, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("Could not parse settings: {error}"))?;
    let Some(object) = value.as_object() else {
        return Err("The file is not a CayenChat settings file.".into());
    };
    let source_version = object
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .unwrap_or(0);
    // Version 1 has a single server in flat fields. Later ones list servers;
    // a file without the list would otherwise read as "no servers" and
    // empty the configuration.
    if source_version >= 2
        && !object
            .get("servers")
            .is_some_and(serde_json::Value::is_array)
    {
        return Err("The file has no server list, so it is not a CayenChat settings file.".into());
    }
    if source_version >= 2 {
        check_servers(object)?;
    }
    let mut settings = parse_settings(bytes, "The current settings were left unchanged.")?;
    if settings.servers.len() > MAX_IMPORT_SERVERS {
        return Err(format!(
            "The file lists more than {MAX_IMPORT_SERVERS} servers."
        ));
    }
    settings
        .appearance
        .validate()
        .map_err(|error| format!("The file holds an invalid appearance setting: {error}"))?;
    // Passwords of old versions are never carried over.
    drop(settings.take_legacy_secrets());
    let dropped_local_paths = settings.experimental != Experimental::default()
        || settings.conversation_log.directory.is_some();
    settings.experimental = Experimental::default();
    settings.conversation_log = ConversationLog {
        directory: None,
        ..settings.conversation_log
    };
    let ids = settings
        .servers
        .iter()
        .map(|server| (server.id.clone(), fresh_id()))
        .collect::<Vec<_>>();
    for (server, (_, id)) in settings.servers.iter_mut().zip(&ids) {
        server.id = id.clone();
    }
    if let Some((_, id)) = ids.iter().find(|(old, _)| *old == settings.selected_server) {
        settings.selected_server = id.clone();
    }
    Ok(Import {
        settings,
        source_version,
        dropped_local_paths,
    })
}

/// Refuses a server list that loading would quietly repair by dropping
/// servers (no id, a repeated id, no host, port 0): an import must not
/// restore less than the file lists.
fn check_servers(object: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    let Some(servers) = object.get("servers").and_then(serde_json::Value::as_array) else {
        return Ok(());
    };
    let mut ids = std::collections::HashSet::new();
    for (index, server) in servers.iter().enumerate() {
        let number = index + 1;
        let text = |key| server.get(key).and_then(serde_json::Value::as_str);
        let id = text("id").unwrap_or_default();
        if id.is_empty() {
            return Err(format!("Server {number} has no id."));
        }
        if !ids.insert(id) {
            return Err(format!("Server {number} repeats the id \"{id}\"."));
        }
        if text("host").is_none_or(|host| host.trim().is_empty()) {
            return Err(format!("Server {number} has no host."));
        }
        if server.get("port").and_then(serde_json::Value::as_u64) == Some(0) {
            return Err(format!("Server {number} has port 0."));
        }
    }
    Ok(())
}

fn fresh_id() -> String {
    format!("custom-{}", uuid::Uuid::new_v4())
}

impl Import {
    /// The settings to save: the imported ones, except what belongs to this
    /// machine and stays as `current` has it.
    pub fn into_settings(self, current: Option<&Settings>) -> Settings {
        let mut settings = self.settings;
        if let Some(current) = current {
            settings.credential_backend = current.credential_backend;
            settings.experimental = current.experimental.clone();
            settings.conversation_log.directory = current.conversation_log.directory.clone();
        }
        settings
    }
}

/// Replaces the settings file at `path` with `settings`. A file that exists
/// is first copied beside it (`settings-backup-<milliseconds>.json`, user
/// only) and the copy's path is returned; the new file then replaces it
/// whole or not at all. When the backup or the replacement fails, the
/// current file is as it was.
pub fn replace_settings_at(path: &Path, settings: &Settings) -> Result<Option<PathBuf>, String> {
    let backup = match fs::read(path) {
        Ok(bytes) => {
            let millis = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_millis());
            let backup = path.with_file_name(format!("settings-backup-{millis}.json"));
            private_file::write(&backup, &bytes, private_file::Flush::Disk)
                .map_err(|error| format!("Could not back up the current settings: {error}"))?;
            Some(backup)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("Could not read the current settings: {error}")),
    };
    save_to(path, settings)?;
    Ok(backup)
}

/// [`replace_settings_at`] for the settings file in use.
pub fn replace_settings(settings: &Settings) -> Result<Option<PathBuf>, String> {
    replace_settings_at(&crate::settings_path()?, settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CredentialBackendKind, SETTINGS_VERSION, load_from};

    fn directory(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "cayenchat-transfer-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn sample() -> Settings {
        let mut settings = Settings::default();
        let server = settings.add_server("irc.example.org");
        server.nickname = "alice".into();
        server.channels = "#a,-#b".into();
        server.connect_on_startup = true;
        server.remember_passwords = true;
        settings.theme = crate::ThemeMode::Dark;
        settings.keybindings.insert("x".into(), "cmd-x".into());
        settings
    }

    #[test]
    fn an_export_imports_back_with_fresh_server_ids() {
        let directory = directory("round-trip");
        let path = directory.join("cayenchat-settings.json");
        let original = sample();
        export_to(&path, &original).unwrap();
        let import = read_import(&path).unwrap();
        assert_eq!(import.source_version, SETTINGS_VERSION);
        let imported = import.settings;
        let (before, after) = (&original.servers[0], &imported.servers[0]);
        assert_ne!(before.id, after.id);
        assert!(after.id.starts_with("custom-"));
        assert_eq!(imported.selected_server, after.id);
        let mut expected = original.clone();
        expected.servers[0].id = after.id.clone();
        expected.selected_server = after.id.clone();
        assert_eq!(imported, expected);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn an_export_holds_no_secret() {
        let directory = directory("secrets");
        let path = directory.join("export.json");
        let value = serde_json::json!({
            "version": 10,
            "selected_server": "custom-1",
            "servers": [{
                "id": "custom-1", "host": "irc.example.org", "port": 6697,
                "use_tls": true, "encoding": "utf8", "remember_passwords": true,
                "server_password": "hunter2-server", "sasl_password": "hunter2-sasl"
            }]
        });
        fs::write(&path, value.to_string()).unwrap();
        let import = read_import(&path).unwrap();
        assert!(!import.settings.has_legacy_secrets());
        export_to(&path, &import.settings).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("hunter2"));
        assert!(!text.contains("password\""));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn older_versions_are_migrated_and_newer_ones_refused() {
        let old = serde_json::json!({
            "version": 1, "server": "custom", "custom_host": "irc.example.org",
            "port": 6667, "use_tls": false, "nickname": "bob", "channels": "#c"
        });
        let import = import_from_bytes(old.to_string().as_bytes()).unwrap();
        assert_eq!(import.source_version, 1);
        assert_eq!(import.settings.servers[0].nickname, "bob");
        assert_eq!(import.settings.version, SETTINGS_VERSION);

        let newer = serde_json::json!({ "version": SETTINGS_VERSION + 1, "servers": [] });
        let error = import_from_bytes(newer.to_string().as_bytes()).unwrap_err();
        assert!(error.contains("newer"), "{error}");
        assert!(error.contains("left unchanged"), "{error}");
    }

    #[test]
    fn files_that_are_not_settings_are_refused() {
        for bytes in [
            &b"not json"[..],
            b"[]",
            b"{}",
            b"{\"version\": 15}",
            b"{\"version\": 15, \"servers\": 3}",
            b"{\"version\": \"15\", \"servers\": []}",
            b"{\"version\": 15, \"servers\": [{\"id\": 1}]}",
        ] {
            assert!(
                import_from_bytes(bytes).is_err(),
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
        let directory = directory("refused");
        assert!(read_import(&directory).is_err());
        assert!(read_import(&directory.join("missing.json")).is_err());
        let big = directory.join("big.json");
        fs::write(&big, vec![b' '; MAX_IMPORT_BYTES as usize + 1]).unwrap();
        assert!(read_import(&big).unwrap_err().contains("larger"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_server_list_that_loading_would_trim_is_refused() {
        let server = |id: &str, host: &str, port: u16| serde_json::json!({ "id": id, "host": host, "port": port });
        let lists = [
            vec![server("a", "irc.example.org", 0)],
            vec![
                server("a", "irc.example.org", 6667),
                server("a", "b.example", 6667),
            ],
            vec![server("", "irc.example.org", 6667)],
            vec![server("a", "  ", 6667)],
        ];
        for servers in lists {
            let value = serde_json::json!({
                "version": SETTINGS_VERSION, "selected_server": "a", "servers": servers
            });
            let error = import_from_bytes(value.to_string().as_bytes()).unwrap_err();
            assert!(error.starts_with("Server "), "{error}");
        }
    }

    #[test]
    fn machine_specific_values_are_not_imported() {
        let mut other = sample();
        other.credential_backend = CredentialBackendKind::LocalFile;
        other.experimental = Experimental {
            debug_logging: true,
            stderr_file: Some(PathBuf::from("/other/debug.log")),
        };
        other.conversation_log.directory = Some(PathBuf::from("C:\\logs"));
        other.conversation_log.channels = true;
        let import = import_from_bytes(&serde_json::to_vec(&other).unwrap()).unwrap();
        assert!(import.dropped_local_paths);
        let mut current = Settings::default();
        current.experimental.debug_logging = true;
        current.conversation_log.directory = Some(PathBuf::from("/home/me/logs"));
        let settings = import.into_settings(Some(&current));
        assert_eq!(settings.credential_backend, current.credential_backend);
        assert_eq!(settings.experimental, current.experimental);
        assert_eq!(
            settings.conversation_log.directory,
            current.conversation_log.directory
        );
        // The choice to log stays; where to is this machine's.
        assert!(settings.conversation_log.channels);
    }

    #[test]
    fn replacing_backs_up_the_current_file_and_a_failure_keeps_it() {
        let directory = directory("replace");
        let path = directory.join("settings.json");
        let current = sample();
        save_to(&path, &current).unwrap();
        let before = fs::read(&path).unwrap();

        let replacement = Settings {
            theme: crate::ThemeMode::Light,
            ..Settings::default()
        };
        let backup = replace_settings_at(&path, &replacement).unwrap().unwrap();
        assert_eq!(fs::read(&backup).unwrap(), before);
        assert_eq!(
            load_from(&path).unwrap().unwrap().theme,
            crate::ThemeMode::Light
        );
        assert_eq!(
            load_from(&backup).unwrap().unwrap(),
            current.clone().normalize()
        );

        // No file yet: nothing to back up.
        let fresh = directory.join("fresh").join("settings.json");
        assert_eq!(replace_settings_at(&fresh, &replacement).unwrap(), None);

        // A directory where the file should be cannot be replaced; the
        // settings stay whole.
        let blocked = directory.join("blocked");
        fs::create_dir_all(blocked.join("settings.json")).unwrap();
        assert!(replace_settings_at(&blocked.join("settings.json"), &replacement).is_err());
        fs::remove_dir_all(directory).unwrap();
    }
}
