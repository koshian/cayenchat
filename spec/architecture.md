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

### media

Inline media display without GPUI or protocol types: which links are image
preview candidates and which explicitly supplied avatar URLs may be fetched
(`policy`), fetching (`fetch::Fetcher`; `HttpFetcher` for public HTTP(S)
links), decoding into small thumbnails (`decode`), and the bounded record of
loads (`cache::PreviewCache`, one instance for previews and a separate small
one for avatars). It does not upload and does not use `upload`.

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
the same displayed minute. Channel activity (JOIN, PART, QUIT, NICK and MODE) shares
the channel arrival sequence, renders in English as a timestamped line without
a nickname column regardless of the UI language, does not mark the channel
unread, and appears only in its channel's main log, never in the combined
subwindow. QUIT and NICK (ours included) are logged only in channels whose last
published roster contained the nickname. Activity text uses configurable green (`#007D00`) by
default. The main log's nickname column fits 15 typical characters; longer
nicknames end in an ellipsis, or wrap when the Appearance setting is on. The
main log keeps separate scroll positions for each server or
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
scoped to the focused draft; Up/Down there recall the last 20 sent drafts
(in memory only, shared by all conversations, but browsing ends when the
conversation changes). Commands that carry credentials (to NickServ or
ChanServ, `/oper`, `/pass`, and `/raw` forms of those) are never kept. Ctrl+Tab / Ctrl+Shift+Tab visit unread channels only.
The performance measures above, the current resource bounds and the
measurement baseline are listed in `spec/performance.md`; keep them when
adding servers or media.

