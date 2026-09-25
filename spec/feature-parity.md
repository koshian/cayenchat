# Feature Parity

This is a working inventory, not a promise of complete LimeChat compatibility.

Statuses: `TODO`, `PARTIAL`, `DONE`, `OUT OF SCOPE`.

## Current scope

The app opens a separate settings window from the four-pane chat window and connects to one configured IRC server.
History, reconnect and notifications remain TODO.

- DONE — native GPUI application shell and app-owned selection
- DONE — mock conversation switching with distinct message logs
- DONE — per-server and per-channel in-memory drafts

## Networks and connections

- DONE — one live server selected from persisted profiles, with user-added servers first and IRCnet presets afterward
- DONE — per-server UTF-8, ISO-2022-JP, Shift_JIS, or EUC-JP line encoding, including channel names; local ISO-2022-JP wire round trip tested
- PARTIAL — rustls connection with certificate verification on by default and a per-server opt-out for self-signed or otherwise invalid certificates; public-server interoperability unverified
- PARTIAL — initial connect, manual disconnect and manual reconnect via settings; no automatic reconnect
- TODO — reconnect
- PARTIAL — one live network; a two-network mock remains in application tests
- PARTIAL — connecting, registered, disconnected status and errors shown in the UI; directional IRC transcript appears automatically during connection/failure, with a menu toggle after registration and clipboard copy (parsed lines, not a complete socket capture)
- DONE — optional server PASS over TLS
- PARTIAL — SASL PLAIN over TLS with CAP negotiation; external-server interoperability unverified

## Channels

- PARTIAL — configured channels auto-join after registration; `/join` and `/part` work, but full membership updates are pending
- PARTIAL — live channel rows, or four mock channels across two networks
- PARTIAL — mock topic kept in application state; display/editing not implemented
- TODO — channel modes relevant to normal use
- DONE — auto-join configured channels

## Private messages

- TODO — open private conversation
- TODO — receive/send private messages
- TODO — conversation persistence during a session

## Messages

- PARTIAL — receive/send channel PRIVMSG; no private-message routing or delivery receipt
- PARTIAL — receive/send channel NOTICE; no private-message routing
- PARTIAL — `/me` sends CTCP ACTION; received ACTION is not specially rendered
- PARTIAL — local receive/send timestamps; no server-time tags
- PARTIAL — own nick changes update send identity; other nick/member changes remain pending
- PARTIAL — own joins/parts update active channels; other join/part/quit events remain pending
- PARTIAL — server responses and errors in the selected server log
- TODO — URLs
- TODO — copy/select text

## Member list

- PARTIAL — selected channel's NAMES roster in the upper-right pane; live changes after the snapshot are not synchronized
- TODO — operator/voice state
- TODO — nick updates
- TODO — join/part/quit synchronization

## Navigation

- DONE — network/channel tree in the lower-right pane for application state
- PARTIAL — reference shortcut set for unread/previous/active/all/indexed channel and
  server navigation; live message/join status drives these sets, no quick switcher
- DONE — cyclic next/previous channel and server commands
- PARTIAL — jump to unread channels in mock or live mode; highlights remain TODO
- DONE — reference four-pane placement: main log over subwindow on the left,
  users over channel tree on the right, draft between left logs
- PARTIAL — subwindow displays other conversations, including live channel messages, and jumps on click; channel/server labels ellipsize on one line

## Unread and highlights

- PARTIAL — unread IDs, visual marks and clearing on selection for mock/live channel messages
- TODO — highlight/mention state
- TODO — configurable highlight words
- TODO — window/application attention indication

## Input and commands

- PARTIAL — editable single-line draft; Enter sends to a joined channel
- PARTIAL — Tab completes listed member nicknames; Ctrl+Enter sends channel NOTICE
- PARTIAL — `/` commands, common channel-target inference, `/raw`/`/quote`; private-conversation UI and command history remain pending
- TODO — command history
- PARTIAL — initial/switch focus, common OS editing shortcuts, undo/redo and
  platform-specific app bindings; user-customized macOS text bindings unsupported
  by GPUI 0.2.2, Windows/Linux runtime unverified
- PARTIAL — CR/LF normalized to spaces; 512-byte encoded IRC-line limit

## Logging and history

- TODO — local logs
- TODO — searchable history
- DONE — bounded in-memory channel and server scrollback
- TODO — IRCv3/server history integration where available

## Notifications

- TODO — desktop notifications
- TODO — per-network/channel controls
- TODO — highlight/private-message notifications

## Settings

- DONE — single-network connection settings with a server drop-down, multiple saved custom profiles, per-profile host/port/TLS/certificate verification/encoding and optional server password
- DONE — persisted nickname and optional SASL account name; per-server server/SASL password persistence only after a plaintext-warning confirmation, with immediate deletion when disabled
- DONE — settings open in a separate window; closing it leaves the chat window running
- DONE — persisted channel auto-join list
- TODO — notification preferences
- TODO — appearance preferences
- TODO — keyboard shortcut preferences where practical

## IRCv3

- PARTIAL — CAP negotiation for SASL PLAIN
- TODO — message tags
- TODO — server-time
- TODO — account-related capabilities
- TODO — extended-join
- TODO — away-notify
- TODO — chathistory evaluation
- TODO — other capabilities based on real-world usefulness

## Appearance

- DONE — compact default layout
- DONE — plain timestamp/nickname/text rows with scrolling and follow-to-bottom unless the user scrolls away
- TODO — light/dark appearance
- PARTIAL — GPUI system font; macOS verified, Windows pending
- TODO — restrained theming

## Platform integration

- PARTIAL — macOS native application, connection, edit and diagnostic menus; Command+, opens the separate settings window
- PARTIAL — Windows/Linux Ctrl+, opens settings and Ctrl+Shift+D/L controls diagnostics, but GPUI 0.2.2 does not render native menus on those platforms
- TODO — notifications on both platforms
- PARTIAL — common macOS/Windows editing shortcuts; Windows runtime and custom
  macOS bindings unverified
