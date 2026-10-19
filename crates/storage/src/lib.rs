// SPDX-License-Identifier: MPL-2.0
//! Versioned preferences. Secrets are not part of the preferences file; they
//! live in the [`credentials`] store.

use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

pub mod chatlog;
pub mod credentials;
pub mod layout;
pub mod order;
mod private_file;

pub use credentials::{CredentialBackendKind, CredentialError, CredentialStore, Secret, SecretKey};

const SETTINGS_VERSION: u32 = 15;
/// Profile IDs of the IRCnet servers that versions 1–12 always listed. They
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

/// Hosts of the two servers that `IRCNET_ID` and `IRCNET_IPV6_ID` meant.
const LEGACY_IRCNET_HOSTS: [&str; 2] = ["irc.ircnet.ne.jp", "irc6.ircnet.ne.jp"];

/// Suggested servers, `irc.ircnet.com` (the default for a new server) first,
/// then every server listed at <https://www.ircnet.info/servers> (some of
/// them only link to other servers and do not accept clients).
pub const PRESETS: [ServerPreset; 56] = [
    ServerPreset {
        name: "IRCnet",
        host: "irc.ircnet.com",
    },
    ServerPreset {
        name: "IRCnet (Japan)",
        host: "irc.ircnet.ne.jp",
    },
    ServerPreset {
        name: "IRCnet (IPv6)",
        host: "irc6.ircnet.ne.jp",
    },
    ServerPreset {
        name: "IRCnet (dev)",
        host: "dev.ircnet.ne.jp",
    },
    ServerPreset {
        name: "Austria",
        host: "hub.irc.at",
    },
    ServerPreset {
        name: "Austria",
        host: "vienna.irc.at",
    },
    ServerPreset {
        name: "Belgium",
        host: "ircnet.clue.be",
    },
    ServerPreset {
        name: "Canada",
        host: "irc.ircnet.ca",
    },
    ServerPreset {
        name: "Czech Republic",
        host: "irc.felk.cvut.cz",
    },
    ServerPreset {
        name: "Denmark",
        host: "irc.dotsrc.org",
    },
    ServerPreset {
        name: "Estonia",
        host: "irc.datanet.ee",
    },
    ServerPreset {
        name: "Estonia",
        host: "hub.ircnet.ee",
    },
    ServerPreset {
        name: "Finland",
        host: "irc.cs.hut.fi",
    },
    ServerPreset {
        name: "Finland",
        host: "irc.cc.tut.fi",
    },
    ServerPreset {
        name: "Finland",
        host: "hub.cc.tut.fi",
    },
    ServerPreset {
        name: "Finland",
        host: "irc.oulu.fi",
    },
    ServerPreset {
        name: "Finland",
        host: "irc.lut.fi",
    },
    ServerPreset {
        name: "Finland",
        host: "irc.nebula.fi",
    },
    ServerPreset {
        name: "Finland",
        host: "irc.elisa.fi",
    },
    ServerPreset {
        name: "Finland",
        host: "irc2.inet.fi",
    },
    ServerPreset {
        name: "Germany",
        host: "fu-berlin.de",
    },
    ServerPreset {
        name: "Germany",
        host: "man-da.de",
    },
    ServerPreset {
        name: "Germany",
        host: "uni-erlangen.de",
    },
    ServerPreset {
        name: "Germany",
        host: "belwue.de",
    },
    ServerPreset {
        name: "Hungary",
        host: "atw.irc.hu",
    },
    ServerPreset {
        name: "Hungary",
        host: "ssl.atw.irc.hu",
    },
    ServerPreset {
        name: "Hungary",
        host: "sasl.irc.atw.hu",
    },
    ServerPreset {
        name: "Hungary",
        host: "irc.atw-inter.net",
    },
    ServerPreset {
        name: "Hungary",
        host: "ssl.irc.atw-inter.net",
    },
    ServerPreset {
        name: "Italy",
        host: "hub.tophost.it",
    },
    ServerPreset {
        name: "Italy",
        host: "irc6.tophost.it",
    },
    ServerPreset {
        name: "Italy",
        host: "spadhausen.irc.it",
    },
    ServerPreset {
        name: "Italy",
        host: "irc.spadhausen.com",
    },
    ServerPreset {
        name: "Japan",
        host: "dh.ircnet.ne.jp",
    },
    ServerPreset {
        name: "Netherlands",
        host: "hostsailor.ircnet.nl",
    },
    ServerPreset {
        name: "Netherlands",
        host: "hub.snt.utwente.nl",
    },
    ServerPreset {
        name: "Netherlands",
        host: "irc3.snt.ipv6.utwente.nl",
    },
    ServerPreset {
        name: "Netherlands",
        host: "ircnet.tngnet.nl",
    },
    ServerPreset {
        name: "Netherlands",
        host: "ircng.snt.utwente.nl",
    },
    ServerPreset {
        name: "Netherlands",
        host: "tngnet.ircnet.io",
    },
    ServerPreset {
        name: "Netherlands",
        host: "irc.nlnog.net",
    },
    ServerPreset {
        name: "Netherlands",
        host: "openirc.snt.utwente.nl",
    },
    ServerPreset {
        name: "Norway",
        host: "irc.uio.no",
    },
    ServerPreset {
        name: "Poland",
        host: "hub.irc.pl",
    },
    ServerPreset {
        name: "Poland",
        host: "poznan.irc.pl",
    },
    ServerPreset {
        name: "Romania",
        host: "ircnet.hostsailor.com",
    },
    ServerPreset {
        name: "Slovenia",
        host: "irc.arnes.si",
    },
    ServerPreset {
        name: "Sweden",
        host: "hub.se",
    },
    ServerPreset {
        name: "Sweden",
        host: "irc.okit.se",
    },
    ServerPreset {
        name: "Sweden",
        host: "irc.swipnet.se",
    },
    ServerPreset {
        name: "Sweden",
        host: "irc.swepipe.net",
    },
    ServerPreset {
        name: "United Kingdom",
        host: "hub.uk",
    },
    ServerPreset {
        name: "United States",
        host: "hub.us.ircnet.com",
    },
    ServerPreset {
        name: "United States",
        host: "hub.us",
    },
    ServerPreset {
        name: "United States",
        host: "irc.psychz.net",
    },
    ServerPreset {
        name: "United States",
        host: "ircnet.tempest.net",
    },
];

fn default_verify_tls_certificates() -> bool {
    true
}

fn default_regain_ops() -> bool {
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
    EucKr,
    Windows1251,
    Koi8R,
    Koi8U,
    Ibm866,
    Iso8859_5,
    XMacCyrillic,
    Windows1254,
    Windows1252,
    Gbk,
    Big5,
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
    /// Every supported character encoding in the order the server settings
    /// list them. The server settings, `/charset` and its help all read this
    /// one list.
    pub const ALL: [Self; 15] = [
        Self::Utf8,
        Self::Iso2022Jp,
        Self::ShiftJis,
        Self::EucJp,
        Self::EucKr,
        Self::Windows1251,
        Self::Koi8R,
        Self::Koi8U,
        Self::Ibm866,
        Self::Iso8859_5,
        Self::XMacCyrillic,
        Self::Windows1254,
        Self::Windows1252,
        Self::Gbk,
        Self::Big5,
    ];

    /// The canonical name, which is also a WHATWG label `encoding_rs`
    /// understands.
    pub fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Iso2022Jp => "ISO-2022-JP",
            Self::ShiftJis => "Shift_JIS",
            Self::EucJp => "EUC-JP",
            Self::EucKr => "EUC-KR",
            Self::Windows1251 => "Windows-1251",
            Self::Koi8R => "KOI8-R",
            Self::Koi8U => "KOI8-U",
            Self::Ibm866 => "IBM866",
            Self::Iso8859_5 => "ISO-8859-5",
            Self::XMacCyrillic => "x-mac-cyrillic",
            Self::Windows1254 => "Windows-1254",
            Self::Windows1252 => "Windows-1252",
            Self::Gbk => "GBK",
            Self::Big5 => "Big5",
        }
    }

    /// Other names accepted when typing an encoding, besides the label.
    /// Matching ignores case, `-`, `_` and spaces.
    fn aliases(self) -> &'static [&'static str] {
        match self {
            Self::Utf8 => &[],
            Self::Iso2022Jp => &[],
            Self::ShiftJis => &["sjis", "windows-31j", "cp932"],
            Self::EucJp => &[],
            Self::EucKr => &["cp949", "windows-949", "uhc"],
            Self::Windows1251 => &["cp1251"],
            Self::Koi8R => &[],
            Self::Koi8U => &[],
            Self::Ibm866 => &["cp866", "866"],
            Self::Iso8859_5 => &[],
            Self::XMacCyrillic => &["maccyrillic", "mac-cyrillic"],
            Self::Windows1254 => &["cp1254"],
            Self::Windows1252 => &["cp1252"],
            Self::Gbk => &[],
            Self::Big5 => &[],
        }
    }

    /// The encoding a typed name stands for: a canonical label or one of its
    /// aliases. `None` for anything else; an unknown name is never taken for
    /// UTF-8. `ISO-8859-1` is not an alias: `encoding_rs` reads it as
    /// Windows-1252, which is not the same table.
    pub fn parse(name: &str) -> Option<Self> {
        fn key(name: &str) -> String {
            name.chars()
                .filter(|ch| !matches!(ch, '-' | '_' | ' '))
                .flat_map(char::to_lowercase)
                .collect()
        }
        let wanted = key(name);
        Self::ALL.into_iter().find(|encoding| {
            key(encoding.label()) == wanted
                || encoding.aliases().iter().any(|alias| key(alias) == wanted)
        })
    }
}