Pane boundaries use `ui::splitter`: a thin handle that turns pointer drags
into a pane size within the owner's minimum and maximum, measured from where
the button went down (GPUI starts a drag only after a small movement). The
owner stores the size; the handle draws and measures nothing else. The chat
window uses it for the boundary between the left column and the right column
(members over channel tree; 160 px at least, and the left column keeps 360 px)
and for the one between the member list and the channel tree (80 px at least
each; an even split until first dragged). The boundary between the two logs
keeps its own handle, which also moves it with the pointer within limits and
resets on a double click. Sizes are not saved yet (issue #57).

`ui::color_picker::ColorPicker` is a saturation/brightness square, a hue bar
and a `#RRGGBB` field that follow each other (GPUI has no color picker). It
emits `ColorChanged` when the user picks or types a complete color;
`set_color` shows a color the owner changed without emitting. Greys keep the
hue the bar had. In the Appearance settings, the swatch beside each color
field opens one picker below its row (one at a time) together with the saved
palette: picking writes the field, typing in the field moves the picker, a
palette color is applied by a click and removed by a right-click, and "Save to
palette" adds the picked color (24 at most, no repeats). Each picker acts only
on its own drags: GPUI delivers a
drag's moves to every element that listens for that drag type, so the drag
carries the picker's entity id.

The visual treatment should follow `spec/project.md`: compact, direct, and Chocoa-like rather than resembling a modern consumer messenger.

## Current implementation

The seven workspace members use the `cayenchat-` package prefix. Dependencies include
`ui -> app -> model`, `ui -> irc-core`, `ui -> storage`, `ui -> notify-rust`, `ui -> upload ->
storage/model`, `ui -> media -> model`, `storage -> keyring`, `upload -> ureq`,
`media -> ureq/url/image` and `irc-core -> irc/Tokio`.
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
server profile (D017). Version 14 adds the image preview appearance setting,
off for new and migrated settings (D018). Version 15 adds per-server IRCv3
opt-ins (message tags and server timestamps), off for new and migrated
settings (D022); the later batch and metadata (avatar) opt-ins are new
fields of the same object without a version change, read as off when
absent, and so is the Appearance `user_avatars` setting (D023), as are the
peer avatar option and the per-server shared URL `peer_avatar_url` (D025),
and the Appearance `saved_colors` palette (up to 24 `#RRGGBB` colors; invalid
or repeated entries are dropped on load). The UI keeps the
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
requires TLS before sending SASL PLAIN credentials, and before sending server
PASS unless the server's profile explicitly allows it without TLS (D024). The nickname (`NICK`), the `USER` username and the SASL account are separate
settings; `ConnectionConfig` carries the username explicitly and its `Debug`
output redacts both passwords. Each core connection supports auto-join, channel
messages, NAMES snapshots, `PRIVMSG`/`NOTICE`, and `/` commands; the UI runs one
per connected server.
Channel target validation is shared by the core and the UI's outgoing-message
routing and accepts `#`, `&` and IRCnet `!` channels.
Safe-channel short names are sent unchanged in JOIN; the server's returned full
name (including its five-character identifier) is used for the conversation,
roster, messages and subsequent commands, and is preserved in WHOIS channel links.
CAP negotiation is shared by SASL and the opt-in IRCv3 extensions (see IRCv3
capabilities and message tags below); SASL sends PLAIN credentials once `sasl`
is acknowledged, and CAP END waits for its success. The UI retains each server's active connection
configuration and retries its unexpected disconnections after 3, 6, 12, 24, then 30
seconds (capped), independently of the other servers.
The core allows 15 seconds for TCP/TLS and 90 seconds for registration (001):
IRCnet holds registration about 30 seconds when a client's ident port 113
silently drops packets, which a 30-second limit turned into a reconnect loop.
A successful registration resets the delay; an explicit disconnect cancels pending
retries. Disconnect during DNS lookup or TCP/TLS setup ends the worker at once
(the command queue is read only after the transport opens); once connected it
sends QUIT and flushes it before closing. The core reports a terminal `Refused` event instead of `Disconnected`
for SASL failures (902 account locked or held, 904–907), a missing SASL PLAIN offer, UTF8ONLY with a legacy encoding,
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
Reconnection preserves the in-memory conversation logs and drafts, and
with chathistory recovers what joined channels missed (see Channel
history). A complete membership event reducer remains future work.
The core emits fresh member snapshots after NAMES completion and incoming JOIN,
PART, KICK, QUIT, NICK and channel MODE changes. Application state sorts each
snapshot with operators first and case-insensitive nickname order within each
group. The member list selects members like a file list: click chooses one,
Cmd/Ctrl-click adds or removes one, Shift-click chooses the range from the
member clicked last. The choice is kept by nickname for one channel (it
follows roster updates and starts empty in another channel), and a
right-click on a member outside the choice replaces it with that member. The
member context menu routes Whois, invite and +o/-o through validated
IRC commands; private-message composition sends directly to the selected nick.
`Connection::send_member_modes` gives or takes op/voice for several members,
as `MODE <channel> +ooo a b c` lines of at most the server's announced
`MODES` (ISUPPORT, read by the worker; 3 when absent, never more than 12) and
a bounded length, validating everything and checking that the queue has room
for every line before queuing anything. NAMES entries without a nickname
(servers padding with spaces) are dropped from rosters in `names_snapshot`.
The channel tree context menu sends `/join` or `/part` for the clicked channel;
only the action matching its current joined state is enabled while registered.
The server context menu also offers Join channel… (sends `/join <name>`) and
Change nickname… (`NICK`), each asking in the small prompt the member menu
uses and enabled only while registered; typed `/join` and `/nick` are unchanged.
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
double-click. With image previews on, a thumbnail of the message's first
direct image link appears below its text (see Inline image previews). The
lower combined log switches to a line's channel on double-click.

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
(Disconnect, Reconnect, Show/Copy diagnostics) act on the selected server;
Disconnect is available (in the menu bar and the server context menu) only
while that server has a connection, including one still opening, or a
scheduled reconnect to cancel; the
settings window's Connect and Disconnect act on the server being
edited. A new server, blank or from a preset, starts with an empty
nickname, `USER` username, auto-join channels and SASL account, SASL,
password saving and startup connection off, IRCv3 options off, and the
transport defaults (port 6667, no TLS, certificate verification on, UTF-8);
a preset fills only its host. Choosing another server in the Connection or
IRCv3 tab (`SettingsForm::switch_server`) first keeps the shown server's
edits in the form's values and stores its typed passwords under its own
keys, then fills every server field from the chosen profile alone, so a
pending autosave can only see each server's own values.
Settings save automatically as they change (D021); saving adds,
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
connection status, selection, unread IDs, and active IDs. Only configured channels,
our own JOINs and private messages (see Conversations) create conversations (at
most 1,000 per network, 100 of them private); messages for
any other channel go to the bounded server log, and member snapshots or activity
for unknown channels are ignored, so a hostile server cannot grow memory without
limit. The core likewise caps unfinished WHOIS replies (32 nicknames, 512
channels or extra lines each) and caches rosters only for joined channels. Typed `NetworkId` and
`ConversationId` distinguish identity from display names. It also retains an offline
mock for tests. `ui::ChatWindow` maps GPUI key actions to
navigation and send commands. GPUI entities retain separate server/channel draft
editing, selection, nickname completion and IME state. No `irc` library types enter
application state or rendering components.

## IRCv3 capabilities and message tags

```text
storage::ServerProfile::ircv3 (Ircv3Preferences, per server, default off)
        |   ui::connection_config / apply_servers (next connection only)
        v
irc-core ConnectionConfig::ircv3 (Ircv3Options)
        |
        v
irc-core::cap::CapNegotiation (one per connection attempt)
        |   CAP LS 302 -> REQ per capability -> ACK/NAK -> SASL -> CAP END
        |   NEW/DEL followed after registration
        v
irc-core::tags (read on the parsed irc message; nothing retained)
        |   server_time() only when server-time was acknowledged
        |   replay::ReplayTracker reads BATCH and the `batch` tag
        v
Event::{ChannelMessage, ChannelActivity, PrivateMessage}::server_time
Event::{ChannelMessage, PrivateMessage}::msgid
        |   ui (IRC adapter) -> app::MessageMeta
        v
app::AppState::append_*_at -> Message::{time, timestamp, native_id, provenance}
```

Negotiation starts only when SASL or an opt-in extension is configured. With
everything off and no SASL, registration still opens with the plain
`CAP END` the `irc` library's `identify()` sends, so a server sees exactly what
it saw before. Otherwise the core sends `CAP LS 302`, collects continuation
lines (`*`) and `name=value` offers (at most 256 names and 512-byte values
kept, always including the names this connection may request), and sends one
`CAP REQ` per wanted capability: a REQ is accepted or rejected as a whole, so
a declined optional extension cannot take SASL down with it. Only
implemented, opted-in extensions that the server offered are requested, plus
`sasl` when SASL is configured. A NAK of an optional extension is a
diagnostic, not an error. A multiline ACK is applied at its last line. CAP END
goes out once every request is answered and SASL has finished; SASL failures
and a missing PLAIN offer remain terminal refusals. A server without CAP
registers directly (001), which ends negotiation. After registration CAP NEW
requests newly offered wanted capabilities (never `sasl`) and CAP DEL
withdraws them; unrequested ACKs are ignored and `-name` entries disable.
The state lives in the connection's worker, so a reconnect always starts
from nothing. Registration timeouts, nickname rejection, TLS requirements and
credential redaction are unchanged, and no timer or thread was added.

Tags come from irc-proto 1.1.0's parsed `Message`, which splits and
unescapes them. `irc-core::tags` adds what the library leaves out: the last
occurrence of a key wins, an empty value equals a missing one, a value
containing U+FFFD (bytes the line codec could not decode) is dropped instead
of used, and a tag section over 8,191 bytes (measured on the re-escaped
tags, including `@` and the trailing space) is ignored as a whole while the
body is still processed. The library enforces no line length at all, so
neither limit truncates a line. Unknown tags are ignored. Only the values
this client uses are read; no tag map is retained. TAGMSG produces no event:
no chat row, unread mark, notification or preview request; it remains in
the diagnostic transcript, where long tag sections are shortened to 512
bytes. Server log lines are shown without their tags.

Legacy encodings: the `irc` codec decodes a whole line with the connection's
encoding before tags are parsed, while tag values are UTF-8. Replacing that
codec would mean a transport rewrite, so the policy is narrow: `message-tags`
(which lets other users' arbitrary client tags through, which could contain
bytes such as ISO-2022-JP escapes that change the decoder's state for the
body) is not requested on legacy-encoding connections, and the IRCv3 tab
says so; `server-time` stays available because its values are ASCII.

server-time: the `time` tag (`YYYY-MM-DDThh:mm:ss.sssZ`, UTC; any number of
fraction digits accepted, leap second clamped) is parsed without new
dependencies into a `SystemTime`. `app` converts it to the local time of
day for display (`Message::time`, `TimeOfDay`, minutes as `u16`) and also
retains the full instant (`Message::timestamp`, milliseconds; see Timeline
items). No timestamp strings are stored. Absent or invalid values, and
connections that did not negotiate server-time, show the receipt time and
retain no timestamp.
The arrival sequence remains the ordering key; logs are never reordered by
server time. Local echoes of our own messages keep local time, and an old
timestamp neither suppresses notifications nor marks anything as history.
Diagnostic elapsed times are unchanged. Bouncer backlog delivered as
ordinary tagged lines, without a history batch, counts as live and can
notify, within the notification rate limit. The display is still HH:MM, so a
line from a previous day shows only its time of day; server log lines
(numerics, server notices) keep receipt time.

msgid: `tags::msgid` reads the `msgid` tag with the normalized reader
whenever a server sends it (servers send it only with message IDs
implemented, normally with `message-tags`) and puts it on
`ChannelMessage`/`PrivateMessage`. The application keeps it only if it is
1–128 bytes of visible ASCII (`model::NativeMessageId`); anything else counts
as absent rather than being truncated, which also keeps legacy-encoding
decoding from producing a different identifier for the same message. No
other tag is retained.

batch (opt-in per server): when enabled and offered, `batch` is requested
with its own `CAP REQ`, on legacy encodings too because references and the
history types are ASCII; it does not turn on `server-time` or
`message-tags`. `draft/event-playback` and `draft/multiline` are never
requested. The batch option alone sends no CHATHISTORY command (the
separate history option below does): it only receives batches that servers
and bouncers send by themselves (soju's join backlog, ZNC playback). `replay::ReplayTracker`, the tracker
that already recognized history, follows them:

- It keeps only the references of open history batches: `chathistory`,
  `znc.in/playback`, and any batch opened inside one (ancestry is settled
  when a batch opens, from the `batch` tag on its `BATCH +` line). Other
  batches (netsplit, multiline, unknown vendor types) are not stored, so
  their messages and messages naming an unknown or ended reference stay
  live. Concurrent batches are independent, and untagged live lines between
  history lines stay live. Messages are classified one at a time as they
  arrive; nothing is buffered until a batch ends.
- References are compared exactly (they are case-sensitive). irc-proto
  1.1.0 upper-cases every batch type (`BatchSubCommand::CUSTOM`, and
  `NETSPLIT`/`NETJOIN` are matched case-insensitively), so the raw type is
  lost and history types are compared in upper case. Tag lookups use the
  normalized reader (last duplicate wins, empty is missing).
- Bounds: at most 64 open history batches (a missing `BATCH -` evicts the
  oldest when the table is full) and references of at most 64 bytes (longer
  ones are not followed). A reused open reference replaces its entry, so a
  stale history batch cannot mute a later live one; an unknown `BATCH -` is
  ignored.
- The tracker lives in the connection's worker, so disconnects and
  reconnects start empty, and CAP DEL (or `ACK -batch`) clears it.
- With batch negotiated, `BATCH` framing lines produce no event (like
  TAGMSG) and stay in the diagnostic transcript.

With the option off, nothing about batch is negotiated and behavior is what
it was before the option existed: a server that sends batches unsolicited
still has its `chathistory`/`znc.in/playback` batches recognized by the
same tracker, with the same bounds, and its `BATCH` lines appear in the
server log as before. That compatibility handling is not a claim of batch
support.

metadata (experimental, D023): `draft/metadata-2` is wanted on every
connection (`Ircv3Options::metadata` is always set by the UI; there is no
option). The draft requires batch, so `batch` is requested too, even when
the server's batch option is off, but only from a server that offers
`draft/metadata-2`. Metadata is requested with its own `CAP REQ` only after `batch` is
acknowledged (a server declining batch therefore never gets a metadata
request) and never together with the legacy `metadata-notify`. If `batch`
goes away (DEL or `ACK -batch`) metadata is dropped at once and
`CAP REQ -draft/metadata-2` is sent. `irc-core::metadata` implements only
the subset avatars need:

- Once registered (001) or once the capability arrives later, it sends
  `METADATA * SUB avatar` and `METADATA * GET avatar` (our own current
  value) a single time and reports `Event::MetadataReady`; the configured
  JOINs follow at end-of-MOTD, so channel bursts include avatars.
- `METADATA <target> avatar <visibility> [<value>]`, `761 RPL_KEYVALUE` and
  `766 RPL_KEYNOTSET` for users produce `Event::UserAvatar`. Other keys,
  channel targets and `*` are ignored; no metadata map is stored. A
  numeric whose first parameter is `*` is a notification (Ergo announces
  changes that way); one addressed to us answers our request. Values for
  our own nickname (or `*` in an answer) also produce `Event::OwnAvatar`. A value
  that is empty, missing, longer than 2,048 bytes, or contains controls,
  whitespace or U+FFFD counts as no avatar. Metadata messages inside
  `metadata` batches are handled like any other; the batch type is not a
  history type, so `ReplayTracker` stores nothing for it.
- `774 RPL_METADATASYNCLATER` for a joined channel schedules one
  `METADATA <channel> SYNC` after the given delay (default 5 s, clamped to
  1–300 s), at most 16 channels pending and 3 requests per channel and
  connection. The select loop has a sleep branch only while one is pending;
  PART/KICK drops the channel's request.
- 770–772 and `FAIL METADATA` become diagnostics. Metadata lines of this
  subset produce no server-log line, chat row, unread mark, notification or
  preview; they stay in the transcript. With the capability off, a
  `METADATA` line from a server is shown in the server log as before.
- Our own avatar: `Connection::set_own_avatar(request, Some(url) | None)`
  queues `METADATA * SET avatar :<url>` or `METADATA * SET avatar`
  (removal of that key only). The worker refuses it unless registered with
  the capability (`OwnAvatarFailed { Unavailable }`) and keeps one request
  outstanding (`Busy`). The answer is `Event::OwnAvatar { url, request:
  Some(id) }` (the server's value, which may differ) or `OwnAvatarFailed`
  with the `FAIL METADATA` code and description (`KEY_NOT_SET` on a removal
  counts as removed), `RateLimited { retry_after }`, `NoReply` after 20 s
  or `CapabilityLost`. Replies are matched by position (one request at a
  time): a `761`/`766` for us addressed to us, or a `FAIL` naming us or the
  key. The value is checked again here (no controls or spaces, at most 400
  bytes, ASCII on legacy encodings).
- Showing our own avatar: rows by our current nickname (main log and member
  list) use the avatar the server confirmed on this connection, else the URL
  shared with peers, read at draw time; no lookup or CTCP request to
  ourselves is made, and a change shows on every row at once. The unpublished
  draft is never shown.
- Later joiners: a live JOIN (not in a history batch) of someone who
  shares no other channel with us (judged from the rosters published
  before it) and has no known avatar is looked up with `METADATA <nick>
  GET avatar` after 2 s unless the server announces it first. At most 64
  lookups are pending, 8 unanswered and two sent per second; an unanswered
  one is abandoned after 30 s and its late answer dropped; `RATE_LIMITED`
  retries once after the given delay and pauses all lookups; `INVALID_TARGET`
  or a permission failure ends it. PART, KICK or QUIT from the last shared
  channel cancels it (an answer in flight is dropped unless the user joined
  again first, since the server's answer then describes the new
  occupant); NICK moves it. The timer branch of the select loop exists
  only while a sync, a request or a lookup is pending.
- Avatars are remembered per connection for at most 2,048 users (later ones
  get none). NICK moves an avatar (`Event::AvatarMoved`); QUIT, and PART or
  KICK from the last channel shared with us (judged from the rosters
  published before the message), end it (`UserAvatar { url: None }`).
  Losing the capability sends `Event::AvatarsReset`. Everything lives in the
  connection's worker, so a reconnect starts empty.

Real name and `setname` (D032): `ConnectionConfig::realname` is the clean
user value; `wire_realname()` adds the avatar mark for `USER`, and the worker
adds the connection's mark to `SETNAME`. `Outgoing::SetName` is answered by
`Event::RealNameChanged` / `Event::RealNameFailed`; the worker keeps only a
counter (at most four) of unanswered requests. The UI applies a changed
setting from `apply_servers`, next to the CTCP AVATAR share URL.

CTCP AVATAR (experimental, D025): `Ircv3Options::peer_avatars` needs no
capability. `irc-core::peer_avatar::PeerAvatars` exists in the worker
only while it is on:

- Registration: `ConnectionConfig::shared_avatar` (the explicitly shared
  URL) together with the option makes the `USER` realname
  `\x034\x0fCayenChat` (KVIrc's avatar mark,
  `ConnectionConfig::advertises_avatar`); otherwise it stays `CayenChat`.
  `Connection::share_avatar(Some | None)` changes only the URL answered,
  not the realname.
- A private `\x01AVATAR\x01` PRIVMSG from a user is answered with
  `NOTICE <nick> :\x01AVATAR <url>\x01` while a URL is shared (one per user
  per minute, five per ten seconds). Every CTCP AVATAR PRIVMSG or NOTICE is
  consumed before `translate_message`: no chat row, server line, unread
  mark, highlight, notification or preview.
- A CTCP AVATAR NOTICE (answer or announcement), live, from a user who
  shares a channel with us (to us) or is in the channel it is sent to,
  sets that user's peer avatar: an `http(s)` URL passing the metadata
  value rules, or none (empty, file name, other scheme). The second field is
  ignored; nothing about DCC exists.
- Discovery: realnames in 311 (read, still collected for WHOIS) and 352
  replies; a live `ChannelMessage` or `PrivateMessage` sender who shares a
  channel and has nothing known queues one `WHO <nick>`; a marked realname
  queues one `PRIVMSG <nick> :\x01AVATAR\x01` unless metadata gave an
  avatar. Queue ≤ 32, one probe per 2 s, ≤ 4 outstanding, 30 s timeout,
  `263` pauses 30 s; our 352/315/401 are consumed. Per-user state is
  cleared on QUIT or leaving the last shared channel and moved on NICK
  (a WHO in flight for the old name is absorbed unused). A select branch
  exists only while a probe is queued or outstanding.
- Merge: the struct keeps each user's metadata and peer reference and
  emits `UserAvatar`/`AvatarMoved` for the shown one (metadata first);
  metadata's own events pass through `merge_metadata`, its moves are
  absorbed (the peer lifecycle, run first for the same message, already
  moved or ended the user), and `AvatarsReset` is followed by the peer
  avatars again. With the option off nothing is merged and metadata
  events are unchanged.

Other CTCP (D029): `irc-core::ctcp::CtcpReplies` runs in the worker after
`PeerAvatars` and before `translate_message`, and consumes every CTCP
PRIVMSG or NOTICE except ACTION (and AVATAR while peer avatars are on). A
live private request from a user is answered by `NOTICE <sender>` for PING,
VERSION (`CayenChat <CARGO_PKG_VERSION>`), TIME (local, via chrono) and
CLIENTINFO; nothing else and nothing sent to a channel is answered. Each
admitted request or reply becomes one `Event::ServerLine`
(`CTCP VERSION request from bob`); replayed, own and misaddressed ones are
dropped silently. Two budgets (requests; replies) each admit five per ten
seconds and two per user, remember only admitted entries, and report a
refusal at most once per ten seconds.

On legacy encodings the whole line is decoded with the connection's
charset, while metadata values are UTF-8. Avatar URLs are the only values
used, so the fallback is narrow: only ASCII values are accepted there (any
non-ASCII byte decodes differently), and non-ASCII URLs are dropped rather
than guessed; publishing likewise accepts only ASCII URLs there. The IRCv3
tab says so.

Publishing from the UI: the IRCv3 tab shows, for the selected server, the
draft URL field (`ServerProfile::avatar_url`,
saved by autosave like any field and never sent by it), "Choose Image…"
when an image host is set up, "Send to IRC Server" (with the exposure warning as its tooltip) only when
connected and the draft differs from the server-confirmed URL, "Remove
from IRC Server" (right-aligned, warning color, confirmation dialog) only
when the server holds one, and only an in-progress or failed
outcome; no explanatory text, except one line that the IRC server does not
support avatars when the current connection asked for them
(`ServerSession::metadata_requested`) and registered without
`MetadataReady`. `ui::ircv3_settings` checks the
draft (`media::policy::publishable_avatar_url`, the length and encoding
rules) and asks `ChatWindow::request_own_avatar`, which starts a request in
the server's `ServerSession::own_avatar` (`app::own_avatar::OwnAvatar`) and
queues it. That state is protocol-free: `Confirmed` (unknown, not set, a
URL), at most one pending request with a session-wide identifier, and the
last `Outcome`. With an image host configured, `ui::ircv3_settings` also
accepts an image dropped on the section, pasted into the URL field (the
field propagates image-only pastes like chat drafts) or chosen in the
system file dialog, opens it in `ui::avatar_editor` (square selection over
a 320-logical-pixel view: corner handles resize the square (the opposite
corner stays), dragging inside moves it, the area outside is shaded, and
mouse moves are followed window-wide during a drag; after a drop leaving
the square under half of the image's shorter side the view becomes that
square centered in twice its size, clamped to the image (shown at once
from the whole image and sharpened when its own preview is made off the
UI thread), otherwise the whole image; "Whole Image" resets, and a 64 px
result preview follows; `media::avatar_edit` decodes, crops and encodes at most
256×256 off the UI thread), and runs the encoded square through the chat
upload steps shared in
`ui::image_upload` (`configured_uploader`, `acceptable_attachment`,
`upload_in_background`) with its own `AttachmentFlow<String>` targeting
the server profile ID; the confirmed URL goes into that profile's draft
(`place_uploaded_avatar`) and is sent at once through
`ChatWindow::request_own_avatar` when that server can receive it (images are
accepted regardless). The own-avatar
state only changes on `MetadataReady`, `OwnAvatar`,
`OwnAvatarFailed`, `AvatarsReset` and the end of a connection (disconnect,
reconnect, removal), which fails a pending request and forgets what the
server held. The settings window reads it through its owner handle and is
redrawn only for batches containing those events.

Peer sharing (D025) is shown under the same field while the server's peer
avatar option is on: the shared URL (`ServerProfile::peer_avatar_url`),
"Share with Peers" (tooltip: exposure warning) when the draft differs from
it, "Stop Sharing" when one is shared, and the reconnect line from
`ircv3_settings::peer_reconnect_needed`. Share copies the checked draft
(`share_draft_with_peers`; `peer_avatar_url_problem` adds the `{size}`
refusal); `toggle_feature` clears the shared URL when the option goes off.
`ChatWindow::apply_servers` (autosave) writes `shared_peer_avatar(profile)`
into `active_config` and, when the current connection was made with the
option (`ServerSession::peer_avatars`, recorded by
`connection_starting`), sends a changed URL with
`Connection::share_avatar`. The draft never reaches it.

Preferences live in `Ircv3Preferences`, one field per feature with its own
serde default; the settings tab renders one `ircv3_settings::Ircv3Feature`
row per field. A feature can later become enabled by default (a settings
version migration) or move to another tab by moving its row, without
touching `irc-core`, which only receives `Ircv3Options` booleans.

## Timeline items

```text
IRC protocol state (irc-core: tags, replay::ReplayTracker, events)
        |   ui::ChatWindow::handle_event (the IRC adapter today)
        v
app::MessageMeta { server_time, native_id, provenance }
        |   app::AppState (conversations, timeline::DuplicateFilter)
        v
model::Message (retained timeline item)
        |
        v
ui logs (LogList, combined subwindow, previews, avatars)
```

CayenChat's shared presentation layer is protocol-agnostic where practical,
but protocol state and protocol-native identifiers remain owned by their
respective backends. `model::Message` is what the logs render and knows no
protocol:

- `sequence` (`u64`) is the application's identity for a message and its
  order key: unique for the run, never reused, assigned when the message is
  added. Logs keep arrival order and are never re-sorted by timestamp;
  `LogList`, the combined subwindow, previews and avatar occupancy all key on
  it. Ordinary messages count up from 2^62; older history pages, which go
  before everything a conversation holds, count down from it (see Channel
  history), so every log stays ascending without renumbering.
- `time` (`TimeOfDay`) is presentation only: the source's time when there is
  one, otherwise the receipt time.
- `timestamp` (`Option<model::Timestamp>`, milliseconds since the Unix epoch)
  is the source's own time and nothing else; receipt times are not stored,
  so it can serve as a history reference.
- `native_id` (`Option<model::NativeMessageId>`) is the source's identifier,
  opaque and meaningful only within the backend and conversation it came
  from (IRC: `msgid`). It is never the application identity: many messages
  have none (no IRCv3, local echoes, activity lines).
- `sender` is a display name. Nothing in the presentation layer treats it
  as a unique identity; per-user state (avatars, private conversations) is
  keyed by the adapter's folded key within one network.
- `provenance` (`model::Provenance`): `Live`, `Replayed` (history the server
  or bouncer sent by itself: bouncer log replay, history batches nobody
  asked for) or `Requested` (history this client asked for; see Recent
  channel history). Only live messages notify or highlight
  (`Message::is_history`).

`app::timeline::DuplicateFilter` (one per conversation, created only when a
conversation receives a message with a native identifier or timestamp)
remembers the latest 512 keys and drops a repeated delivery before it takes
a sequence, marks the conversation unread or notifies (the UI notifies only
when `append_channel_message_at` returns `true`). Keys are the native
identifier (any provenance) or, without one, a fingerprint of timestamp,
sender, activity flag, text length and the first 512 text bytes, used only
to drop history and only with a source timestamp, because fingerprints can
collide. Keys are 64-bit hashes with a per-filter random seed. The filter is
dropped with its conversation (reset, reconnect, removed server), so
nothing persists or grows with the session. Server-log lines (numerics,
server notices, private notices outside a conversation) retain the same
fields but are not filtered.

## Conversations

```text
irc-core events (ChannelMessage, PrivateMessage, OwnPrivateMessage,
                 OutgoingAccepted, UserNickChanged, UserQuit)
        |   ui::ChatWindow::handle_event (IRC adapter: targets, case mapping,
        |   NOTICE policy, activity wording)
        v
app::AppState conversations (model::Conversation { kind, name, messages })
        |   ConversationKind::Channel | Private { peer_key }
        v
Selection::Channel(ConversationId) -> the same main log, draft, previews,
avatars, scroll state, unread/highlight and combined subwindow
```

A conversation is a channel or a private conversation; the server log is
not a conversation (`Selection::Server`). The UI renders conversations by
`ConversationId` and never branches on IRC target syntax to draw them; the
kind only changes what the tree's context menu offers (Join/Part or
Close), who the draft sends to (`send_message` for a channel,
`send_private_message` for a peer) and that private conversations have no
member list. Identity: `ConversationKind::Private { peer_key }`, where the
key is the adapter's folded peer name (IRC: RFC 1459 case-mapped nickname,
the same key avatars use) and is only compared within one network, so the
same nickname on two servers is two conversations. `channel_id` never
matches a private conversation, and `private_id` never a channel.

IRC routing (the adapter):

- A live or replayed PRIVMSG (including CTCP ACTION) from a user to our
  nickname opens or reuses the sender's conversation, is added there and
  not to the server log, marks it unread and, if live, highlighted, and
  notifies unless that conversation is selected in the focused window.
  Replayed ones (bouncer playback) go to the same conversation and never
  notify; their sender and target are unambiguous there.
- A private NOTICE joins an existing conversation with its sender;
  otherwise it stays in the server log as before, because notices are
  usually services and bots.
- Our own messages to a nickname (the draft of a private conversation,
  `/msg nick`, the member and WHOIS "private message" prompt) appear in
  its conversation when the connection accepts them (`OutgoingAccepted`),
  without an unread mark; credentials sent to NickServ/ChanServ are shown
  as `[redacted]`, like the transcript. A PRIVMSG/NOTICE from our own
  nickname to someone else (a bouncer relaying another client of ours, or
  its playback) is `OwnPrivateMessage` and goes to that conversation too;
  after our nickname changed, such old lines stay in the server log.
- `/me` and `/msg :text` in a private conversation target the peer.
- NICK of another user renames a conversation with them and adds an
  activity line, unless a conversation with the new name exists (then
  nothing is merged; the line still goes to the old one). A later user of
  the old nickname starts a new conversation. QUIT adds an activity line
  to the quitter's conversation, so a reused nickname's messages are
  visibly after a boundary. Only users sharing a channel are seen to
  change nick or quit. Accounts are not used.
- Private conversations are usable (active) while their network is
  registered; they are closed from the channel tree's context menu, and a
  fresh session of their network (connect, which resets conversations to
  the configured channels) or removing the server drops them, like channel
  logs. Events of an old connection generation are dropped by the event
  pump, so nothing is routed into the new session.
- At most 100 private conversations per network; further PRIVMSGs from new
  peers stay in the server log.

A future backend adds conversations of these kinds with its own keys (for
example a room or DM identifier) through the same `AppState` calls and
appends timeline items with `MessageMeta`; the IRC-specific parts above
stay in the IRC adapter.

## Channel history

Four behaviors look alike and are kept apart:

- **Server-pushed playback**: history a server or bouncer sends by itself
  (soju/ZNC join backlog, Tiarra's Log::Recent). Recognized by
  `replay::ReplayTracker`, shown as `Provenance::Replayed`, never asked
  for. Receiving `chathistory` batches (`batch` framing) is this, not
  history request support.
- **Recent history on join**: `CHATHISTORY LATEST` when we join a channel
  (below).
- **Older pages**: `CHATHISTORY BEFORE` when the user scrolls to the top of
  a channel's log (below).
- **Reconnect gap recovery**: on the first join after a reconnect,
  `CHATHISTORY LATEST` with the newest message received before the link
  dropped asks only for what was missed (below).

The last three share the per-server option, `irc-core::history`'s queue
and the duplicate filter, and produce `Provenance::Requested` lines, which
never notify, highlight or mark unread and stay out of the combined
subwindow.

### Recent history on join

```text
storage Ircv3Preferences::chathistory (per server, default off)
        v
irc-core cap: batch -> ACK -> draft/chathistory (+ server-time, message-tags on UTF-8)
        v
irc-core history::HistoryRequests (worker; bounded queue, one request at a time)
        |   our JOIN -> CHATHISTORY LATEST <channel> * min(50, ISUPPORT CHATHISTORY)
        |   Event::HistoryRequested { channel }
        |   reply batch consumed whole -> Event::ChannelHistory { channel, messages }
        v
app::AppState::history_requested (reserve 256 sequences)
app::AppState::insert_channel_history (splice at the reservation, dedupe)
        v
main log (same rows); not in the combined subwindow; no notification
```

Negotiation: with the per-server option on, `draft/chathistory` is
requested only from a server that offers it and only after `batch` is
acknowledged, because this client recognizes replies by their batch (the
specification also allows replies without batches; CayenChat does not use
them). The same option requests `server-time` and, on UTF-8 connections,
`message-tags`, which the specification lists for full support (timestamps
for display and deduplication, message IDs for deduplication); on legacy
encodings message-tags stays off as before and deduplication falls back to
timestamps. `draft/event-playback`, echo-message and labeled-response are
not requested; the specification does not require them. Losing `batch`
drops chathistory with `CAP REQ -draft/chathistory`, and a request being
answered then ends without lines. Negotiating chathistory asks servers and
bouncers not to play history back by themselves, so the option always
comes with requests: every channel we join (auto-join, `/join`, and
channels already joined when the capability arrives later with CAP NEW)
asks for its latest lines.

Requests: `CHATHISTORY LATEST <channel> * <n>`, `n` = 50 lowered by the
server's `CHATHISTORY` ISUPPORT value (0 or absent: 50). One request is
outstanding per connection; the next goes out when its reply ends, fails
(`FAIL CHATHISTORY …`, 421/461 naming CHATHISTORY) or times out after
30 s, so joining many channels queues rather than bursts (at most 64
waiting; more are skipped with a diagnostic). PART/KICK drops a queued
request. The only timer is the outstanding request's timeout.

Replies: a `chathistory` batch whose parameter matches the outstanding
channel (RFC 1459 case mapping) is ours. Every line in it and in batches
nested inside it (at most 16) is consumed there: it never reaches replay
classification, rosters, avatars, notifications or the server log. Only
PRIVMSG and NOTICE addressed to the channel are kept (at most 100; the
rest is counted in a diagnostic); other commands, which servers must not
send without event-playback, are dropped rather than applied. Lines are
reported together when the batch ends, so a batch cut off by a disconnect
reports nothing. A reply after its timeout is swallowed (the last 8
abandoned channels are remembered) rather than shown as live or as
another request's reply. Other `chathistory` batches (bouncer playback)
keep the existing replay handling. Ergo 2.19 reports events such as our
own JOIN as `HistServ` PRIVMSGs inside the reply when event-playback is not
negotiated; they are shown as ordinary history lines.

Merge: `Event::HistoryRequested` makes `AppState` reserve 256 sequences
after the conversation's current lines. `Event::ChannelHistory` inserts
its lines there as `Provenance::Requested`, in the server's order, before
every line that arrived after the request, so unrelated live lines are
never reordered and logs stay in ascending sequence order (`LogList`
replaces only the inserted rows). Lines the duplicate filter recognizes
(a live line the reply repeats, bouncer playback) are skipped. Nothing is
marked unread or highlighted, nothing notifies, and requested history is
left out of the combined subwindow. A reply without a pending reservation
is ignored: the reservation is dropped when the network disconnects, is
reset or removed, and the UI already drops events of an old connection
generation. Limitations: a live line that arrived after the request and is
repeated in the reply without msgid or matching server-time appears twice;
and the reply is placed after lines received before the request (for
example our own JOIN line).

### Older pages

```text
ui main log scroll handler (LogList::on_scroll, deferred)
        |   first visible row within 5 message rows of the top
        v
ChatWindow::load_older_history
        |   app::AppState::request_older_history(conversation)
        |     -> OlderHistoryRequest { request, native_id, timestamp, limit }
        v
irc-core Connection::request_older_history(channel, request, MessageReference, limit)
        |   worker: history::HistoryRequests::enqueue_older (queue front)
        |   CHATHISTORY BEFORE <channel> msgid=<id> | timestamp=<time> <n>
        |   reply batch -> Event::OlderChannelHistory { request, messages, status }
        v
app::AppState::insert_older_history / older_history_failed
        |   prepend with descending sequences, skip overlap, bound
        v
LogList::sync keeps the top row (sequence + pixel offset) in place
```

Trigger: the main log's GPUI scroll handler (a user's wheel or trackpad
scroll, never a redraw, a timer or merely opening the channel) reports the
first visible row; within 5 message rows of the top the chat window asks
the application for one page. The handler runs while the list is
borrowed, so it defers to the chat window. `AppState::request_older_history`
decides whether a page may be asked for: the network can page
(`Event::HistoryAvailable(true)`: registered with `draft/chathistory`), the
conversation is a joined channel, its recent-history request is not still
open, no page is on its way (`older_history_in_flight`), paging has not
ended, and the log is below its 2,000-line bound. Scroll jitter only
repeats that check. Private conversations are not paged.

State (`app`, per `ConversationId`, created by the first page): the request
on its way (a run-wide counter, never reused) and whether paging ended.
Ended means the reply was empty or tagged `draft/chathistory-end`, a page
added nothing new (so the same request is not repeated), or a request
failed (FAIL, timeout, capability lost, queue full, no usable reference).
All of it is dropped when the network disconnects, the channel is left, or
the conversation is closed or replaced (reset, removed server); the next
session may page again.

Reference: among the oldest 256 lines, the one with the earliest source
time (recent history is placed after our own JOIN line, so the first line
is not necessarily the oldest), or else the first with a msgid. The
application hands over its `NativeMessageId` and `Timestamp`
(`OlderHistoryRequest`); `irc-core` picks the wire form: `msgid=` when the
server accepts msgid references and the identifier is 1–128 bytes of
visible ASCII, else `timestamp=YYYY-MM-DDThh:mm:ss.sssZ` when it accepts
timestamps. `MSGREFTYPES` (read from ISUPPORT, absent means both) limits
the choice; CayenChat prefers msgid whatever the server's order, because a
timestamp reference skips other messages of the same millisecond. With no
usable reference the request fails at once. On legacy encodings
message-tags is not requested, so only timestamp references normally
exist; both forms are ASCII, so no decoded text is ever sent back.

Limits: `min(50, ISUPPORT CHATHISTORY, room left under 2,000 lines)`;
servers returning more are cut at 100 kept lines as for LATEST. BEFORE
requests share the one-outstanding-request queue with LATEST and go ahead
of queued LATEST requests; one per channel. Every accepted request ends in
exactly one `OlderChannelHistory` (`More`, `Beginning`, `Failed`); PART,
KICK or losing the capability ends queued ones as failures.

Insertion (`insert_older_history`): the answer is matched by its request
number, so a page for a left channel, a closed or replaced conversation, or
an ended session is ignored (the UI also drops events of an old connection
generation). Lines already near the top of the log (a repeated page,
recent history, playback) are skipped with a temporary duplicate filter
seeded from the oldest 256 lines, and lines received recently with the
conversation's own filter (checked, not recorded, so paging does not push
out the keys live traffic needs). The rest is prepended in the server's
order with sequences counted down below everything the log holds, with
one `Vec::splice`; nothing already shown moves, is renumbered or
re-sorted, and live lines arriving meanwhile stay at the bottom. When the
log would exceed 2,000 lines the oldest lines of the page are dropped;
paging resumes once live traffic trims the log.

