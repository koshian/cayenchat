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
until the bottom is reached again, including during initial IRC history bursts. Channel
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
to `#007D00`; the previous default migrates while custom colors remain. The
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
Windows/Linux use Ctrl+, while GPUI's native menu rendering remains unavailable
there. Passwords persist per server only after explicit plaintext confirmation;
turning saving off immediately removes stored values. TLS certificate verification
defaults to on per server and can be disabled for a specific connection. The core
requires TLS before sending either server PASS or SASL PLAIN credentials. The core supports one connection, auto-join, channel
messages, NAMES snapshots, `PRIVMSG`/`NOTICE`, and `/` commands. Its SASL state
machine negotiates CAP, sends PLAIN credentials, and waits for success before
ending CAP negotiation. The UI retains the active connection configuration and
retries unexpected disconnections after 3, 6, 12, 24, then 30 seconds (capped).
A successful registration resets the delay; an explicit disconnect cancels pending
retries. Reconnection preserves the in-memory conversation logs and drafts. A
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

`app::AppState` owns networks, conversations, bounded message logs, user lists,
connection status, selection, unread IDs, and active IDs. Typed `NetworkId` and
`ConversationId` distinguish identity from display names. It also retains an offline
mock for tests. `ui::ChatWindow` maps GPUI key actions to
navigation and send commands. GPUI entities retain separate server/channel draft
editing, selection, nickname completion and IME state. No `irc` library types enter
application state or rendering components.