/// An encoding override for one channel of a server: the side that is set
/// differs from the server's encoding, the other follows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelCharset {
    pub channel: String,
    /// Used for the text of messages received in the channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decode: Option<TextEncoding>,
    /// Used for the text of messages sent to the channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encode: Option<TextEncoding>,
}

/// Overrides kept per server. A settings file can hold more, but the extra
/// entries are dropped when it is read.
pub const MAX_CHANNEL_CHARSETS: usize = 256;
/// The longest channel name an override is kept for, in bytes.
const MAX_CHARSET_CHANNEL_BYTES: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    #[serde(alias = "background")]
    pub member_list_background: String,
    pub main_log_background: String,
    pub main_log_alternate: String,
    pub channel_event_color: String,
    /// Text of NOTICE messages in the logs. Added after version 15 without a
    /// version change; files without it read as the default.
    pub notice_color: String,
    /// Mentions and keywords in logs, and channels where they arrived.
    pub highlight_color: String,
    pub sub_log_background: String,
    pub sub_log_alternate: String,
    /// Background of the bubble that shows a shortened URL in full. Added
    /// after version 15 without a version change; files without it read as
    /// the default.
    pub url_tooltip_color: String,
    /// Link text color in that bubble. Added after version 15 without a
    /// version change; files without it read as the default.
    pub url_tooltip_link_color: String,
    /// How opaque that bubble is, in percent (`MIN_URL_TOOLTIP_OPACITY` to
    /// 100). Added after version 15 without a version change.
    pub url_tooltip_opacity: u8,
    /// Text and underline of URLs in the channel logs. Added after version 15
    /// without a version change; files without it read as the default.
    pub link_color: String,
    /// Background behind URLs in the channel logs. Empty means automatic: none
    /// for plain URLs, a faint tint of the link color for shortened ones.
    /// Added after version 15 without a version change.
    pub link_background: String,
    pub alternate_rows: bool,
    /// Inline thumbnails of direct image links in the main channel log
    /// (version 14). Off by default, also for settings saved before it existed.
    pub image_previews: bool,
    /// Show long URLs in the channel log in a short form; the text, links
    /// and copies keep the full URL. Added after version 15 without a version
    /// change; files without it read as off.
    pub compact_urls: bool,
    /// Draw HTML numeric character references (`&#x1F352;`, `&#127826;`) in
    /// received text as the characters they name; the stored text is not
    /// changed. Added after version 15 without a version change; files
    /// without it read as off.
    pub decode_char_refs: bool,
    /// Reiwa mode, a channel log layout: the avatar spans two lines, the
    /// first line holds the nickname and time and the second the message,
    /// instead of the default `time | avatar | nick: message` flow. Read
    /// from the former `header_line_messages` too; the older
    /// `wrap_long_nicknames` is ignored. Added after version 15 without a
    /// version change; files without it read as off.
    #[serde(alias = "header_line_messages")]
    pub reiwa_mode: bool,
    pub main_log_font: String,
    pub sub_log_font: String,
    pub member_font: String,
    pub channel_font: String,
    pub input_font: String,
    pub time_font: String,
    /// Pane colors used while the dark theme is active; the flat fields above
    /// are the light theme's.
    pub dark: DarkColors,
    /// Colors the user saved to pick again, as `#RRGGBB` in the order added.
    /// At most [`MAX_SAVED_COLORS`], without repeats. Added after version 15
    /// without a version change; files without it read as empty.
    pub saved_colors: Vec<String>,
}

/// Colors the palette holds.
pub const MAX_SAVED_COLORS: usize = 24;

/// Opacity of the full-URL bubble, in percent. Below the minimum the text
/// would be hard to read.
pub const MIN_URL_TOOLTIP_OPACITY: u8 = 20;
pub const DEFAULT_URL_TOOLTIP_OPACITY: u8 = 80;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DarkColors {
    pub member_list_background: String,
    pub main_log_background: String,
    pub main_log_alternate: String,
    pub channel_event_color: String,
    pub notice_color: String,
    pub highlight_color: String,
    pub sub_log_background: String,
    pub sub_log_alternate: String,
    pub url_tooltip_color: String,
    pub link_color: String,
    pub link_background: String,
    pub url_tooltip_link_color: String,
}

impl Default for DarkColors {
    fn default() -> Self {
        Self {
            member_list_background: "#1F2124".into(),
            main_log_background: "#1F2124".into(),
            main_log_alternate: "#272B31".into(),
            channel_event_color: "#6CC46C".into(),
            notice_color: "#8C949C".into(),
            highlight_color: "#EFA0BE".into(),
            sub_log_background: "#24272B".into(),
            sub_log_alternate: "#2C3036".into(),
            url_tooltip_color: "#2A2D32".into(),
            link_color: "#78AAFF".into(),
            link_background: String::new(),
            url_tooltip_link_color: "#78AAFF".into(),
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
            notice_color: "#7A838C".into(),
            highlight_color: "#D46A8E".into(),
            sub_log_background: "#F9FAFB".into(),
            sub_log_alternate: "#F2F5FF".into(),
            url_tooltip_color: "#FFFFFF".into(),
            url_tooltip_link_color: "#0645AD".into(),
            url_tooltip_opacity: DEFAULT_URL_TOOLTIP_OPACITY,
            link_color: "#0645AD".into(),
            link_background: String::new(),
            alternate_rows: false,
            image_previews: false,
            compact_urls: false,
            decode_char_refs: false,
            reiwa_mode: false,
            main_log_font: String::new(),
            sub_log_font: String::new(),
            member_font: String::new(),
            channel_font: String::new(),
            input_font: String::new(),
            time_font: String::new(),
            dark: DarkColors::default(),
            saved_colors: Vec::new(),
        }
    }
}

impl Appearance {
    /// Saves `color` (`#RRGGBB`, any case) to the palette. A color already
    /// saved is not added again.
    pub fn save_color(&mut self, color: &str) -> Result<(), String> {
        let color = color.trim().to_ascii_uppercase();
        if color_value(&color).is_none() {
            return Err("Palette color must be #RRGGBB.".into());
        }
        if self.saved_colors.contains(&color) {
            return Ok(());
        }
        if self.saved_colors.len() >= MAX_SAVED_COLORS {
            return Err(format!(
                "The palette holds at most {MAX_SAVED_COLORS} colors."
            ));
        }
        self.saved_colors.push(color);
        Ok(())
    }

    pub fn remove_saved_color(&mut self, color: &str) {
        self.saved_colors
            .retain(|saved| !saved.eq_ignore_ascii_case(color.trim()));
    }

    /// Drops palette entries that are not colors or repeat, and any beyond
    /// the limit, so a hand-edited file cannot keep the settings from loading.
    fn clean_palette(&mut self) {
        let mut kept: Vec<String> = Vec::new();
        for color in &self.saved_colors {
            let color = color.trim().to_ascii_uppercase();
            if color_value(&color).is_some() && !kept.contains(&color) {
                kept.push(color);
            }
        }
        kept.truncate(MAX_SAVED_COLORS);
        self.saved_colors = kept;
    }