Viewport: `LogList::sync` records the message row at the top of a scrolled
list and its pixel offset within the row, and after splicing restores that
row by sequence (GPUI's `ListState::splice` already shifts its scroll top by
the rows inserted above, so this is a check rather than a correction).
Rows above need no measured height, so images, avatars and wrapped text
of any height above or below do not move the view; a list following the
bottom keeps following it. With a status or diagnostics row at the very
top (disconnected, or the debug transcript on) the row inserted below it
still appears below it.

### Reconnect gap recovery

```text
app::AppState::set_status(Disconnected), session had history
        |   joined channel -> ResumePoint { newest native_id/timestamp,
        |                                   256 sequences reserved at the cut }
        v
ui::ChatWindow::reconnect_config -> ConnectionConfig::resume_history
        |   (the saved active_config is not changed)
        v
irc-core history::HistoryRequests::with_resume
        |   our JOIN -> CHATHISTORY LATEST <channel> msgid=<id> | timestamp=<t-5 s> <n>
        |   Event::HistoryRequested { resumed: true }
        |   reply -> Event::ChannelHistory { messages, incomplete }
        v
app::AppState::history_resumed (pending = the cut's slot)
app::AppState::insert_resumed_history (dedupe; optional gap note first)
```

When a session that had history available (`Event::HistoryAvailable`)
ends, every channel joined at that moment gets a resume point: the newest
line among its last 256 by source time (with its msgid), or else the last
line with a msgid, and 256 sequences reserved right after the lines
received so far. The reference is the source's identifier or time, never
the displayed `HH:MM` or an arrival sequence. A channel without such a
line gets none and simply asks for its latest lines on rejoin, as before.
A point that was not answered is kept across further disconnects (the
earliest cut is what needs recovering, also when an attempt rejoined and
asked but dropped before the answer); a session that joined the channel
without history drops it, and so do PART/KICK, closing or replacing the
conversation (connecting from settings, a removed or edited server).
Automatic and menu reconnects reuse the same server profile
(`active_config`), so a point never moves to another server; at most one
exists per conversation.

