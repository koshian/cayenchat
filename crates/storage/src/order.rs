// SPDX-License-Identifier: MPL-2.0
//! The channel order the user chose in the channel tree.
//!
//! Like the window layout, this is UI state in its own file beside the
//! settings, so changing it never races the settings window's autosave and
//! damage to it can never stop the application from starting: a missing,
//! unreadable or unknown file reads as "nothing saved". Orders are keyed by
//! server profile ID and hold channel names compared ASCII case-insensitively
//! (stored lowercased), including channels that are currently parted, so a
//! channel joined again returns to its place.
//!
//! The channel groups (D075) are saved here too: display-only parents that
//! hold channels of several servers. A grouped channel is named by its server
//! profile ID and lowercased name, never by a connection or conversation.

use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

const ORDER_VERSION: u32 = 1;
/// Servers and names per server kept; anything beyond is damage.
const MAX_SERVERS: usize = 256;
const MAX_NAMES: usize = 2_000;
const MAX_NAME_BYTES: usize = 512;
pub const MAX_GROUPS: usize = 64;
pub const MAX_GROUP_MEMBERS: usize = 2_000;
/// The longest group name, in characters. `app` keeps the same three limits;
/// `storage` does not depend on it, so a `ui` test keeps them equal.
pub const MAX_GROUP_NAME_CHARS: usize = 64;
/// Larger files are damage and are not parsed.
const MAX_FILE_BYTES: u64 = 1 << 20;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelOrders {
    pub version: u32,
    /// Server profile ID to the channels in tree order.
    pub servers: BTreeMap<String, Vec<String>>,
    /// The channel groups in tree order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<StoredGroup>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoredGroup {
    pub name: String,
    pub collapsed: bool,
    pub members: Vec<StoredMember>,
}

/// A channel of a group: server profile ID and lowercased channel name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoredMember {
    pub server: String,
    pub channel: String,
}

impl ChannelOrders {
    /// The key a channel is stored under.
    pub fn key(name: &str) -> String {
        name.to_ascii_lowercase()
    }

    pub fn order(&self, profile_id: &str) -> &[String] {
        self.servers.get(profile_id).map_or(&[], Vec::as_slice)
    }

    /// Replaces one server's order. An empty order removes the entry.
    pub fn set(&mut self, profile_id: &str, order: Vec<String>) {
        if order.is_empty() {
            self.servers.remove(profile_id);
        } else {
            self.servers.insert(profile_id.to_owned(), order);
        }
    }

    /// Drops anything oversized, empty or repeated.
    fn sanitized(mut self) -> Self {
        let mut servers = std::mem::take(&mut self.servers);
        while servers.len() > MAX_SERVERS {
            servers.pop_last();
        }
        for names in servers.values_mut() {
            let mut seen = std::collections::HashSet::new();
            names.retain(|name| {
                !name.is_empty() && name.len() <= MAX_NAME_BYTES && seen.insert(name.clone())
            });
            names.truncate(MAX_NAMES);
        }
        servers.retain(|_, names| !names.is_empty());
        self.servers = servers;
        // A name is kept trimmed; one that is empty, too long or holds a
        // control character is damage and the group is dropped, not shortened.
        self.groups.retain_mut(|group| {
            let name = group.name.trim();
            let usable = !name.is_empty()
                && name.chars().count() <= MAX_GROUP_NAME_CHARS
                && !name.chars().any(char::is_control);
            if usable && name.len() != group.name.len() {
                group.name = name.to_owned();
            }
            usable
        });
        self.groups.truncate(MAX_GROUPS);
        let mut seen = std::collections::HashSet::new();
        for group in &mut self.groups {
            group.members.retain(|member| {
                !member.channel.is_empty()
                    && member.channel.len() <= MAX_NAME_BYTES
                    && member.server.len() <= MAX_NAME_BYTES
                    && seen.insert((member.server.clone(), member.channel.clone()))
            });
            group.members.truncate(MAX_GROUP_MEMBERS);
        }
        self
    }

    /// Forgets servers that are no longer configured.
    pub fn retain_servers(&mut self, keep: impl Fn(&str) -> bool) {
        self.servers.retain(|id, _| keep(id));
        for group in &mut self.groups {
            group.members.retain(|member| keep(&member.server));
        }
    }
}

/// `channel-order.json` beside the settings file.
pub fn order_path() -> Result<PathBuf, String> {
    Ok(crate::settings_path()?.with_file_name("channel-order.json"))
}

/// The saved orders, or none when the file is missing or cannot be used.
pub fn load_orders() -> ChannelOrders {
    order_path()
        .map(|path| load_orders_from(&path))
        .unwrap_or_default()
}

pub fn load_orders_from(path: &Path) -> ChannelOrders {
    // Read at most one byte past the limit so a huge file is refused
    // without being held in memory.
    let mut bytes = Vec::new();
    let read =
        fs::File::open(path).and_then(|file| file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes));
    if read.is_err() || bytes.len() as u64 > MAX_FILE_BYTES {
        return ChannelOrders::default();
    }
    match serde_json::from_slice::<ChannelOrders>(&bytes) {
        Ok(orders) if orders.version == ORDER_VERSION => orders.sanitized(),
        _ => ChannelOrders::default(),
    }
}

