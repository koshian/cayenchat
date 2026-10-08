//! The channel order the user chose in the channel tree.
//!
//! Like the window layout, this is UI state in its own file beside the
//! settings, so changing it never races the settings window's autosave and
//! damage to it can never stop the application from starting: a missing,
//! unreadable or unknown file reads as "nothing saved". Orders are keyed by
//! server profile ID and hold channel names compared ASCII case-insensitively
//! (stored lowercased), including channels that are currently parted, so a
//! channel joined again returns to its place.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

const ORDER_VERSION: u32 = 1;
/// Servers and names per server kept; anything beyond is damage.
const MAX_SERVERS: usize = 256;
const MAX_NAMES: usize = 2_000;
const MAX_NAME_BYTES: usize = 512;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelOrders {
    pub version: u32,
    /// Server profile ID to the channels in tree order.
    pub servers: BTreeMap<String, Vec<String>>,
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
        self
    }

    /// Forgets servers that are no longer configured.
    pub fn retain_servers(&mut self, keep: impl Fn(&str) -> bool) {
        self.servers.retain(|id, _| keep(id));
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
    let Ok(bytes) = fs::read(path) else {
        return ChannelOrders::default();
    };
    match serde_json::from_slice::<ChannelOrders>(&bytes) {
        Ok(orders) if orders.version == ORDER_VERSION => orders.sanitized(),
        _ => ChannelOrders::default(),
    }
}

pub fn save_orders(orders: &ChannelOrders) -> Result<(), String> {
    save_orders_to(&order_path()?, orders)
}

/// Writes the file whole or not at all (a temporary file, then a rename).
pub fn save_orders_to(path: &Path, orders: &ChannelOrders) -> Result<(), String> {
    let parent = path.parent().ok_or("The order path has no directory.")?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Could not create the order directory: {error}"))?;
    let bytes = serde_json::to_vec_pretty(&ChannelOrders {
        version: ORDER_VERSION,
        ..orders.clone()
    })
    .map_err(|error| format!("Could not serialize the channel order: {error}"))?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes)
        .map_err(|error| format!("Could not write the channel order: {error}"))?;
    fs::rename(&temporary, path)
        .map_err(|error| format!("Could not save the channel order: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_order_reads_back_and_damage_reads_as_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channel-order.json");
        let mut orders = ChannelOrders::default();
        orders.set("a", vec!["#b".into(), "#a".into()]);
        save_orders_to(&path, &orders).unwrap();
        assert_eq!(load_orders_from(&path).order("a"), ["#b", "#a"]);
        assert!(load_orders_from(&path).order("missing").is_empty());
        fs::write(&path, b"{ not json").unwrap();
        assert_eq!(load_orders_from(&path), ChannelOrders::default());
        fs::write(&path, br##"{"version":99,"servers":{"a":["#x"]}}"##).unwrap();
        assert_eq!(load_orders_from(&path), ChannelOrders::default());
        fs::write(&path, br##"{"version":1,"servers":{"a":["#x","#x",""]}}"##).unwrap();
        assert_eq!(load_orders_from(&path).order("a"), ["#x"]);
    }
}