The next connection receives the points in
`ConnectionConfig::resume_history` (built per attempt by
`ChatWindow::reconnect_config`). The worker keeps them (at most 1,024)
and uses each for the first JOIN of its channel only, after registration
and the JOIN itself, through the same queue as recent history (one
request outstanding, 64 queued; many channels are paced, not burst).
`LATEST <channel> <reference> <n>` returns the most recent `n` lines
after the reference: recovered lines join up with the live lines that
follow, and a long gap loses its oldest part rather than its newest. The
reference is `msgid=` when the server accepts msgid references, else
`timestamp=` five seconds before the line's time (clock skew between a
network's servers; the lines that repeats are already shown and dropped
as duplicates). With no usable reference type, capability or option the
join asks for `LATEST *` as before and the application gives the point
up. A failed resumed request (FAIL, timeout) falls back to one plain
`LATEST *`.

Merge: `Event::HistoryRequested { resumed: true }` makes the application
use the point's reserved slot instead of reserving at the request, so the
missed lines go right after the last line received before the cut and
before our JOIN line and everything since. The existing insertion does
the rest: duplicates of lines already shown (live lines that arrived
after the rejoin, bouncer playback, older pages, the skew overlap) are
skipped, nothing notifies, highlights or marks unread, and the answer is
matched to the current connection's reservation, so a reply to an earlier
attempt is dropped (the UI also ignores old connection generations). A
reply with as many lines as asked for and no `draft/chathistory-end`
(`incomplete`) may have missed older lines of the gap; one activity line
(`Some messages sent while disconnected are not shown.`) goes first in
the slot. Nothing is chased with further requests, and older pages cannot
fill a gap in the middle of the log.