    pub fn validate(&self) -> Result<(), String> {
        for (label, value) in [
            ("Member list", &self.member_list_background),
            ("Main log", &self.main_log_background),
            ("Main alternate", &self.main_log_alternate),
            ("System message", &self.channel_event_color),
            ("Notice", &self.notice_color),
            ("Highlight", &self.highlight_color),
            ("Sub log", &self.sub_log_background),
            ("Sub alternate", &self.sub_log_alternate),
            ("URL tooltip", &self.url_tooltip_color),
            ("Dark URL tooltip", &self.dark.url_tooltip_color),
            ("Link", &self.link_color),
            ("Dark link", &self.dark.link_color),
            ("URL tooltip link", &self.url_tooltip_link_color),
            ("Dark URL tooltip link", &self.dark.url_tooltip_link_color),
            ("Dark member list", &self.dark.member_list_background),
            ("Dark main log", &self.dark.main_log_background),
            ("Dark main alternate", &self.dark.main_log_alternate),
            ("Dark system message", &self.dark.channel_event_color),
            ("Dark notice", &self.dark.notice_color),
            ("Dark highlight", &self.dark.highlight_color),
            ("Dark sub log", &self.dark.sub_log_background),
            ("Dark sub alternate", &self.dark.sub_log_alternate),
        ] {
            if color_value(value).is_none() {
                return Err(format!("{label} color must be #RRGGBB."));
            }
        }
        for (label, value) in [
            ("Link background", &self.link_background),
            ("Dark link background", &self.dark.link_background),
        ] {
            if !value.is_empty() && color_value(value).is_none() {
                return Err(format!("{label} color must be #RRGGBB or empty."));
            }
        }
        if !(MIN_URL_TOOLTIP_OPACITY..=100).contains(&self.url_tooltip_opacity) {
            return Err(format!(
                "URL tooltip opacity must be {MIN_URL_TOOLTIP_OPACITY} to 100."
            ));
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
    /// Send the server password even without TLS, after the user accepted a
    /// warning; for bouncers such as ZNC on a trusted network.
    #[serde(default)]
    pub allow_plaintext_pass: bool,
    /// Identity and channels belong to each server (version 13); earlier
    /// versions kept one application-wide set, which migration gives only
    /// to the server it was used with.
    #[serde(default)]
    pub nickname: String,
    /// IRC `USER` username (ident); independent of the nickname.
    #[serde(default)]
    pub username: String,
    /// IRC real name (GECOS) sent in `USER` and by IRCv3 `SETNAME`; empty
    /// means the built-in default. Added without a version change; files
    /// without it read as empty.
    #[serde(default)]
    pub realname: String,
    /// `QUIT` reason sent on a normal disconnect; empty means the built-in
    /// default. Added without a version change; files without it read as empty.
    #[serde(default)]
    pub quit_message: String,
    /// `QUIT` reason sent just before the machine sleeps; empty means
    /// `quit_message`. Added without a version change; files without it read
    /// as empty.
    #[serde(default)]
    pub sleep_quit_message: String,
    /// `PART` reason for a `PART` that names none; empty means no reason.
    /// Independent of `quit_message`. Added without a version change; files
    /// without it read as empty.
    #[serde(default)]
    pub part_message: String,
    /// Short name shown for this server in the combined log; empty means the
    /// first word of the host. Added without a version change; files without
    /// it read as empty.
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub channels: String,
    #[serde(default)]
    pub sasl_enabled: bool,
    /// SASL account name; independent of both nickname and username.
    #[serde(default)]
    pub sasl_username: String,
    #[serde(default)]
    pub connect_on_startup: bool,
    /// Show small user avatars beside main-log messages and in the member
    /// list of this server, with client-drawn defaults for users without
    /// one (D023). Independent per server and of `image_previews`; there is
    /// no application-wide switch. Added after version 15 without a version
    /// change; files without it read as off. A file that still has the old
    /// `appearance.user_avatars` gets its value copied here on load.
    #[serde(default)]
    pub user_avatars: bool,
    /// After an unexpected disconnect, rejoin a channel where we were an
    /// operator before, found us alone and without operator status, to get
    /// operator status back (#444). Parting an empty channel resets it; the
    /// topic and the common flags are restored. On by default; added
    /// without a version change, so files without it read as on.
    #[serde(default = "default_regain_ops")]
    pub regain_ops: bool,
    /// Opt-in IRCv3 features for this server (version 15), off by default.
    #[serde(default)]
    pub ircv3: Ircv3Preferences,
    /// Draft URL of our own avatar for this server's experimental metadata
    /// (D023). Saving it publishes nothing: only the IRCv3 tab's explicit
    /// Publish sends it, on a connected server. Added without a version
    /// change; files without it read as empty.
    #[serde(default)]
    pub avatar_url: String,
    /// The avatar URL we share with other clients through CTCP AVATAR
    /// (`ircv3.peer_avatars`), empty when nothing is shared. It is set only
    /// by an explicit "Share with Peers", never from the draft above, and
    /// cleared when peer avatars are turned off. Added without a version
    /// change; files without it read as empty.
    #[serde(default)]
    pub peer_avatar_url: String,
    /// Channels whose text uses another encoding than `encoding`, set with
    /// `/charset` only (there is no settings page for it). A side that is
    /// not set follows `encoding`. Added without a version change; files
    /// without it read as empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channel_charsets: Vec<ChannelCharset>,
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
            allow_plaintext_pass: false,
            nickname: String::new(),
            username: String::new(),
            realname: String::new(),
            quit_message: String::new(),
            sleep_quit_message: String::new(),
            part_message: String::new(),
            display_name: String::new(),
            channels: String::new(),
            sasl_enabled: false,
            sasl_username: String::new(),
            connect_on_startup: false,
            user_avatars: false,
            regain_ops: true,
            ircv3: Ircv3Preferences::default(),
            avatar_url: String::new(),
            peer_avatar_url: String::new(),
            channel_charsets: Vec::new(),
            legacy_server_password: None,
            legacy_sasl_password: None,
        }
    }

    /// The channels joined after registration, in order: the enabled
    /// auto-join entries.
    pub fn channels(&self) -> Vec<String> {
        self.auto_join_entries()
            .into_iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.name)
            .collect()
    }

    /// Every auto-join entry in the configured order, disabled ones included.
    pub fn auto_join_entries(&self) -> Vec<AutoJoinEntry> {
        parse_auto_join(&self.channels)
    }

    pub fn set_auto_join_entries(&mut self, entries: &[AutoJoinEntry]) {
        self.channels = format_auto_join(entries);
    }

    /// The override kept for `channel`; `same` tells whether two names are
    /// the same channel (the network's case mapping).
    pub fn channel_charset(
        &self,
        channel: &str,
        same: impl Fn(&str, &str) -> bool,
    ) -> Option<&ChannelCharset> {
        self.channel_charsets
            .iter()
            .find(|entry| same(&entry.channel, channel))
    }

    /// Replaces the override of `channel`; a side left `None` follows the
    /// server's encoding, and none at all removes the entry. Fails when
    /// there is no room for one more.
    pub fn set_channel_charset(
        &mut self,
        channel: &str,
        decode: Option<TextEncoding>,
        encode: Option<TextEncoding>,
        same: impl Fn(&str, &str) -> bool,
    ) -> Result<(), String> {
        let at = self
            .channel_charsets
            .iter()
            .position(|entry| same(&entry.channel, channel));
        match (at, decode.is_none() && encode.is_none()) {
            (Some(at), true) => {
                self.channel_charsets.remove(at);
            }
            (None, true) => {}
            (Some(at), false) => {
                let entry = &mut self.channel_charsets[at];
                entry.decode = decode;
                entry.encode = encode;
            }
            (None, false) => {
                if self.channel_charsets.len() >= MAX_CHANNEL_CHARSETS {
                    return Err(format!(
                        "At most {MAX_CHANNEL_CHARSETS} channels can have their own character encoding."
                    ));
                }
                self.channel_charsets.push(ChannelCharset {
                    channel: channel.to_owned(),
                    decode,
                    encode,
                });
            }
        }
        Ok(())
    }

    /// Drops entries a settings file should not hold: no override, an
    /// unusable name, a repeated name or more than the limit.
    fn clean_channel_charsets(&mut self) {
        let mut seen = HashSet::new();
        self.channel_charsets.retain(|entry| {
            (entry.decode.is_some() || entry.encode.is_some())
                && entry.channel.len() <= MAX_CHARSET_CHANNEL_BYTES
                && entry.channel.starts_with(['#', '&', '!'])
                && !entry
                    .channel
                    .contains(|ch: char| ch.is_control() || ch.is_whitespace() || ch == ',')
                && seen.insert(entry.channel.to_ascii_lowercase())
        });
        self.channel_charsets.truncate(MAX_CHANNEL_CHARSETS);
    }

    pub fn server_password_key(&self) -> SecretKey {
        SecretKey::server_password(&self.id)
    }

    pub fn sasl_password_key(&self) -> SecretKey {
        SecretKey::sasl_password(&self.id)
    }
}

/// One auto-join channel of a server. A disabled entry stays configured but
/// is skipped when connecting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoJoinEntry {
    pub name: String,
    pub enabled: bool,
}

/// Prefix marking a disabled entry in `ServerProfile::channels`, a
/// comma-separated list. No channel name starts with it.
const DISABLED_PREFIX: char = '-';

/// Reads the comma-separated list kept in `ServerProfile::channels`.
pub fn parse_auto_join(text: &str) -> Vec<AutoJoinEntry> {
    text.split(',')
        .map(str::trim)
        .filter_map(|item| {
            let (name, enabled) = match item.strip_prefix(DISABLED_PREFIX) {
                Some(name) => (name.trim(), false),
                None => (item, true),
            };
            (!name.is_empty()).then(|| AutoJoinEntry {
                name: name.to_owned(),
                enabled,
            })
        })
        .collect()
}

/// Writes entries as `parse_auto_join` reads them. Names are cleaned of
/// whitespace and commas; empty ones are dropped.
pub fn format_auto_join(entries: &[AutoJoinEntry]) -> String {
    entries
        .iter()
        .filter_map(|entry| {
            let name: String = entry
                .name
                .chars()
                .filter(|c| !c.is_whitespace() && *c != ',')
                .collect();
            let name = name.trim_start_matches(DISABLED_PREFIX);
            (!name.is_empty()).then(|| {
                if entry.enabled {
                    name.to_owned()
                } else {
                    format!("{DISABLED_PREFIX}{name}")
                }
            })
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Opt-in IRCv3 features of one server. Negotiation happens when the server
/// next connects. Every field is a separate preference with its own serde
/// default, so a feature can later become on by default (with a version
/// migration) or be shown on another settings tab without touching the
/// protocol code, which only receives the resulting booleans.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ircv3Preferences {
    /// Request `message-tags`.
    pub message_tags: bool,
    /// Request `server-time` and show server timestamps.
    pub server_time: bool,
    /// Request `batch`, so history batches are recognized. Added after
    /// version 15 without a version change: files without it read as off.
    pub batch: bool,
    /// Exchange avatars with other clients through KVIrc's CTCP AVATAR
    /// (experimental), for servers without avatar metadata. Added without a
    /// version change: files without it read as off.
    pub peer_avatars: bool,
    /// Request recent channel history with `draft/chathistory`
    /// (experimental). Added without a version change: files without it
    /// read as off.
    pub chathistory: bool,
    /// Server-confirmed sending: `echo-message` and `labeled-response`.
    /// Added without a version change: files without it read as off.
    pub confirmed_sending: bool,
    /// Follow the services accounts and real names of channel members
    /// (`account-notify`, `extended-join`, WHOX). Added without a version
    /// change: files without it read as off.
    pub accounts: bool,
}

/// External image hosting for IRC. Disabled until the user picks a provider.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageUpload {
    /// Provider ID from the uploader registry, or `None` when disabled.
    pub provider: Option<String>,
}

/// Desktop notifications (version 12). Older settings get the defaults.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notifications {
    pub enabled: bool,
    /// Channel messages that mention our nickname.
    pub mentions: bool,
    /// Channel messages containing one of `keywords`.
    pub keyword_alerts: bool,
    pub keywords: Vec<String>,
    pub private_messages: bool,
    /// Beep and flash the taskbar button (Windows) when a notification
    /// fires. Off by default.
    pub sound: bool,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            enabled: true,
            mentions: true,
            keyword_alerts: true,
            keywords: Vec::new(),
            private_messages: true,
            sound: false,
        }
    }
}

