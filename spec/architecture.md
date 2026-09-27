# Architecture

This document describes the intended architecture. Keep it concise and update it when major boundaries change.

## Core rule

IRC/network logic must not depend on the GUI framework.

A plausible initial workspace is:

```text
crates/
  model/
  irc-core/
  storage/
  upload/
  app/
  ui/
```

The exact crate boundaries may evolve as implementation experience accumulates.

## Responsibilities

### model

Small domain types shared by the application, such as networks, conversations, users, messages, IDs, and connection state.

Keep dependencies minimal.

### irc-core

IRC transport, protocol handling, capability negotiation, TLS, reconnect behavior, and protocol-facing state.

Must not depend on GPUI.

### storage

Persistent configuration, preferences, and history/log storage.

Persistent formats should be explicit and versionable.

Also owns the application's only credential store (`storage::credentials`).
Secrets never enter the preferences file.

### upload

IRC's external image hosting: the `ExternalUploader` trait, provider
registry, provider implementations (ImgBB) and a fake for tests. It is not
part of the IRC wire implementation and must not depend on GPUI or `irc`.
Protocols with native media must not route through it.

### app

Application state and orchestration: selected conversation, unread state, commands, event routing, network lifecycle, and services.

Must not contain rendering code.

### ui

GPUI desktop interface.

Consumes application state and emits application commands. Protocol details should not leak into UI components unless they are genuinely user-visible IRC concepts.

## Event flow

Prefer message passing over shared mutable state.

```text
network tasks
    |
    v
IRC events
    |
    v
application state
    |
    v
GPUI rendering

GPUI actions
    |
    v
application commands
    |
    v
network tasks
```

Tokio is a likely runtime for networking, but the integration strategy should be validated against current GPUI behavior before being treated as final.

## Platform boundary

macOS and Windows are equal primary targets.

Platform-specific behavior should sit behind narrow interfaces. Do not introduce a macOS-only assumption into core or application state for convenience.

## UI structure

The initial UI should be structurally close to a traditional desktop IRC client:

```text
+--------------------------------------+--------------------+
| selected-channel main log            | selected users     |
|                                      |                    |
|--------------------------------------|--------------------|
| draft input                          | network/channel    |
|--------------------------------------| tree               |
| other-channel subwindow              |                    |
+--------------------------------------+--------------------+
```