Limitations: a second disconnect before a recovery was answered keeps
the first cut, so lines of the second gap are placed there, before lines
received live in between; a channel whose request was skipped because 64
were already queued keeps its point for a later reconnect; channels
joined with `/join` are not rejoined by reconnects (only configured ones
are), so they are not recovered until joined again; private
conversations are not recovered.

Direct-message discovery (D034): once per connection, when chathistory
becomes available, the worker queues `CHATHISTORY TARGETS <from> <to> 16`
ahead of the join requests (`from`: the previous disconnect, at most a week
back, default a day; `to`: now plus 5 minutes for clock skew). The reply
batch (`draft/chathistory-targets`) is consumed whole; nicknames (not
channels) are deduplicated by IRC casemapping, newest first, at most 16, and
each is queued as an ordinary `LATEST <nick> * 50` in the same bounded queue.
The application shows such a peer's conversation when its request is sent
and inserts the reply like channel history (provenance `Requested`: no
unread mark, highlight or notification). The reply of a request belongs to
the connection's worker, so a reconnect cannot receive an older
connection's targets.

Not implemented: persistence; recovery of a known private conversation
beyond one page (it repeats `LATEST *` and relies on the duplicate filter;
lines missed while lines arrived live are placed by arrival order);
channels the bouncer knows but we have not joined.