/// Opt-in conversation logs written to files (see [`chatlog`]). Both are off
/// by default and independent of each other. Added after version 15 without
/// a version change; files without it read as the defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConversationLog {
    /// Channel conversations.
    pub channels: bool,
    /// Private conversations (direct messages).
    pub private: bool,
    /// Where logs go; `None` is [`chatlog::default_directory`].
    pub directory: Option<PathBuf>,
}

impl ConversationLog {
    pub fn enabled(&self) -> bool {
        self.channels || self.private
    }

    /// The directory in use: the chosen one, else the default.
    pub fn resolved_directory(&self) -> Option<PathBuf> {
        self.directory
            .clone()
            .filter(|directory| !directory.as_os_str().is_empty())
            .or_else(chatlog::default_directory)
    }
}

/// Opt-in diagnostics. Additive defaults keep older settings compatible.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Experimental {
    pub debug_logging: bool,
    pub stderr_file: Option<PathBuf>,
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
    /// Windows and Linux: hide the in-window menu bar until Alt, F10 or the
    /// top edge asks for it. Off shows it all the time. Added after version
    /// 15 without a version change; files without it read as off. macOS has
    /// native menus and ignores it.
    pub menu_bar_auto_hide: bool,
    /// Show the toolbar below the menu bar (View → Toolbar). On by default,
    /// also for files saved before it existed (added after version 15
    /// without a version change).
    pub show_toolbar: bool,
    pub appearance: Appearance,
    /// Reopen the main window where it was left, with its pane sizes (see
    /// [`layout`]). On by default, also for files saved before it existed.
    pub restore_window_layout: bool,
    /// Keep every CayenChat window (chat, settings, WHOIS) above ordinary
    /// windows on Windows.
    /// Off by default, including settings files written before this existed.
    /// Ignored on other platforms.
    pub always_on_top: bool,
    /// Ask before the user quits the application. Off by default, also for
    /// files saved before it existed. Quits by the system or by a signal
    /// never ask.
    pub confirm_quit: bool,
    /// Shorten long Amazon product links to `/dp/<ASIN>` in the text a send
    /// puts on the wire; the draft keeps what was typed. On by default, also
    /// for files saved before it existed.
    pub shorten_amazon_urls: bool,
    pub credential_backend: CredentialBackendKind,
    pub image_upload: ImageUpload,
    /// Navigation shortcuts the user changed: action id → the key as GPUI
    /// reports it when pressed (for example `cmd-}`). An action that is not
    /// listed has its platform defaults. Entries for unknown actions, or keys
    /// that cannot be used, are ignored when the shortcuts are built, so a
    /// damaged or hand-edited file never stops the application from starting.
    /// Added after version 15 without a version change; files without it read
    /// as empty.
    pub keybindings: BTreeMap<String, String>,
    pub notifications: Notifications,
    pub experimental: Experimental,
    pub conversation_log: ConversationLog,
    /// Application-wide identity of versions 1–12, read only to migrate it
    /// into the previously selected server profile; never written back.
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
            menu_bar_auto_hide: false,
            show_toolbar: true,
            text_key_theme: TextKeyTheme::Auto,
            appearance: Appearance::default(),
            restore_window_layout: true,
            always_on_top: false,
            confirm_quit: false,
            shorten_amazon_urls: true,
            credential_backend: CredentialBackendKind::System,
            image_upload: ImageUpload::default(),
            keybindings: BTreeMap::new(),
            notifications: Notifications::default(),
            experimental: Experimental::default(),
            conversation_log: ConversationLog::default(),
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
        self.appearance.clean_palette();
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
        let migrate_identity = self.version <= 12;
        self.version = SETTINGS_VERSION;
        let mut seen = HashSet::new();
        self.servers
            .retain(|s| !s.id.is_empty() && seen.insert(s.id.clone()) && s.port != 0);
        if migrate_identity {
            // Versions 1–12 always listed both IRCnet servers after the
            // user-added ones. Keep a preset only if it was in use: selected
            // (created if the file omitted it) or holding saved passwords.
            for (index, id) in [IRCNET_ID, IRCNET_IPV6_ID].into_iter().enumerate() {
                let host = LEGACY_IRCNET_HOSTS[index];
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
            server.clean_channel_charsets();
            if !server.remember_passwords {
                server.legacy_server_password = None;
                server.legacy_sasl_password = None;
            }
        }
        // The server that versions 1–12 connected with: the saved selection,
        // or the only server there is. Otherwise it is not guessed.
        let identity_owner = if self.servers.iter().any(|s| s.id == self.selected_server) {
            Some(self.selected_server.clone())
        } else if let [only] = self.servers.as_slice() {
            Some(only.id.clone())
        } else {
            None
        };
        if !self.servers.iter().any(|s| s.id == self.selected_server) {
            self.selected_server = self
                .servers
                .first()
                .map(|s| s.id.clone())
                .unwrap_or_default();
        }
        let legacy = std::mem::take(&mut self.legacy);
        if migrate_identity
            && let Some(server) = self
                .servers
                .iter_mut()
                .find(|s| Some(&s.id) == identity_owner.as_ref())
        {
            // The old shared identity belonged to the server it was used
            // with. Other servers keep their connection details and stored
            // credentials but start without a nickname, username, channels
            // or account: copying them made every server auto-join the same
            // channels. SASL stays on only with TLS, which credentials need.
            server.nickname = legacy.nickname;
            server.username = legacy.username;
            server.channels = legacy.channels;
            server.sasl_username = legacy.sasl_username;
            server.sasl_enabled = legacy.sasl_enabled && server.use_tls;
            server.connect_on_startup = legacy.connect_on_startup;
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
            OldServerChoice::Ircnet => (IRCNET_ID.to_owned(), LEGACY_IRCNET_HOSTS[0]),
            OldServerChoice::IrcnetIpv6 => (IRCNET_IPV6_ID.to_owned(), LEGACY_IRCNET_HOSTS[1]),
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

/// The directory `CAYENCHAT_TEST_DIR` names for a test build, created
/// readable only by the user when it is missing. `None` when it is unset or
/// empty. It is for trying what must survive a restart (the window layout,
/// the settings) in a build that otherwise starts empty every time; it also
/// holds that build's credentials file, so it should be a directory of its
/// own.
#[cfg(any(test, feature = "test-build"))]
fn fixed_test_directory(value: Option<std::ffi::OsString>) -> Result<Option<PathBuf>, String> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let directory = PathBuf::from(value);
    if !directory.is_absolute() {
        return Err("CAYENCHAT_TEST_DIR must be an absolute path.".into());
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&directory)
        .map_err(|error| format!("Could not create CAYENCHAT_TEST_DIR: {error}"))?;
    // A directory that existed already keeps its permissions, and a test
    // build keeps its credentials file here: refuse one other users can read.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&directory)
            .map_err(|error| format!("Could not check CAYENCHAT_TEST_DIR: {error}"))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(format!(
                "CAYENCHAT_TEST_DIR is accessible by other users (mode {:o}); run chmod 700 on it.",
                mode & 0o777
            ));
        }
    }
    Ok(Some(directory))
}