The main log, other-channel subwindow, user list and channel tree are the four
information panes. The left logs stack around the draft input; the right side
stacks users over the channel tree. The subwindow shows messages from conversations
other than the selected one, with a direct jump to the source conversation. Its
channel/server labels are single-line and ellipsize within their column. Channel
messages have an application arrival sequence so the combined subwindow shows
the actual latest line last even when several channels receive messages within
the same displayed minute. Channel activity (JOIN, PART, QUIT and MODE) shares
the channel arrival sequence, renders in English as a timestamped line without
a nickname column regardless of the UI language, does not mark the channel
unread, and appears only in its channel's main log, never in the combined
subwindow. QUIT is logged only in channels whose last published roster contained
the quitting nickname. Activity text uses configurable green (`#007D00`) by
default. The main log keeps separate scroll positions for each server or
channel, and both left logs follow incoming messages while at the bottom. User scrolling pauses follow mode
until the bottom is reached again, including during initial IRC history bursts.
Both logs, the user list and the channel tree are virtualized (GPUI `list` /
`uniform_list`), so
each redraw lays out only rows near the viewport. GPUI re-renders the whole chat
window whenever the draft input changes, so rebuilding every retained line made
typing, IME composition and channel switching slow, most visibly on Linux. The
two logs, the user list and the channel tree are therefore separate views drawn
with `AnyView::cached`: a keystroke redraws only the window shell and the input,
while each pane reuses its previous layout and paint until the chat window is
notified of a state change (or the pane itself scrolls or changes hover state).
State changes that affect panes must call `cx.notify()` on the chat window.
GPUI still reshapes rows it did not draw in the previous frame; on Linux the
vendored GPUI caches cosmic-text shaping per word (bounded) so a switch does not
pay full shaping cost for every newly visible line (issue #5). The
combined subwindow shows the newest 1,000 lines across other channels; it is
rebuilt only when a message arrives or the selection changes, and log lists
replace only the rows whose messages appeared or disappeared, so switching
channels keeps the measured heights of lines shown before and after. Channel
navigation commands are independent of GPUI. The UI binds macOS shortcuts from the
reference and platform-specific Windows/Linux alternatives; text editing remains
scoped to the focused draft. Ctrl+Tab / Ctrl+Shift+Tab visit unread channels only.
The performance measures above, the current resource bounds and the
measurement baseline are listed in `spec/performance.md`; keep them when
adding servers or media.

The visual treatment should follow `spec/project.md`: compact, direct, and Chocoa-like rather than resembling a modern consumer messenger.

## Current implementation

The six workspace members use the `cayenchat-` package prefix. Dependencies include
`ui -> app -> model`, `ui -> irc-core`, `ui -> storage`, `ui -> notify-rust`, `ui -> upload ->
storage/model`, `storage -> keyring`, `upload -> ureq` and `irc-core -> irc/Tokio`.
There are no dependency cycles and GPUI occurs only in `ui`.

`irc-core::Connection` owns a dedicated current-thread Tokio runtime. It translates
the `irc` crate's messages into owned application events and accepts bounded outgoing
commands. TCP/TLS connection and registration do not block GPUI. Each connected
server has its own `Connection`, thread and runtime. The UI moves each
connection's `Events` stream into its own GPUI task that sleeps until the worker
sends something, applies up to 256 events per update to `app::AppState` for that
server's network, redraws, and yields between batches of a burst, so a busy
server cannot hold back another's lines beyond one batch. An earlier 50 ms poll handled at most 64
events per tick (about 640 incoming lines per second, since each line also
produces a wire diagnostic), delayed every line and woke 20 times a second while
idle. QUIT and NICK republish rosters only for channels that contained the user.
Before a TLS connection, the core installs rustls's ring crypto provider as the
process default. The GUI dependency graph enables both ring and aws-lc-rs, so
rustls cannot infer a provider from crate features alone.
The UI loads versioned preferences from the platform user configuration directory.
The reader accepts versions 1 through the current writer version on every OS.
A future version is rejected without rewriting the file and reports both the
version range and path, with guidance to update the app. Missing or invalid
version fields instead suggest restoring a valid backup. Settings opening stays
blocked on a read error so defaults cannot overwrite unreadable preferences.
Version 5 adds appearance colors, alternating log rows, and per-pane font choices;
version 6 adds an opt-in startup connection flag, and version 7 adds a language
preference. Version 8 renames the appearance background to member-list background
and changes its default to white; the old default gray migrates to white while
custom saved colors remain. Version 9 changes the default channel event text color
to `#007D00`; the previous default migrates while custom colors remain. Version 10
adds a theme preference (System, Light or Dark), a dark-theme set of the six pane
colors beside the existing light ones, and a Linux display server choice; older
settings default to System, the dark defaults and Wayland. Version 11 adds the
`USER` username (migrated from the old nickname, which earlier versions sent as
USER), the credential backend choice and the image upload provider, and stops
reading passwords from the file except to migrate them. Version 12 adds
notification preferences (enabled, mentions, keyword alerts, keywords and
private messages) and a highlight color to the light and dark pane colors;
older settings enable them all with no keywords. Version 13 moves the
nickname, username, channels, SASL account and startup connection into each
server profile (D017). The UI keeps the
effective colors in a GPUI global `Theme`: System follows the appearance GPUI
reports (macOS/Windows appearance, or the XDG desktop portal color scheme on
Linux) and switches live when it changes. Native title bars on macOS and Windows
follow the operating system, so they can differ from an explicitly chosen app
theme. On Linux the display choice is applied at startup before GPUI picks a
backend: X11 removes `WAYLAND_DISPLAY` when an X display exists, so GNOME draws a
themed title bar through XWayland at the cost of XIM input and blurrier
fractional scaling. `CAYENCHAT_DISPLAY=x11|wayland` overrides the setting. When the compositor offers no
`xdg-decoration` (GNOME), GPUI reports client-side decorations (a local GPUI
patch; upstream wrongly kept `Server`) and every window draws an Adwaita-like
frame: centered bold title, round window buttons, rounded top corners, a shadow
margin with resize handles and a dimmed backdrop state, in the active theme's
light or dark colors. The XDG desktop portal supplies GNOME's `button-layout`,
`action-double-click-titlebar` and interface font, and changes apply live.
Server-decorated windows (macOS, Windows, X11, KDE and other compositors with
`xdg-decoration`) are unchanged. The
member-list color no longer affects the input bar
or the window root. Missing startup-connection values default to disabled, and older
settings default to System language; existing saved choices remain intact. The UI resolves the system language with `sys-locale` (Japanese or
English fallback), loads `locales/ja.json` or `locales/en.json` beside the app or
from its resource directory, and falls back to bundled catalog content. Saving a
language choice updates both windows and native menu labels without reconnecting.
The four-pane chat window stays open while a separate settings
window offers Connection and Appearance tabs. With startup connection enabled,
the UI validates the saved selected profile and connects without opening settings;
invalid saved connection details open settings with feedback. Only explicitly
saved credentials are available at startup. The macOS
application menu and Command+, open that window;
Windows/Linux use Ctrl+, or an in-window menu bar revealed by pressing Alt alone,
by F10, or by resting the pointer for 0.4 s in a 6 px strip at the top of the
content. Alt alone is commonly an IME on/off key, in which case the IME consumes
it and the app never sees it, so F10 and hover are the reliable routes. A
hover-revealed bar slides in and hides again once the pointer moves more than
12 px below it, unless a menu is open. Hover reveal is not a Windows/GNOME
convention (it resembles macOS full-screen menus).
The bar shares native menu definitions and actions, supports arrows/Enter/Escape,
and preserves input focus for editing commands. Alt chords do not toggle it;
selecting an action or clicking outside dismisses it. Action availability is
queried only after the menu is revealed: GPUI has no rendered dispatch tree
during the first frame, so querying it while the initially hidden menu renders
would panic on startup. macOS retains native menus.
Passwords persist per server only when password saving is on for that server,
and only through the credential store (see Credentials below); turning saving
off immediately removes stored values. TLS certificate verification
defaults to on per server and can be disabled for a specific connection. The core
requires TLS before sending either server PASS or SASL PLAIN credentials. The nickname (`NICK`), the `USER` username and the SASL account are separate
settings; `ConnectionConfig` carries the username explicitly and its `Debug`
output redacts both passwords. Each core connection supports auto-join, channel
messages, NAMES snapshots, `PRIVMSG`/`NOTICE`, and `/` commands; the UI runs one
per connected server.
Channel target validation is shared by the core and the UI's outgoing-message
routing and accepts `#`, `&` and IRCnet `!` channels.
Safe-channel short names are sent unchanged in JOIN; the server's returned full
name (including its five-character identifier) is used for the conversation,
roster, messages and subsequent commands, and is preserved in WHOIS channel links.
Its SASL state machine negotiates CAP, sends PLAIN credentials, and waits for
success before ending CAP negotiation. The UI retains each server's active connection
configuration and retries its unexpected disconnections after 3, 6, 12, 24, then 30
seconds (capped), independently of the other servers.
The core allows 15 seconds for TCP/TLS and 90 seconds for registration (001):
IRCnet holds registration about 30 seconds when a client's ident port 113
silently drops packets, which a 30-second limit turned into a reconnect loop.
A successful registration resets the delay; an explicit disconnect cancels pending
retries. The core reports a terminal `Refused` event instead of `Disconnected`
for SASL failures, a missing SASL PLAIN offer, UTF8ONLY with a legacy encoding,
and 464/465 during registration; the UI does not retry those automatically,
because repeating rejected credentials risks account lockout or a server ban.
The `irc` library consumes 432/433 and reports `NoUsableNick` because no
alternate nicknames are configured; the stream stays usable afterwards. Before
registration the core turns it into `NicknameRejected`, pauses its registration
timeout and keeps the link open. The UI adds a row for that server to the
nickname dialog, prefilled with the rejected nick plus `_` (see Servers and
sessions); submitting sends NICK (or reconnects if the server
closed the link, which the core reports as `Refused` so it is not retried with
the same nick) and replaces the nickname for this session's reconnects without
changing saved settings. Disconnect closes that server's connection. After registration a rejected
`/nick` only produces a server line instead of dropping the connection.
Reconnection preserves the in-memory conversation logs and drafts. A
complete membership event reducer remains future work.
The core emits fresh member snapshots after NAMES completion and incoming JOIN,
PART, KICK, QUIT, NICK and channel MODE changes. Application state sorts each
snapshot with operators first and case-insensitive nickname order within each
group. The member context menu routes Whois, invite and +o/-o through validated
IRC commands; private-message composition sends directly to the selected nick.
The channel tree context menu sends `/join` or `/part` for the clicked channel;
only the action matching its current joined state is enabled while registered.
The core merges WHOIS numerics (311–319, 330, 301 while pending, and other
WHOIS-only lines) per nickname and emits one `Whois` event at end-of-WHOIS (318);
the raw lines still reach the server log. The UI opens a separate WHOIS window
only for nicknames this client requested, because a bouncer such as Tiarra relays
replies to every attached client; an open window for the same nick is updated and
raised instead. The window offers private message, a channel dropdown with join for the selected
channel,
update (re-sends WHOIS) and close/Escape. A 318 without 311 reports that the nick
is offline. The chat window pushes its joined-channel set into WHOIS windows;
they must not read the chat window while rendering, because opening a window
renders synchronously inside the chat window's update and re-entry aborts on
macOS.
The core preserves displayed rank across nick changes because the current IRC
library drops user access levels on rename; a later MODE or completed NAMES
snapshot replaces that carried rank.

The upper channel log shapes each message body as selectable text, maps mouse
positions through GPUI's text layout, and opens recognized HTTP(S) URLs on a
double-click. The lower combined log switches to a line's channel on double-click.

Version 4 settings keep several server profiles. Since version 13 the list
starts empty: the IRCnet hosts are `storage::PRESETS`, offered only when adding
a server, and every profile is editable and removable in the order added.
Host/port/TLS/certificate verification/encoding are per
profile; version 13 moves the nickname, `USER` username, auto-join channels,
SASL account and startup connection into each profile too (see Servers and
sessions below). Versions 1–3
are migrated on load, as are version 4 settings. `irc-core` passes the profile's encoding to the `irc` line
codec so protocol parameters, including channel names, and message text use the
same wire charset. It strictly validates outgoing encoding and wire length before
queueing, avoiding silent replacement and preserving a draft on rejection. A
legacy-encoding connection disconnects if the server advertises `UTF8ONLY`.
Connection-stage diagnostics and directional, decoded IRC lines flow as owned
events from `irc-core` to the UI. The transcript includes message bodies, masks
known credential commands, and retains the latest 1,000 entries in memory for
display and clipboard export. Library-generated PONG and configured JOIN lines
are reflected when their triggering server lines are processed. This is a parsed
IRC transcript, not a byte-for-byte socket capture. The UI shows it automatically
while registration is incomplete or after disconnection, including over a selected
channel. A worker panic produces a disconnected event; the UI also handles a closed
event channel with no terminal event. A terminal disconnection reason is included
in the transcript and clipboard export.
Connection diagnostics are reached through the View menu (Alt, F10 or hovering
below the title bar reveals the menu bar on Linux/Windows) or keyboard shortcuts. There are no permanent diagnostic
buttons. Displaying diagnostics selects the server view, enables the transcript
and scrolls to its start; copying exports the retained transcript regardless of
the selected pane.

### Servers and sessions

Every server profile in the settings is a network in the channel tree,
connected or not, in the order added. With no servers the selection is
`Selection::None`: the tree is empty, the main log explains how to add one
and settings open at startup. `ui::session::ServerSession`
holds everything per connection: the `Connection`, the configuration reused by
reconnects, retry state, the current nickname, pending WHOIS nicknames and a
bounded transcript (1,000 lines per server). The chat window keys sessions by
`NetworkId`; a profile keeps its network for the whole run. Events carry no
network, so each event pump applies its batch to the network it was started
for. Context menus, prompts and WHOIS windows remember their network and act
on that server only. Rejected nicknames get one row per server in a single
dialog, in arrival order: each row names the server, pre-fills the rejected
nick plus `_`, and offers Retry or Disconnect; a repeated rejection updates
the server's row and registration closes it. A new row takes focus unless
another nickname field already has it, and Enter submits the focused row. Menu commands
(Disconnect, Reconnect, Show/Copy diagnostics) act on the selected server; the
settings window's Connect and Disconnect act on the server being
edited. Settings save automatically as they change (D018); saving adds,
renames or removes networks: a removed server is
disconnected and its conversations, drafts and scroll state are dropped.
Connecting a server replaces only that server's conversations with its
configured channels; the other servers keep their logs. All servers marked
Connect when the app starts connect at launch. Servers not used in this run
show no connection mark and offer Connect instead of Reconnect in their
context menu; connecting one uses its saved settings and stored passwords, or
opens the settings on that server when they are incomplete.
There is no application-wide bound on logs, conversations or transcripts yet;
see `performance.md`.

`app::AppState` owns networks, conversations, bounded message logs, user lists,
connection status, selection, unread IDs, and active IDs. Only configured channels
and our own JOINs create conversations (at most 1,000 per network); messages for
any other channel go to the bounded server log, and member snapshots or activity
for unknown channels are ignored, so a hostile server cannot grow memory without
limit. The core likewise caps unfinished WHOIS replies (32 nicknames, 512
channels or extra lines each) and caches rosters only for joined channels. Typed `NetworkId` and
`ConversationId` distinguish identity from display names. It also retains an offline
mock for tests. `ui::ChatWindow` maps GPUI key actions to
navigation and send commands. GPUI entities retain separate server/channel draft
editing, selection, nickname completion and IME state. No `irc` library types enter
application state or rendering components.

## Notifications

```text
irc-core Event::ChannelMessage { mentioned } / Event::PrivateMessage
        |   (irc-core::text: nickname word match, formatting and ACTION)
        v
ui::ChatWindow::notify_message -> app::notifications::IncomingMessage
        |   (plain text, channel/private, notice, from_self, mentioned)
        v
app::notifications::NotificationRules::trigger  (GPUI- and protocol-free:
        |                          mention / keyword / private message)
        v
ui::ChatWindow  (skip the visible conversation, burst limit)
        |
        v
ui::notifier worker thread -> notify-rust
        +-- Linux/BSD: org.freedesktop.Notifications over zbus (body escaped)
        +-- macOS: NSUserNotificationCenter via mac-notification-sys
        +-- Windows: WinRT toast via tauri-winrt-notification
```

`irc-core` reports a PRIVMSG or NOTICE from a user mask to our nickname as
`PrivateMessage`; server notices and CTCP requests other than ACTION stay
server lines. Private messages notify only as PRIVMSG, because private NOTICEs
are usually services or bots. Mentions and keywords are separate choices.
`irc-core` sets `mentioned` when someone else names our nickname as a whole
word (RFC 1459 case mapping, formatting ignored), which also covers `nick:`
and `@nick`; keywords are case-insensitive substrings, so the nickname is
matched inside other words only if the user adds it as a keyword. The rules
only receive the `mentioned` flag: a Matrix adapter would set it from the
event's intentional mentions (`m.mentions`) and push rules instead of parsing
text, so no `@`-specific rule is needed in `app`. Our own messages never
notify (bouncer echoes). Nothing notifies while the chat window is
focused and the message's conversation (the server view for private messages)
is selected. At most five notifications are shown per ten seconds so bouncer
history playback cannot flood the desktop; the log and unread marks are
unaffected. Showing can block (D-Bus, macOS delivery confirmation), so a
dedicated thread does it; UI tests record notifications instead.

The same matches are shown in the logs, whether or not notifications are on:
`irc-core::text::mention_ranges` and `app::notifications::keyword_ranges`
give byte ranges that the main and sub logs draw bold in the theme's highlight
color (light `#D46A8E`, dark `#EFA0BE` by default, matching the pastel pane
colors). A channel that receives a mention or keyword while it is not selected
is recorded in `AppState::highlighted`, and the channel tree draws its name in
the highlight color until it is selected.

## Credentials

```text
settings / IRC connect / image upload UI
        |
        v
ui::secrets (GPUI global holding the one CredentialStore)
        |
        v
storage::credentials::CredentialStore
        |
        +-- SystemBackend::shared() (one per process, cached)
        |     macOS: VaultBackend, every secret in one Keychain item `secrets`
        |     else:  EntryBackend, one Credential Manager / Secret Service
        |            entry per secret
        |       -> EntryStore -> keyring crate (zbus for Secret Service)
        +-- LocalFileBackend: credentials.json, 0600, atomic replace
        +-- MemoryBackend: tests
```

Secrets are addressed by `SecretKey`, whose names come from stable internal
IDs: `connection/<profile-id>/server-password`,
`connection/<profile-id>/sasl-password` and
`uploader/<provider-id>/<account>/credential`. Profile IDs are the existing
`ircnet`, `ircnet-ipv6` and legacy `custom-N` IDs remain unchanged. New custom
profiles use `custom-<UUID v4>` so deleting and adding a profile before saving,
or a failed credential cleanup, cannot make a new server inherit old passwords.
Saving settings deletes the secrets of removed profiles. `Secret` redacts
its `Debug` output and zeroes its buffer on drop (best effort). Credential
errors are mapped to sanitized text; keyring payloads, which may contain secret
bytes, are dropped. The settings file records only the backend choice
(`credential_backend`) and per-profile `remember_passwords`.

The system backend reads each store entry at most once per process and keeps
the values in memory, so reconnecting or opening settings does not touch the
store again; unchanged values are not written back, and failed reads are not
cached so a refused prompt can be retried. On macOS the legacy Keychain asks
for the login password per item whenever the app's signature is not on the
item's access list, which for the ad hoc signed builds means after every
update, so `VaultBackend` keeps all secrets as one JSON map (same layout as the
local file) in the item `secrets`: one prompt per launch at most. Secrets that
earlier versions stored as separate items move into it the first time they are
read (a missing item is looked up without a prompt), and setting or deleting a
secret also removes its separate item. Other platforms keep one entry per
secret, because Credential Manager limits entry size and Secret Service
unlocks a whole collection at once.

The backend is chosen explicitly (settings version 11, default System). Opening
never falls back to another backend. The Credential Storage tab probes the
system store on a background task (D-Bus may be slow) and offers the local file
when it is unavailable; switching requires confirmation for the local file and
moves known secrets (`migrate` copies all, then deletes the originals). On
startup, plaintext passwords read from version 10 and earlier move into the
store before any window opens; only if the system store is unavailable do they
go to the local file, because the user had already accepted plaintext storage
for them. The settings form never shows saved passwords: fields start empty,
typing replaces the saved value, and a typed value wins for the connection.

The UI's stderr logger keeps `irc`, `ureq`, `rustls` and keyring crates at warning
level even under `RUST_LOG=trace`, including the IRC library's raw PASS and
AUTHENTICATE output. There are no crash diagnostics; the copied
connection transcript masks credential commands as before.

## Attachments and image sharing

```text
image paste (draft TextInput propagates image-only Paste)   file drop on draft row
                         \                                   /
                          v                                 v
                 ui::image_upload -> model::Attachment (sniffed, 32 MiB guard)
                          |
                          v
           app::attachments::AttachmentFlow  (GPUI-free state machine)
             offer -> Configure | Reconnect | Confirm | Busy
             confirm -> UploadJob (once) ; finish -> InsertLink | Failed | Ignored
                          |
                          v   (IRC only)
           upload::ExternalUploader (background executor, blocking HTTPS)
                          |
                          v
           link inserted at the originating draft's cursor; never sent
```

Text on the clipboard always pastes as text; only an image-only clipboard in a
draft reaches the flow. Paste and drop share the flow and the prompts
(confirmation naming the provider and the public-link consequences, setup
guidance, reconnect guidance after an authentication failure). The flow allows
one upload at a time; cancelling abandons the wait but cannot recall a request
already sent. The UI knows providers only by registry ID and display name.

Matrix support, if added, keeps the shared parts (`model::Attachment`, the
selection-to-attachment UI actions and the confirmation/progress presentation)
but replaces the transport step with the Matrix client's native media upload
producing an `m.image` event. It must not implement or call
`ExternalUploader`, and `upload` must not grow Matrix types. Inline display of
image links is not implemented; it should consume URLs (IRC) or media events
(Matrix) through a separate display layer, with a remote-loading preference,
HTTP(S)-only fetching, size and redirect limits, and decoding off the UI thread.