## Notifications

```text
irc-core Event::ChannelMessage { mentioned } / Event::PrivateMessage
        |   (irc-core::text: nickname word match, formatting and ACTION)
        v
ui::ChatWindow::notify_message -> app::notifications::IncomingMessage
        |   (plain text, channel/private, notice, from_self, mentioned, replayed)
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
`PrivateMessage`; server notices stay server lines, and CTCP other than
ACTION becomes a readable server line (D029). Private messages notify only as PRIVMSG, because private NOTICEs
are usually services or bots. Mentions and keywords are separate choices.
`irc-core` sets `mentioned` when someone else names our nickname as a whole
word (RFC 1459 case mapping, formatting ignored), which also covers `nick:`
and `@nick`; keywords are case-insensitive substrings, so the nickname is
matched inside other words only if the user adds it as a keyword. The rules
only receive the `mentioned` flag: a Matrix adapter would set it from the
event's intentional mentions (`m.mentions`) and push rules instead of parsing
text, so no `@`-specific rule is needed in `app`. Our own messages never
notify (bouncer echoes), and neither does replayed history: `irc-core`
(`replay::ReplayTracker`) sets `replayed` on a PRIVMSG or NOTICE that has no
user mask (a line from the server or bouncer itself, such as Tiarra's
Log::Recent replaying channel logs as `:tiarra NOTICE #chan`), that belongs
to an IRCv3 `chathistory` or `znc.in/playback` batch (or a batch nested in
one; see batch under IRCv3 capabilities). A server-time tag alone never
marks history, however old (D022).
Replayed messages still appear in the log (`model::Provenance::Replayed`) and
mark their channel unread, but are neither highlighted there nor in the
channel tree. Nothing notifies while the chat window is
focused and the message's conversation (its private conversation, or the
server view for a private notice kept in the server log) is selected. At most five notifications are shown per ten seconds so bouncer
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
AUTHENTICATE output. The copied connection transcript masks credential commands
as before; it is independent of the Experimental tab's debug file logging.