/// A test build's configuration directory: new and empty for every launch,
/// under the system temporary directory, readable only by the user. Unless
/// `CAYENCHAT_TEST_DIR` names a directory, which is then used and kept.
#[cfg(feature = "test-build")]
pub fn test_build_directory() -> Result<PathBuf, String> {
    static DIRECTORY: std::sync::OnceLock<Result<PathBuf, String>> = std::sync::OnceLock::new();
    DIRECTORY
        .get_or_init(|| {
            if let Some(fixed) = fixed_test_directory(std::env::var_os("CAYENCHAT_TEST_DIR"))? {
                return Ok(fixed);
            }
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_millis())
                .unwrap_or_default();
            let directory =
                std::env::temp_dir().join(format!("cayenchat-test-{}-{stamp}", std::process::id()));
            let mut builder = fs::DirBuilder::new();
            // A fresh directory: creating one that exists fails.
            builder.recursive(false);
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

/// Moves the old application-wide `appearance.user_avatars` into every server
/// that has no value of its own, then drops it, so a file is migrated once
/// and the old key is never written back (D023). A missing or non-boolean
/// old value reads as off.
fn move_legacy_user_avatars(value: &mut serde_json::Value) {
    let Some(appearance) = value
        .get_mut("appearance")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    let Some(old) = appearance.remove("user_avatars") else {
        return;
    };
    let old = old.as_bool().unwrap_or(false);
    let Some(servers) = value
        .get_mut("servers")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for server in servers.iter_mut().filter_map(|s| s.as_object_mut()) {
        server
            .entry("user_avatars")
            .or_insert(serde_json::Value::Bool(old));
    }
}

/// Reads and migrates settings from `path`; `None` when there is no file.
/// [`load`] uses the platform path; tests pass their own.
pub fn load_from(path: &Path) -> Result<Option<Settings>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Could not read settings: {error}")),
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Could not parse settings: {error}"))?;
    let mut value = value;
    move_legacy_user_avatars(&mut value);
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

/// Writes settings to `path`, readable only by the user, replacing the file
/// whole or not at all.
pub fn save_to(path: &Path, settings: &Settings) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(&settings.clone().normalize())
        .map_err(|error| format!("Could not serialize settings: {error}"))?;
    // The settings window saves on the UI thread, so the file is not flushed.
    private_file::write(path, &bytes, private_file::Flush::Skip)
        .map_err(|error| format!("Could not write settings: {error}"))
}

/// Copies a settings file that could not be loaded next to it, so the next
/// save, which replaces it, does not lose it (for example settings from a
/// newer version, or a file edited by hand). Returns the copy's path.
pub fn keep_unreadable_copy() -> Result<PathBuf, String> {
    keep_unreadable_copy_of(&settings_path()?)
}

/// [`keep_unreadable_copy`] for the settings file at `path`.
pub fn keep_unreadable_copy_of(path: &Path) -> Result<PathBuf, String> {
    let bytes = fs::read(path).map_err(|error| format!("Could not read settings: {error}"))?;
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let copy = path.with_file_name(format!("settings-unreadable-{seconds}.json"));
    private_file::write(&copy, &bytes, private_file::Flush::Skip)
        .map_err(|error| format!("Could not copy the unreadable settings: {error}"))?;
    Ok(copy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_auto_join_entries_stay_saved_but_are_not_joined() {
        let mut profile = ServerProfile {
            channels: "#a, -#b ,#c,,-".into(),
            ..Default::default()
        };
        let entries = profile.auto_join_entries();
        assert_eq!(
            entries.iter().map(|e| e.enabled).collect::<Vec<_>>(),
            [true, false, true]
        );
        assert_eq!(profile.channels(), ["#a", "#c"]);
        // Order is kept, and names cannot break the list format.
        let mut entries = entries;
        entries.swap(0, 2);
        entries[1].name = "#x y,z".into();
        profile.set_auto_join_entries(&entries);
        assert_eq!(profile.channels, "#c,-#xyz,#a");
        assert_eq!(profile.channels(), ["#c", "#a"]);
    }

    #[test]
    fn encoding_names_parse_to_the_canonical_encoding() {
        for encoding in TextEncoding::ALL {
            assert_eq!(TextEncoding::parse(encoding.label()), Some(encoding));
            assert_eq!(
                TextEncoding::parse(&encoding.label().to_lowercase()),
                Some(encoding)
            );
        }
        let mut labels: Vec<_> = TextEncoding::ALL.map(TextEncoding::label).to_vec();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), 15);
        for (name, expected) in [
            ("shift-jis", TextEncoding::ShiftJis),
            ("sjis", TextEncoding::ShiftJis),
            ("CP949", TextEncoding::EucKr),
            ("windows-949", TextEncoding::EucKr),
            ("CP866", TextEncoding::Ibm866),
            ("MacCyrillic", TextEncoding::XMacCyrillic),
            ("utf8", TextEncoding::Utf8),
            ("cp1251", TextEncoding::Windows1251),
        ] {
            assert_eq!(TextEncoding::parse(name), Some(expected), "{name}");
        }
        // Never taken for another encoding.
        for name in ["", "latin1", "ISO-8859-1", "klingon", "utf-16"] {
            assert_eq!(TextEncoding::parse(name), None, "{name}");
        }
    }

    #[test]
    fn channel_charsets_are_set_replaced_removed_and_limited() {
        let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
        let mut profile = ServerProfile::new("a".into(), "irc.example.net");
        assert!(profile.channel_charset("#a", same).is_none());
        profile
            .set_channel_charset("#A", Some(TextEncoding::Koi8R), None, same)
            .unwrap();
        profile
            .set_channel_charset("#a", None, Some(TextEncoding::Big5), same)
            .unwrap();
        assert_eq!(profile.channel_charsets.len(), 1);
        let entry = profile.channel_charset("#A", same).unwrap();
        assert_eq!(entry.decode, None);
        assert_eq!(entry.encode, Some(TextEncoding::Big5));
        profile.set_channel_charset("#a", None, None, same).unwrap();
        assert!(profile.channel_charsets.is_empty());
        for index in 0..MAX_CHANNEL_CHARSETS {
            profile
                .set_channel_charset(&format!("#c{index}"), Some(TextEncoding::Gbk), None, same)
                .unwrap();
        }
        assert!(
            profile
                .set_channel_charset("#more", Some(TextEncoding::Gbk), None, same)
                .is_err()
        );
        // An existing one can still change.
        profile
            .set_channel_charset("#c0", Some(TextEncoding::Big5), None, same)
            .unwrap();
    }

    #[test]
    fn channel_charsets_persist_per_server_and_unusable_entries_are_dropped() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let same = |a: &str, b: &str| a == b;
        let mut settings = Settings::default();
        settings.add_server("");
        settings.selected_profile_mut().unwrap().host = "one.example.net".into();
        settings
            .selected_profile_mut()
            .unwrap()
            .set_channel_charset(
                "#がが",
                Some(TextEncoding::Iso2022Jp),
                Some(TextEncoding::EucJp),
                same,
            )
            .unwrap();
        settings.add_server("");
        settings.selected_profile_mut().unwrap().host = "two.example.net".into();
        let profile = settings.selected_profile_mut().unwrap();
        profile.channel_charsets = vec![
            ChannelCharset {
                channel: "#ok".into(),
                decode: Some(TextEncoding::Gbk),
                encode: None,
            },
            ChannelCharset {
                channel: "#none".into(),
                decode: None,
                encode: None,
            },
            ChannelCharset {
                channel: "nick".into(),
                decode: Some(TextEncoding::Gbk),
                encode: None,
            },
            ChannelCharset {
                channel: "#a b".into(),
                decode: Some(TextEncoding::Gbk),
                encode: None,
            },
        ];
        save_to(&path, &settings).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        let [one, two] = loaded.servers.as_slice() else {
            panic!("two servers");
        };
        assert_eq!(one.channel_charsets.len(), 1);
        assert_eq!(one.channel_charsets[0].channel, "#がが");
        assert_eq!(
            one.channel_charsets[0].decode,
            Some(TextEncoding::Iso2022Jp)
        );
        assert_eq!(one.channel_charsets[0].encode, Some(TextEncoding::EucJp));
        assert_eq!(two.channel_charsets.len(), 1);
        assert_eq!(two.channel_charsets[0].channel, "#ok");
        assert!(two.channel_charset("#がが", same).is_none());
    }

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
    fn realname_is_per_profile_and_absent_in_older_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        // A version 15 file from before the setting existed.
        let original = serde_json::json!({
            "version": 15,
            "selected_server": "custom-1",
            "servers": [{
                "id": "custom-1", "custom": true, "host": "irc.example.net",
                "port": 6697, "use_tls": true, "encoding": "utf8",
                "nickname": "alice", "username": "ident"
            }],
            "credential_backend": "system"
        });
        fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.selected_profile().unwrap().realname, "");
        assert_eq!(settings.selected_profile().unwrap().username, "ident");

        settings.selected_profile_mut().unwrap().realname = "Alice Liddell".into();
        settings.add_server("irc.example.org");
        save_to(&path, &settings).unwrap();
        let reloaded = load_from(&path).unwrap().unwrap();
        let realnames: Vec<_> = reloaded
            .servers
            .iter()
            .map(|server| server.realname.as_str())
            .collect();
        assert_eq!(realnames, ["Alice Liddell", ""]);
    }

    #[test]
    fn an_unreadable_file_is_copied_before_a_save_replaces_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let newer = br#"{"version": 9999, "servers": []}"#;
        fs::write(&path, newer).unwrap();
        assert!(load_from(&path).is_err());
        let copy = keep_unreadable_copy_of(&path).unwrap();
        save_to(&path, &Settings::default()).unwrap();
        assert_eq!(fs::read(copy).unwrap(), newer);
        assert_eq!(load_from(&path).unwrap(), Some(Settings::default()));
    }

    #[test]
    fn quit_message_is_per_profile_and_absent_in_older_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let original = serde_json::json!({
            "version": 15,
            "selected_server": "custom-1",
            "servers": [{
                "id": "custom-1", "custom": true, "host": "irc.example.net",
                "port": 6697, "use_tls": true, "encoding": "utf8",
                "nickname": "alice", "username": "ident"
            }],
            "credential_backend": "system"
        });
        fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.selected_profile().unwrap().quit_message, "");

        settings.selected_profile_mut().unwrap().quit_message = "Back soon".into();
        settings.add_server("irc.example.org");
        save_to(&path, &settings).unwrap();
        let reloaded = load_from(&path).unwrap().unwrap();
        let messages: Vec<_> = reloaded
            .servers
            .iter()
            .map(|server| server.quit_message.as_str())
            .collect();
        assert_eq!(messages, ["Back soon", ""]);
    }

    #[test]
    fn sleep_quit_message_is_per_profile_and_absent_in_older_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let original = serde_json::json!({
            "version": 15,
            "selected_server": "custom-1",
            "servers": [{
                "id": "custom-1", "custom": true, "host": "irc.example.net",
                "port": 6697, "use_tls": true, "encoding": "utf8",
                "nickname": "alice", "username": "ident", "quit_message": "Bye"
            }],
            "credential_backend": "system"
        });
        fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.selected_profile().unwrap().sleep_quit_message, "");

        settings.selected_profile_mut().unwrap().sleep_quit_message = "Zzz".into();
        settings.add_server("irc.example.org");
        save_to(&path, &settings).unwrap();
        let reloaded = load_from(&path).unwrap().unwrap();
        let messages: Vec<_> = reloaded
            .servers
            .iter()
            .map(|server| {
                (
                    server.quit_message.as_str(),
                    server.sleep_quit_message.as_str(),
                )
            })
            .collect();
        assert_eq!(messages, [("Bye", "Zzz"), ("", "")]);
    }

    #[test]
    fn part_message_is_per_profile_and_absent_in_older_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let original = serde_json::json!({
            "version": 15,
            "selected_server": "custom-1",
            "servers": [{
                "id": "custom-1", "custom": true, "host": "irc.example.net",
                "port": 6697, "use_tls": true, "encoding": "utf8",
                "nickname": "alice", "username": "ident"
            }],
            "credential_backend": "system"
        });
        fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.selected_profile().unwrap().part_message, "");

        let profile = settings.selected_profile_mut().unwrap();
        profile.part_message = "またね".into();
        profile.quit_message = "Bye".into();
        settings.add_server("irc.example.org");
        save_to(&path, &settings).unwrap();
        let reloaded = load_from(&path).unwrap().unwrap();
        let messages: Vec<_> = reloaded
            .servers
            .iter()
            .map(|server| (server.part_message.as_str(), server.quit_message.as_str()))
            .collect();
        assert_eq!(messages, [("またね", "Bye"), ("", "")]);
    }

    #[test]
    fn display_name_is_per_profile_and_absent_in_older_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let original = serde_json::json!({
            "version": 15,
            "selected_server": "custom-1",
            "servers": [{
                "id": "custom-1", "custom": true, "host": "irc.example.net",
                "port": 6697, "use_tls": true, "encoding": "utf8",
                "nickname": "alice", "username": "ident"
            }],
            "credential_backend": "system"
        });
        fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.selected_profile().unwrap().display_name, "");

        settings.selected_profile_mut().unwrap().display_name = "ex".into();
        settings.add_server("irc.example.org");
        save_to(&path, &settings).unwrap();
        let reloaded = load_from(&path).unwrap().unwrap();
        let names: Vec<_> = reloaded
            .servers
            .iter()
            .map(|server| server.display_name.as_str())
            .collect();
        assert_eq!(names, ["ex", ""]);
    }

    #[test]
    fn legacy_channel_notice_setting_is_ignored() {
        let old = serde_json::json!({
            "enabled": true,
            "mentions": true,
            "keyword_alerts": true,
            "private_messages": true,
            "channel_notices": true
        });
        let notifications: Notifications = serde_json::from_value(old).unwrap();
        assert_eq!(notifications, Notifications::default());
        let saved = serde_json::to_value(&notifications).unwrap();
        assert!(saved.get("channel_notices").is_none());
    }

    #[test]
    fn supported_profile_versions_preserve_connection_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        // Versions 2–12 kept one application-wide identity (see D017).
        for version in 2..=12 {
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
            assert_eq!(settings.notifications, Notifications::default());
            assert_eq!(fs::read(&path).unwrap(), bytes);
            save_to(&path, &settings).unwrap();
            assert_eq!(load_from(&path).unwrap(), Some(settings));
        }
    }

    #[test]
    fn shared_identity_moves_only_into_the_selected_server() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let old = serde_json::json!({
            "version": 12,
            "selected_server": "custom-1",
            "servers": [
                {"id": "custom-1", "custom": true, "host": "irc.example.net",
                 "port": 6697, "use_tls": true, "encoding": "utf8",
                 "remember_passwords": true},
                {"id": "custom-2", "custom": true, "host": "irc.example.org",
                 "port": 7000, "use_tls": true, "encoding": "iso2022_jp",
                 "verify_tls_certificates": false, "remember_passwords": true}
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
        let selected = settings.profile("custom-1").unwrap();
        assert_eq!(selected.nickname, "alice");
        assert_eq!(selected.username, "ident");
        assert_eq!(selected.channels(), ["#a", "#b"]);
        assert_eq!(selected.sasl_username, "account");
        assert!(selected.sasl_enabled && selected.connect_on_startup);
        assert!(
            !selected.allow_plaintext_pass,
            "absent means TLS is required for passwords"
        );

        // The other server keeps its connection details and saved-password
        // choice (its credentials stay under its ID) but gets no copy of the
        // shared identity, channels or account.
        let other = settings.profile("custom-2").unwrap();
        assert_eq!(
            (other.host.as_str(), other.port, other.use_tls),
            ("irc.example.org", 7000, true)
        );
        assert_eq!(other.encoding, TextEncoding::Iso2022Jp);
        assert!(!other.verify_tls_certificates && other.remember_passwords);
        assert!(other.nickname.is_empty() && other.username.is_empty());
        assert!(other.channels().is_empty() && other.sasl_username.is_empty());
        assert!(!other.sasl_enabled && !other.connect_on_startup);
        assert_eq!(other.ircv3, Ircv3Preferences::default());

        // Version 13 keeps each server's values and writes no shared identity.
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
    fn shared_identity_is_not_guessed_when_the_selection_is_unknown() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let server = |id: &str, host: &str| {
            serde_json::json!({"id": id, "custom": true, "host": host, "port": 6697,
                "use_tls": true, "encoding": "utf8"})
        };
        for (selected, servers, owner) in [
            // The selection names a server that is gone: nobody is guessed.
            (
                "custom-9",
                vec![
                    server("custom-1", "a.example"),
                    server("custom-2", "b.example"),
                ],
                None,
            ),
            // No selection at all.
            (
                "",
                vec![
                    server("custom-1", "a.example"),
                    server("custom-2", "b.example"),
                ],
                None,
            ),
            // A single remaining server is unambiguous.
            (
                "custom-9",
                vec![server("custom-1", "a.example")],
                Some("custom-1"),
            ),
        ] {
            let old = serde_json::json!({
                "version": 12, "selected_server": selected, "servers": servers,
                "nickname": "alice", "username": "ident", "channels": "#a",
                "sasl_enabled": true, "sasl_username": "account",
                "connect_on_startup": true
            });
            fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
            let settings = load_from(&path).unwrap().unwrap();
            for profile in &settings.servers {
                let owns = Some(profile.id.as_str()) == owner;
                assert_eq!(
                    profile.nickname == "alice",
                    owns,
                    "{selected} {}",
                    profile.id
                );
                assert_eq!(profile.channels == "#a", owns);
                assert_eq!(profile.sasl_enabled, owns);
                assert_eq!(profile.connect_on_startup, owns);
            }
        }
    }

    #[test]
    fn per_server_settings_survive_loading_even_when_identical() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut settings = Settings::default();
        for host in ["a.example", "b.example"] {
            let profile = settings.add_server(host);
            // The same values on purpose: they are not mistakes to undo.
            profile.nickname = "alice".into();
            profile.username = "ident".into();
            profile.channels = "#shared".into();
            profile.use_tls = true;
            profile.sasl_enabled = true;
            profile.sasl_username = "account".into();
            profile.remember_passwords = true;
            profile.connect_on_startup = true;
        }
        settings.servers[1].ircv3.batch = true;
        settings.servers[1].allow_plaintext_pass = true;
        for version in 13..=SETTINGS_VERSION {
            let mut file = serde_json::to_value(&settings).unwrap();
            file["version"] = version.into();
            fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
            let loaded = load_from(&path).unwrap().unwrap();
            assert_eq!(loaded.servers, settings.servers, "version {version}");
            save_to(&path, &loaded).unwrap();
            assert_eq!(load_from(&path).unwrap().unwrap().servers, settings.servers);
        }
    }

    #[test]
    fn new_settings_have_no_servers_and_presets_are_only_suggestions() {
        let mut settings = Settings::default();
        assert!(settings.servers.is_empty());
        assert!(settings.selected_profile().is_none());
        let added = settings.add_server(PRESETS[2].host);
        assert_eq!(added.host, "irc6.ircnet.ne.jp");
        assert!(PRESETS.iter().any(|p| p.host == "dev.ircnet.ne.jp"));
        assert_eq!(PRESETS[0].host, "irc.ircnet.com", "the default suggestion");
        let hosts: HashSet<_> = PRESETS.iter().map(|p| p.host).collect();
        assert_eq!(hosts.len(), PRESETS.len(), "no host is listed twice");
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
    fn experimental_logging_defaults_off_and_survives_reload() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(&path, r#"{"version":15}"#).unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.experimental, Experimental::default());
        settings.experimental = Experimental {
            debug_logging: true,
            stderr_file: Some(directory.path().join("日本語 debug.log")),
        };
        save_to(&path, &settings).unwrap();
        assert_eq!(
            load_from(&path).unwrap().unwrap().experimental,
            settings.experimental
        );
        settings.experimental.debug_logging = false;
        save_to(&path, &settings).unwrap();
        assert_eq!(
            load_from(&path).unwrap().unwrap().experimental,
            settings.experimental
        );
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
    fn saved_palette_colors_round_trip_and_are_bounded() {
        let mut appearance = Appearance::default();
        assert!(appearance.saved_colors.is_empty());
        appearance.save_color(" #ff8800 ").unwrap();
        appearance.save_color("#FF8800").unwrap();
        appearance.save_color("#00aa00").unwrap();
        assert_eq!(
            appearance.saved_colors,
            ["#FF8800", "#00AA00"],
            "no repeats"
        );
        assert!(appearance.save_color("orange").is_err());
        assert!(appearance.save_color("#12345").is_err());
        appearance.remove_saved_color("#ff8800");
        assert_eq!(appearance.saved_colors, ["#00AA00"]);
        for n in 0..MAX_SAVED_COLORS {
            let _ = appearance.save_color(&format!("#{n:06X}"));
        }
        assert_eq!(appearance.saved_colors.len(), MAX_SAVED_COLORS);
        assert!(appearance.save_color("#ABCDEF").is_err(), "full");
        assert!(appearance.save_color("#00AA00").is_ok(), "already there");
        assert!(appearance.validate().is_ok());
    }

    #[test]
    fn palette_is_empty_when_absent_and_cleaned_when_edited_by_hand() {
        let dir = std::env::temp_dir().join(format!("cayenchat-palette-{}", uuid::Uuid::new_v4()));
        let path = dir.join("settings.json");
        let mut settings = Settings::default();
        let mut value = serde_json::to_value(&settings).unwrap();
        value["appearance"]
            .as_object_mut()
            .unwrap()
            .remove("saved_colors");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert!(loaded.appearance.saved_colors.is_empty());

        settings.appearance.saved_colors = vec![
            "#aabbcc".into(),
            "nonsense".into(),
            "#AABBCC".into(),
            "#112233".into(),
        ];
        fs::write(&path, serde_json::to_vec(&settings).unwrap()).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert_eq!(loaded.appearance.saved_colors, ["#AABBCC", "#112233"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reiwa_mode_is_off_when_absent_and_reads_the_former_name() {
        let mut appearance = Appearance::default();
        assert!(!appearance.reiwa_mode);
        appearance.reiwa_mode = true;
        let mut saved = serde_json::to_value(&appearance).unwrap();
        let loaded: Appearance = serde_json::from_value(saved.clone()).unwrap();
        assert!(loaded.reiwa_mode);
        let object = saved.as_object_mut().unwrap();
        object.remove("reiwa_mode");
        let loaded: Appearance = serde_json::from_value(saved.clone()).unwrap();
        assert!(!loaded.reiwa_mode);
        saved
            .as_object_mut()
            .unwrap()
            .insert("header_line_messages".into(), true.into());
        let loaded: Appearance = serde_json::from_value(saved).unwrap();
        assert!(loaded.reiwa_mode);
    }

    #[test]
    fn ignores_the_removed_combined_log_name_width() {
        let mut saved = serde_json::to_value(Appearance::default()).unwrap();
        saved
            .as_object_mut()
            .unwrap()
            .insert("sub_log_name_width".into(), 0.into());
        let loaded: Appearance = serde_json::from_value(saved).unwrap();
        assert_eq!(loaded, Appearance::default());
        assert!(loaded.validate().is_ok());
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
    fn image_previews_default_off_for_old_and_new_settings_and_round_trip() {
        assert!(!Settings::default().appearance.image_previews);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old["version"] = 13.into();
        old["appearance"]
            .as_object_mut()
            .unwrap()
            .remove("image_previews");
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();

        let mut settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.version, SETTINGS_VERSION);
        assert!(!settings.appearance.image_previews);
        settings.appearance.image_previews = true;
        save_to(&path, &settings).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["version"], SETTINGS_VERSION);
        assert_eq!(saved["appearance"]["image_previews"], true);
        assert_eq!(load_from(&path).unwrap(), Some(settings.clone()));

        settings.appearance.image_previews = false;
        save_to(&path, &settings).unwrap();
        assert_eq!(load_from(&path).unwrap(), Some(settings));
    }

    #[test]
    fn compact_urls_default_off_for_old_settings_and_round_trip() {
        assert!(!Settings::default().appearance.compact_urls);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old["appearance"]
            .as_object_mut()
            .unwrap()
            .remove("compact_urls");
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();

        let mut settings = load_from(&path).unwrap().unwrap();
        assert!(!settings.appearance.compact_urls);
        settings.appearance.compact_urls = true;
        save_to(&path, &settings).unwrap();
        assert_eq!(load_from(&path).unwrap(), Some(settings));
    }

    #[test]
    fn decode_char_refs_default_off_for_old_settings_and_round_trip() {
        assert!(!Settings::default().appearance.decode_char_refs);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old["appearance"]
            .as_object_mut()
            .unwrap()
            .remove("decode_char_refs");
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();

        let mut settings = load_from(&path).unwrap().unwrap();
        assert!(!settings.appearance.decode_char_refs);
        settings.appearance.decode_char_refs = true;
        save_to(&path, &settings).unwrap();
        assert_eq!(load_from(&path).unwrap(), Some(settings));
    }

    #[test]
    fn url_tooltip_appearance_defaults_for_old_settings_and_is_validated() {
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        let appearance = old["appearance"].as_object_mut().unwrap();
        appearance.remove("url_tooltip_color");
        appearance.remove("url_tooltip_opacity");
        appearance.remove("url_tooltip_link_color");
        let dark = appearance["dark"].as_object_mut().unwrap();
        dark.remove("url_tooltip_color");
        dark.remove("url_tooltip_link_color");
        let settings: Settings = serde_json::from_value(old).unwrap();
        assert_eq!(settings.appearance, Appearance::default());
        assert_eq!(settings.appearance.url_tooltip_link_color, "#0645AD");
        assert_eq!(settings.appearance.dark.url_tooltip_link_color, "#78AAFF");
        assert_eq!(settings.appearance.url_tooltip_opacity, 80);

        let mut appearance = Appearance {
            url_tooltip_opacity: MIN_URL_TOOLTIP_OPACITY - 1,
            ..Appearance::default()
        };
        assert!(appearance.validate().is_err());
        appearance.url_tooltip_opacity = 101;
        assert!(appearance.validate().is_err());
        appearance.url_tooltip_opacity = 100;
        assert!(appearance.validate().is_ok());
        appearance.url_tooltip_color = "white".into();
        assert!(appearance.validate().is_err());
        appearance.url_tooltip_color = "#FFFFFF".into();
        appearance.url_tooltip_link_color = "blue".into();
        assert!(appearance.validate().is_err());
        appearance.url_tooltip_link_color = "#0645AD".into();
        appearance.dark.url_tooltip_link_color = "blue".into();
        assert!(appearance.validate().is_err());
    }

    #[test]
    fn link_colors_default_for_old_settings_round_trip_and_are_validated() {
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        let appearance = old["appearance"].as_object_mut().unwrap();
        appearance.remove("link_color");
        appearance.remove("link_background");
        let dark = appearance["dark"].as_object_mut().unwrap();
        dark.remove("link_color");
        dark.remove("link_background");
        let settings: Settings = serde_json::from_value(old).unwrap();
        assert_eq!(settings.appearance, Appearance::default());
        assert_eq!(settings.appearance.link_color, "#0645AD");
        assert_eq!(settings.appearance.dark.link_color, "#78AAFF");
        assert!(settings.appearance.link_background.is_empty());
        assert!(settings.appearance.dark.link_background.is_empty());

        let mut appearance = Appearance {
            link_background: "#FFEEAA".into(),
            dark: DarkColors {
                link_background: "#112233".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let json = serde_json::to_string(&appearance).unwrap();
        assert_eq!(
            serde_json::from_str::<Appearance>(&json).unwrap(),
            appearance
        );
        assert!(appearance.validate().is_ok());
        appearance.link_background = "yellow".into();
        assert!(appearance.validate().is_err());
        appearance.link_background.clear();
        appearance.dark.link_background = "#12".into();
        assert!(appearance.validate().is_err());
        appearance.dark.link_background.clear();
        appearance.link_color = String::new();
        assert!(appearance.validate().is_err());
    }

    #[test]
    fn ircv3_preferences_default_off_per_server_and_round_trip() {
        let mut fresh = Settings::default();
        assert_eq!(
            fresh.add_server("irc.example.org").ircv3,
            Ircv3Preferences::default()
        );
        assert!(!fresh.servers[0].ircv3.message_tags && !fresh.servers[0].ircv3.server_time);

        // A version 14 file has no IRCv3 preferences: both stay off.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut old = serde_json::to_value(&fresh).unwrap();
        old["version"] = 14.into();
        old["servers"][0].as_object_mut().unwrap().remove("ircv3");
        old["servers"].as_array_mut().unwrap().push(
            serde_json::json!({"id": "custom-2", "host": "irc.other.example",
                "port": 6697, "use_tls": true, "encoding": "utf8"}),
        );
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        assert_eq!(settings.version, SETTINGS_VERSION);
        assert!(
            settings
                .servers
                .iter()
                .all(|server| server.ircv3 == Ircv3Preferences::default())
        );

        // Each server keeps its own choices, and one is independent of the other.
        settings.servers[0].ircv3.server_time = true;
        settings.servers[1].ircv3.message_tags = true;
        save_to(&path, &settings).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            saved["servers"][0]["ircv3"],
            serde_json::json!({"message_tags": false, "server_time": true, "batch": false, "peer_avatars": false, "chathistory": false, "confirmed_sending": false, "accounts": false})
        );
        assert_eq!(
            saved["servers"][1]["ircv3"],
            serde_json::json!({"message_tags": true, "server_time": false, "batch": false, "peer_avatars": false, "chathistory": false, "confirmed_sending": false, "accounts": false})
        );
        assert_eq!(load_from(&path).unwrap(), Some(settings.clone()));

        // A version 15 file written before batch existed keeps its choices,
        // and batch reads as off.
        let mut old = saved.clone();
        old["servers"][0]["ircv3"]
            .as_object_mut()
            .unwrap()
            .remove("batch");
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert_eq!(loaded, settings);
        assert!(!loaded.servers[0].ircv3.batch && loaded.servers[0].ircv3.server_time);

        // Batch is per server too.
        settings.servers[1].ircv3.batch = true;
        save_to(&path, &settings).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert!(!loaded.servers[0].ircv3.batch && loaded.servers[1].ircv3.batch);

        // So is channel history, off when a file predates it.
        settings.servers[0].ircv3.chathistory = true;
        save_to(&path, &settings).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert!(loaded.servers[0].ircv3.chathistory && !loaded.servers[1].ircv3.chathistory);
        let mut old: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        old["servers"][0]["ircv3"]
            .as_object_mut()
            .unwrap()
            .remove("chathistory");
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        assert!(
            !load_from(&path).unwrap().unwrap().servers[0]
                .ircv3
                .chathistory
        );
        settings.servers[0].ircv3.chathistory = false;
        save_to(&path, &settings).unwrap();

        // The per-server avatar option of earlier builds is gone; files that
        // still have it load and keep everything else.
        let mut old: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        old["servers"][0]["ircv3"]["metadata"] = true.into();
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        assert_eq!(load_from(&path).unwrap(), Some(settings.clone()));
    }

    #[test]
    fn avatar_url_drafts_are_per_server_and_read_as_empty_when_absent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut settings = Settings::default();
        settings.add_server("irc.one.example");
        settings.add_server("irc.two.example");
        assert!(
            settings
                .servers
                .iter()
                .all(|server| server.avatar_url.is_empty())
        );
        settings.servers[0].avatar_url = "https://example.com/me/{size}.png".into();
        save_to(&path, &settings).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert_eq!(
            loaded.servers[0].avatar_url,
            "https://example.com/me/{size}.png"
        );
        assert_eq!(loaded.servers[1].avatar_url, "", "other server untouched");
        // A file written before the field existed loads with no draft and
        // keeps its other choices.
        let mut old: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        old["servers"][0]
            .as_object_mut()
            .unwrap()
            .remove("avatar_url");
        old["servers"][0]["ircv3"]["batch"] = true.into();
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert_eq!(loaded.servers[0].avatar_url, "");
        assert!(loaded.servers[0].ircv3.batch);
    }

    #[test]
    fn peer_avatars_default_off_and_share_nothing_until_chosen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut settings = Settings::default();
        settings.add_server("irc.one.example");
        settings.add_server("irc.two.example");
        assert!(
            settings
                .servers
                .iter()
                .all(|server| { !server.ircv3.peer_avatars && server.peer_avatar_url.is_empty() })
        );
        // Per server, and separate from the draft.
        settings.servers[0].ircv3.peer_avatars = true;
        settings.servers[0].peer_avatar_url = "https://example.com/me.png".into();
        settings.servers[0].avatar_url = "https://example.com/draft.png".into();
        save_to(&path, &settings).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert_eq!(loaded, settings);
        assert!(!loaded.servers[1].ircv3.peer_avatars);
        assert_eq!(loaded.servers[1].peer_avatar_url, "");
        // Files written before the fields existed read as off and empty.
        let mut old: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        old["servers"][0]
            .as_object_mut()
            .unwrap()
            .remove("peer_avatar_url");
        old["servers"][0]["ircv3"]
            .as_object_mut()
            .unwrap()
            .remove("peer_avatars");
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert!(!loaded.servers[0].ircv3.peer_avatars);
        assert_eq!(loaded.servers[0].peer_avatar_url, "");
        assert_eq!(
            loaded.servers[0].avatar_url,
            "https://example.com/draft.png"
        );
    }

    #[test]
    fn a_file_with_the_removed_invisible_mark_switch_still_loads() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut settings = Settings::default();
        settings.add_server("irc.one.example");
        save_to(&path, &settings).unwrap();
        let mut old: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        old["servers"][0]["peer_avatar_invisible_mark"] = serde_json::Value::Bool(false);
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert_eq!(loaded, settings);
    }

    #[test]
    fn user_avatars_are_per_server_default_off_and_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut settings = Settings::default();
        settings.add_server("a.example");
        settings.add_server("b.example");
        assert!(settings.servers.iter().all(|s| !s.user_avatars));
        settings.servers[0].user_avatars = true;
        save_to(&path, &settings).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(saved["appearance"].get("user_avatars").is_none());
        let loaded = load_from(&path).unwrap().unwrap();
        assert_eq!(
            loaded
                .servers
                .iter()
                .map(|s| s.user_avatars)
                .collect::<Vec<_>>(),
            [true, false]
        );
    }

    #[test]
    fn regain_ops_defaults_on_and_round_trips() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut settings = Settings::default();
        settings.add_server("a.example");
        settings.add_server("b.example");
        assert!(settings.servers.iter().all(|s| s.regain_ops));
        settings.servers[0].regain_ops = false;
        save_to(&path, &settings).unwrap();
        let mut saved: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert_eq!(
            loaded
                .servers
                .iter()
                .map(|s| s.regain_ops)
                .collect::<Vec<_>>(),
            [false, true]
        );
        // A file from before the setting reads as on.
        for server in saved["servers"].as_array_mut().unwrap() {
            server.as_object_mut().unwrap().remove("regain_ops");
        }
        fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert!(loaded.servers.iter().all(|s| s.regain_ops));
    }

    #[test]
    fn old_global_user_avatars_is_copied_to_servers_once() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut settings = Settings::default();
        settings.add_server("a.example");
        settings.add_server("b.example");
        for global in [true, false] {
            let mut old = serde_json::to_value(&settings).unwrap();
            old["appearance"]["user_avatars"] = global.into();
            for server in old["servers"].as_array_mut().unwrap() {
                server.as_object_mut().unwrap().remove("user_avatars");
            }
            fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
            let mut loaded = load_from(&path).unwrap().unwrap();
            assert!(loaded.servers.iter().all(|s| s.user_avatars == global));
            // Servers added afterwards start off.
            assert!(!loaded.add_server("c.example").user_avatars);
            loaded.servers[0].user_avatars = !global;
            save_to(&path, &loaded).unwrap();
            let saved: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert!(saved["appearance"].get("user_avatars").is_none());
            let again = load_from(&path).unwrap().unwrap();
            assert_eq!(
                again
                    .servers
                    .iter()
                    .map(|s| s.user_avatars)
                    .collect::<Vec<_>>(),
                [!global, global, false]
            );
        }
        // A file with an old value and no servers loads and saves.
        let mut empty = serde_json::to_value(Settings::default()).unwrap();
        empty["appearance"]["user_avatars"] = true.into();
        fs::write(&path, serde_json::to_vec(&empty).unwrap()).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert!(loaded.servers.is_empty());
        save_to(&path, &loaded).unwrap();
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

    /// Default settings as a version 1–12 file wrote them: the IRCnet
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
    fn changed_shortcuts_are_saved_by_action_and_absent_means_the_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(&path, r#"{"version":15,"selected_server":"","servers":[]}"#).unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        assert!(
            settings.keybindings.is_empty(),
            "absent reads as no changes"
        );
        settings
            .keybindings
            .insert("next_channel".into(), "cmd-}".into());
        save_to(&path, &settings).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert_eq!(loaded.keybindings["next_channel"], "cmd-}");
        // Unknown entries are kept as they are: they mean something to a
        // newer version, and the application ignores them when binding.
        fs::write(
            &path,
            r#"{"version":15,"selected_server":"","servers":[],"keybindings":{"later_action":"ctrl-x"}}"#,
        )
        .unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert_eq!(loaded.keybindings["later_action"], "ctrl-x");
    }

    #[test]
    fn a_test_build_can_keep_its_directory_between_launches() {
        use std::ffi::OsString;

        assert_eq!(fixed_test_directory(None), Ok(None));
        assert_eq!(fixed_test_directory(Some(OsString::new())), Ok(None));
        assert!(fixed_test_directory(Some("relative/dir".into())).is_err());
        let base = tempfile::tempdir().unwrap();
        let nested = base.path().join("a").join("b");
        let made = fixed_test_directory(Some(nested.clone().into_os_string())).unwrap();
        assert_eq!(made, Some(nested.clone()));
        assert!(nested.is_dir());
        // Using it again keeps what is in it.
        fs::write(nested.join("window.json"), "kept").unwrap();
        fixed_test_directory(Some(nested.clone().into_os_string())).unwrap();
        assert_eq!(
            fs::read_to_string(nested.join("window.json")).unwrap(),
            "kept"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&nested).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "readable only by the user");
            // A directory that existed already is refused when others can
            // read it (it would hold the credentials file), and accepted once
            // it is private.
            fs::set_permissions(&nested, fs::Permissions::from_mode(0o755)).unwrap();
            let refused = fixed_test_directory(Some(nested.clone().into_os_string()));
            assert!(
                refused
                    .as_ref()
                    .is_err_and(|error| error.contains("chmod 700")),
                "{refused:?}"
            );
            fs::set_permissions(&nested, fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(
                fixed_test_directory(Some(nested.clone().into_os_string())),
                Ok(Some(nested.clone()))
            );
        }
    }

    #[test]
    fn the_window_layout_is_restored_unless_switched_off() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(&path, r#"{"version":15,"selected_server":"","servers":[]}"#).unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        assert!(settings.restore_window_layout, "absent reads as on");
        settings.restore_window_layout = false;
        save_to(&path, &settings).unwrap();
        assert!(!load_from(&path).unwrap().unwrap().restore_window_layout);
    }

    #[test]
    fn quitting_is_confirmed_only_when_switched_on() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(&path, r#"{"version":15,"selected_server":"","servers":[]}"#).unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        assert!(!settings.confirm_quit, "absent reads as off");
        settings.confirm_quit = true;
        save_to(&path, &settings).unwrap();
        assert!(load_from(&path).unwrap().unwrap().confirm_quit);
    }

    #[test]
    fn amazon_url_shortening_is_on_unless_switched_off() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(&path, r#"{"version":15,"selected_server":"","servers":[]}"#).unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        assert!(settings.shorten_amazon_urls, "absent reads as on");
        settings.shorten_amazon_urls = false;
        save_to(&path, &settings).unwrap();
        assert!(!load_from(&path).unwrap().unwrap().shorten_amazon_urls);
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

    #[test]
    fn always_on_top_is_opt_in_for_existing_settings_and_round_trips() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(&path, r#"{"version":15,"selected_server":"","servers":[]}"#).unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        assert!(!settings.always_on_top, "absent reads as off");
        settings.always_on_top = true;
        save_to(&path, &settings).unwrap();
        assert!(load_from(&path).unwrap().unwrap().always_on_top);
        settings.always_on_top = false;
        save_to(&path, &settings).unwrap();
        assert!(!load_from(&path).unwrap().unwrap().always_on_top);
    }

    #[test]
    fn the_menu_bar_is_shown_unless_auto_hide_is_chosen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(&path, r#"{"version":15,"selected_server":"","servers":[]}"#).unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        assert!(!settings.menu_bar_auto_hide, "absent reads as always shown");
        settings.menu_bar_auto_hide = true;
        save_to(&path, &settings).unwrap();
        assert!(load_from(&path).unwrap().unwrap().menu_bar_auto_hide);
    }

    #[test]
    fn the_toolbar_is_shown_unless_hidden() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(&path, r#"{"version":15,"selected_server":"","servers":[]}"#).unwrap();
        let mut settings = load_from(&path).unwrap().unwrap();
        assert!(settings.show_toolbar, "absent reads as shown");
        settings.show_toolbar = false;
        save_to(&path, &settings).unwrap();
        assert!(!load_from(&path).unwrap().unwrap().show_toolbar);
    }
}