pub fn save_orders(orders: &ChannelOrders) -> Result<(), String> {
    save_orders_to(&order_path()?, orders)
}

/// Writes the file whole or not at all, keeping it readable only by the user.
pub fn save_orders_to(path: &Path, orders: &ChannelOrders) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(&ChannelOrders {
        version: ORDER_VERSION,
        ..orders.clone()
    })
    .map_err(|error| format!("Could not serialize the channel order: {error}"))?;
    crate::private_file::write(path, &bytes, crate::private_file::Flush::Skip)
        .map_err(|error| format!("Could not save the channel order: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_order_reads_back_and_damage_reads_as_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("channel-order.json");
        let mut orders = ChannelOrders::default();
        orders.set("a", vec!["#b".into(), "#a".into()]);
        save_orders_to(&path, &orders).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(path.parent().unwrap()), 0o700);
        }
        assert_eq!(load_orders_from(&path).order("a"), ["#b", "#a"]);
        assert!(load_orders_from(&path).order("missing").is_empty());
        fs::write(&path, b"{ not json").unwrap();
        assert_eq!(load_orders_from(&path), ChannelOrders::default());
        fs::write(&path, br##"{"version":99,"servers":{"a":["#x"]}}"##).unwrap();
        assert_eq!(load_orders_from(&path), ChannelOrders::default());
        fs::write(&path, br##"{"version":1,"servers":{"a":["#x","#x",""]}}"##).unwrap();
        assert_eq!(load_orders_from(&path).order("a"), ["#x"]);
    }

    #[test]
    fn groups_read_back_and_damage_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channel-order.json");
        let member = |server: &str, channel: &str| StoredMember {
            server: server.into(),
            channel: channel.into(),
        };
        let group = |name: &str, members| StoredGroup {
            name: name.into(),
            collapsed: true,
            members,
        };
        let orders = ChannelOrders {
            groups: vec![
                group(
                    "daily",
                    vec![member("a", "#x"), member("b", "#y"), member("a", "#x")],
                ),
                group("  ", vec![member("a", "#z")]),
                group("other", vec![member("a", "#x"), member("a", "")]),
            ],
            ..Default::default()
        };
        save_orders_to(&path, &orders).unwrap();
        let loaded = load_orders_from(&path);
        assert_eq!(loaded.groups.len(), 2);
        assert_eq!(
            loaded.groups[0].members,
            [member("a", "#x"), member("b", "#y")]
        );
        assert!(loaded.groups[0].collapsed);
        assert!(loaded.groups[1].members.is_empty());
        let mut loaded = loaded;
        loaded.retain_servers(|id| id == "a");
        assert_eq!(loaded.groups[0].members, [member("a", "#x")]);
        // A file without groups, as older versions wrote, still reads.
        fs::write(&path, br##"{"version":1,"servers":{"a":["#x"]}}"##).unwrap();
        assert!(load_orders_from(&path).groups.is_empty());
    }

    #[test]
    fn group_names_and_counts_are_bounded_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channel-order.json");
        let group = |name: String| StoredGroup {
            name,
            collapsed: false,
            members: Vec::new(),
        };
        let mut groups = vec![group("x".repeat(MAX_GROUP_NAME_CHARS + 1))];
        groups.extend((0..MAX_GROUPS + 5).map(|n| group(format!("g{n}"))));
        let orders = ChannelOrders {
            groups,
            ..Default::default()
        };
        save_orders_to(&path, &orders).unwrap();
        let loaded = load_orders_from(&path);
        // The long name is dropped, not shortened.
        assert_eq!(loaded.groups.len(), MAX_GROUPS);
        assert_eq!(loaded.groups[0].name, "g0");
        // Names are kept trimmed, and one with a control character is dropped.
        let orders = ChannelOrders {
            groups: vec![group("  spaced ".into()), group("bell\u{7}".into())],
            ..Default::default()
        };
        save_orders_to(&path, &orders).unwrap();
        let loaded = load_orders_from(&path);
        assert_eq!(loaded.groups.len(), 1);
        assert_eq!(loaded.groups[0].name, "spaced");
    }

    #[test]
    fn a_file_over_the_size_limit_is_not_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channel-order.json");
        let names: Vec<String> = (0..MAX_FILE_BYTES as usize / 8)
            .map(|n| format!("#{n}"))
            .collect();
        let text = format!(r#"{{"version":1,"servers":{{"a":{:?}}}}}"#, names);
        assert!(text.len() as u64 > MAX_FILE_BYTES);
        fs::write(&path, text).unwrap();
        assert_eq!(load_orders_from(&path), ChannelOrders::default());
    }

    #[cfg(unix)]
    #[test]
    fn saving_replaces_a_symlink_without_following_it() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.json");
        let path = dir.path().join("channel-order.json");
        fs::write(&target, b"target remains unchanged").unwrap();
        symlink(&target, &path).unwrap();
        let mut orders = ChannelOrders::default();
        orders.set("a", vec!["#private".into()]);

        save_orders_to(&path, &orders).unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"target remains unchanged");
        assert!(
            !fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(load_orders_from(&path).order("a"), ["#private"]);
    }
}