`Settings::experimental` is an additive, serde-defaulted preference group
(default off, no settings version change). When `debug_logging` is enabled,
`ui::diagnostics` redirects stderr to the user's absolute `stderr_file` path
in append mode and sets the logger to debug, retaining the sensitive-target
filter. This includes Rust's `eprintln!` and default panic hook, not only
`log` records. Windows uses `SetStdHandle` and retains the file handle, even
when a GUI launch has no original stderr handle; Unix replaces descriptor 2
with `dup2`. No console is allocated. A stderr lock serializes switches with
Rust stderr writes. Disabling restores the original destination and the
`RUST_LOG` level (warn by default); changing paths opens the new file before
replacing the old destination. New Unix log files use mode 0600. There is no
background logging thread, timer or in-memory log queue; disk writes are
synchronous, and logs grow until disabled (no rotation).

Startup applies saved logging preferences before credential migration and
GPUI initialization. Failures are shown as a startup notice without stopping
the app. Settings changes apply immediately through the existing save path;
while a new server form is incomplete, experimental preferences are persisted
independently so they do not require finishing the connection form. Opening a
file must succeed before saving the preference, and a settings-save failure
attempts to restore the previous output. The Experimental panel
shows the actual active destination separately from the selected file and
surfaces errors. A file-picker cancellation changes nothing. Tab headers wrap
so the additional tab remains reachable in the fixed-width settings panel.
Windows release binaries use the windows subsystem; debug builds retain the
console for development. Native libraries that cache their own stderr
handles are outside the Rust stderr redirection guarantee.

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
`ExternalUploader`, and `upload` must not grow Matrix types. Displaying images
is the separate `media` layer below.

## Inline image previews

