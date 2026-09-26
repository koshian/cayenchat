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
a nickname column regardless of the UI language, and does not mark the channel
unread. Its text uses configurable green (`#007D00`) by default in both the main log
and combined subwindow. The main log keeps separate scroll positions for each
server or channel, and both left logs
follow incoming messages while at the bottom. User scrolling pauses follow mode
until the bottom is reached again, including during initial IRC history bursts.
Both logs and the user list are virtualized (GPUI `list` / `uniform_list`), so
each redraw lays out only rows near the viewport. GPUI re-renders the whole chat
window whenever the draft input changes, so rebuilding every retained line made
typing, IME composition and channel switching slow, most visibly on Linux. The
combined subwindow shows the newest 1,000 lines across other channels. Channel
navigation commands are independent of GPUI. The UI binds macOS shortcuts from the
reference and platform-specific Windows/Linux alternatives; text editing remains
scoped to the focused draft. Ctrl+Tab / Ctrl+Shift+Tab visit unread channels only.

The visual treatment should follow `spec/project.md`: compact, direct, and Chocoa-like rather than resembling a modern consumer messenger.

## Current implementation

The five workspace members use the `cayenchat-` package prefix. Dependencies include
`ui -> app -> model`, `ui -> irc-core`, `ui -> storage`, and `irc-core -> irc/Tokio`.
There are no dependency cycles and GPUI occurs only in `ui`.

`irc-core::Connection` owns a dedicated current-thread Tokio runtime. It translates
the `irc` crate's messages into owned application events and accepts bounded outgoing
commands. TCP/TLS connection and registration do not block GPUI. The current UI polls
at 50 ms intervals while connected, applies events to `app::AppState`, and redraws.
Before a TLS connection, the core installs rustls's ring crypto provider as the
process default. The GUI dependency graph enables both ring and aws-lc-rs, so
rustls cannot infer a provider from crate features alone.
The UI loads versioned preferences from the platform user configuration directory.
Version 5 adds appearance colors, alternating log rows, and per-pane font choices;
version 6 adds an opt-in startup connection flag, and version 7 adds a language
preference. Version 8 renames the appearance background to member-list background
and changes its default to white; the old default gray migrates to white while
custom saved colors remain. Version 9 changes the default channel event text color
to `#007D00`; the previous default migrates while custom colors remain. Version 10
adds a theme preference (System, Light or Dark), a dark-theme set of the six pane
colors beside the existing light ones, and a Linux display server choice; older
settings default to System, the dark defaults and Wayland. The UI keeps the
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
Passwords persist per server only after explicit plaintext confirmation;
turning saving off immediately removes stored values. TLS certificate verification
defaults to on per server and can be disabled for a specific connection. The core
requires TLS before sending either server PASS or SASL PLAIN credentials. The core supports one connection, auto-join, channel
messages, NAMES snapshots, `PRIVMSG`/`NOTICE`, and `/` commands. Its SASL state
machine negotiates CAP, sends PLAIN credentials, and waits for success before
ending CAP negotiation. The UI retains the active connection configuration and
retries unexpected disconnections after 3, 6, 12, 24, then 30 seconds (capped).
The core allows 15 seconds for TCP/TLS and 90 seconds for registration (001):
IRCnet holds registration about 30 seconds when a client's ident port 113
silently drops packets, which a 30-second limit turned into a reconnect loop.
A successful registration resets the delay; an explicit disconnect cancels pending
retries. The core reports a terminal `Refused` event instead of `Disconnected`
for SASL failures, a missing SASL PLAIN offer, UTF8ONLY with a legacy encoding,
and 464/465 during registration; the UI does not retry those automatically,
because repeating rejected credentials risks account lockout or a server ban. Reconnection preserves the in-memory conversation logs and drafts. A
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
double-click. The lower combined log retains click-to-channel navigation.

Version 4 settings keep several server profiles, ordered with user-added entries
before built-in presets. Host/port/TLS/certificate verification/encoding are per
profile; the single live connection and nickname/channel list remain application-wide. Versions 1–3
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