```text
channel message text (main log row being drawn, previews on)
        |   ui::log_urls (the same recognition as link opening)
        v
media::policy::image_link -> media::MediaRef::Link   (direct image link only)
        |
        v
media::cache::PreviewCache  (one per app: dedupe, bounded queue/in-flight,
        |                    byte budget, failures, generation)
        |   next_job
        v
GPUI background executor: media::load_thumbnail
        |   Fetcher (HttpFetcher: checked redirects, public addresses only)
        |   decode: sniff, header dimensions, capped decode, resize, BGRA
        v
ui::previews: RenderImage in the cache; the row draws `img` below its text;
evicted images are removed from the GPU sprite atlas (Window::drop_image)
```

Discovery, loading and display are separate. `MediaRef` is the boundary: IRC
only produces public links, a Matrix client would add its own reference
(`mxc://` plus server thumbnails) and `Fetcher` with its own transport and
authentication, and reuse decoding, the cache and rendering. Nothing here
depends on an upload provider or account, and `ExternalUploader` is not used.

Rows request their preview while they are drawn, so only rows the virtualized
log lays out (the viewport plus its 400 px overdraw) cause requests; receiving
or retaining image links fetches nothing. Only message rows of the main log
(channels and private conversations) preview; the combined log, the server
log (diagnostics) and activity lines stay text-only, and link opening and selection
work on the text as before. A pending preview reserves a box of the full
preview height so a finished load does not move the row. When the finished
height differs (a wide image, or a failure that leaves only the text link),
the rows that showed the placeholder have their measured height replaced in
their `LogList` (`LogList::invalidate`), except the row at the scroll top,
which is on screen and remeasured anyway; the list is not reset. A finished
load notifies the chat window, which redraws the cached panes; typing still
reuses them.

Turning the setting off takes effect immediately: the cache stops answering,
cancels running loads (checked between reads and before decoding), forgets
every record, bumps a generation so late completions are dropped, and hands
decoded images back for release, and the HTTP agent with its idle connections
is dropped. No preview timer or thread exists; loads run on GPUI's shared
background executor. The limits, formats and remote-loading rules are in D018
and `performance.md`.

## User avatars

```text
irc-core metadata (draft/metadata-2) + peer_avatar (CTCP AVATAR, opt-in)
        |  (metadata first)                                a future Matrix client
        |  Event::UserAvatar / AvatarMoved / AvatarsReset      |
        v                                                      v
app::avatars::AvatarDirectory  (per network: user key -> avatar reference,
        |                       occupancy delimited by message sequences)
        |   for_message(network, key, sequence) / current(network, key)
        v
ui::avatars (only while "Show user avatars" is on, only for drawn rows)
        |   media::policy::avatar_url ({size} -> 32) -> MediaRef::Link
        v
media::cache::PreviewCache (separate small avatar instance)
        |   next_job, fetch slots shared with previews (3 in flight)
        v
media::load_thumbnail (HttpFetcher, Limits::avatar(32): centered square)
        v
RenderImage in a fixed 16×16 slot (main-log message rows, member rows)
```

Presentation and protocol are separate. The Appearance setting "Show user
avatars" (off by default) decides whether avatars are displayed and
downloaded; whether IRC avatar references are received depends only on the
server (metadata) and the per-server peer avatar option (CTCP AVATAR). `app::avatars` stores avatar references
(for IRC, the metadata URL template) keyed by network and a
protocol-folded user key (IRC: RFC 1459 case-mapped nickname); it knows no
protocol and fetches nothing, and nothing is copied into retained messages
(`model::Message` is unchanged). A Matrix client would fill the same
directory from room member events and resolve its references through its
own authenticated `Fetcher`, without the IRCv3 option or any uploader.

Identity policy: an avatar belongs to one *occupancy* of a name, from the
next message sequence after it was first seen until the user quits, leaves
the last shared channel, changes name, is removed, disconnects, or the
capability is lost. Messages that arrived during the occupancy show it,
also after it ended (one retired occupancy per name is kept); a later user
of the same nickname starts a new occupancy, so historical lines of an
earlier occupant never show the new one's image, and lines received before
an avatar was known (including replayed history, and a later joiner's
lines before the lookup answer) show none. Registered and
Disconnected events end every occupancy of their network; resetting or
removing a server forgets its directory. The member list shows only
current occupancies. Bounds: 2,048 current and 512 retired entries per
network, the oldest-ended retired entries dropped first.

Display: with the setting on (the only avatar switch, for every server),
main-log channel message rows (not activity
lines, the server log or the combined subwindow) and member rows get a
fixed 16×16 slot (below the 20 px line height, so rows keep their height)
between the time and the nickname, or before the member name. Rows ask for
their avatar while they are drawn, so only visible rows plus the log's
400 px overdraw cause requests; a large roster fetches nothing until its
rows are on screen, and a URL shared by several users is fetched once. A
loading avatar leaves the slot blank; a missing or failed one shows the
nickname's default avatar (`ui::default_avatar`: the `defaultAvatar.js`
port, an SVG of 32 px rasterized by GPUI's `img`, kept per look in
`ui::avatars`, at most 512, cleared when the setting is turned off). With
the setting off there is no slot, no lookup, no fetch, no decode and no
default avatar; turning it off
cancels loads, drops late results (cache generation), releases the
records and drops the HTTP agent. Released images are removed from the GPU
atlas only after both cached panes that draw avatars (main log and member
list) have redrawn, since a cached pane replays its last paint; if one
lags, both are redrawn on the next frame.

`media::policy::avatar_url` separates recognition from safety: an avatar
URL was supplied as an image, so no file extension is required (unlike
`image_link` for chat links, whose rules are unchanged), but the scheme,
credential, port and public-address checks, redirect checks, response type
(`image/*`, not SVG), sniffed content (PNG, JPEG, GIF, WebP; first frame
of animations), size limits and background decoding are the ones previews
use. Limits are in D023 and `performance.md`.

## Settings theme adapter

`ui::settings_theme` caches light/dark native OS theme variants and their
`native-theme-gpui` mappings (D019). Settings renderers use its palette and
button/checkbox helpers. `TextInput::new_settings_field` opts settings fields
into the native font, fill, border/focus, placeholder and selection styling;
chat drafts and other prompts retain their original styling and editing code.
The app's saved theme mode remains authoritative. OS reads happen outside
rendering; non-macOS readers run in the background, with the existing palette
available during loading or after errors.

## Server-confirmed sending (D036)

`irc-core::echo::Echoes` (worker) tracks each sent message until its echo,
ACK, error or expiry; `Event::OutgoingAccepted { local_id }`,
`OutgoingConfirmed`, `OutgoingFailed` connect it to the application, which
keeps `ServerSession::pending_sends` and calls `AppState::confirm_message` /
`fail_message` on the optimistic line. The worker schedules expiry only while
messages are pending, so an unanswered send is reported after 60 seconds even
if no further server line arrives.

## User accounts (D035)

`irc-core::accounts::Accounts` sees each incoming line before translation
(JOIN, ACCOUNT, PART, KICK, QUIT, NICK, its WHOX reply), the published member
lists (`Event::Names`) and our own JOINs, and emits `Event::UserAccount` /
`UserAccountForgotten`. `ServerSession::user_accounts` mirrors them;
`complete_whois` uses it. Active only with the "User accounts" preference.

## account-tag (D033)

`tags::account` reads the `account` tag; `irc-core` events carry it,
`irc_message_meta` in the UI turns it into `model::ServicesAccount`, and
`new_message` retains it in `Message::account`. There is no per-user account
table in this step.
