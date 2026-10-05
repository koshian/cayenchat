# Feature Parity

This is a working inventory, not a promise of complete LimeChat compatibility.

Statuses: `TODO`, `PARTIAL`, `DONE`, `OUT OF SCOPE`.

## Current scope

The app opens a separate settings window from the four-pane chat window by default and connects to one configured IRC server. An opt-in setting connects to the selected server at startup.
Server history is limited to channel history on opt-in IRCv3 servers (recent lines on join, older pages on scroll); desktop notifications cover mentions, keywords and private messages.

- DONE — native GPUI application shell and app-owned selection
- DONE — mock conversation switching with distinct message logs
- DONE — per-server and per-channel in-memory drafts

## Networks and connections

- DONE — every persisted server profile appears in the channel tree in the order added; the list starts empty and IRCnet is offered as a suggestion when adding a server; any number connect at once, each with its own nickname, username, channels, SASL account and startup connection
- DONE — per-server UTF-8, ISO-2022-JP, Shift_JIS, or EUC-JP line encoding, including channel names; local ISO-2022-JP wire round trip tested
- PARTIAL — rustls connection with certificate verification on by default and a per-server opt-out for self-signed or otherwise invalid certificates; public-server interoperability unverified
- DONE — initial connect, manual disconnect and manual reconnect via settings, native menu or server context menu; the server context menu disables reconnect while a connection is active, and it and the menu bar disable disconnect unless a connection or a scheduled retry exists; disconnect also cancels a connection still in TCP/TLS setup; unexpected disconnections retry with capped backoff, while explicit disconnect cancels retries; a nickname rejected during registration (432/433) prompts for another nickname instead of retrying
- DONE — multiple live networks; each connects, disconnects and retries independently, and its events, menus, prompts and WHOIS windows stay with it
- PARTIAL — connecting, registered, disconnected status and errors shown in the UI; directional IRC transcript appears automatically during connection/failure, with a menu toggle after registration and clipboard copy (parsed lines, not a complete socket capture)
- DONE — optional server PASS over TLS
- PARTIAL — SASL PLAIN over TLS with CAP negotiation; external-server interoperability unverified

## Channels

- PARTIAL — configured channels auto-join after registration; `/join` and `/part` work, with member snapshots refreshed on JOIN, PART, KICK, QUIT, NICK and channel MODE
- PARTIAL — live channel rows, or four mock channels across two networks
- DONE — width of the combined log's channel name column (Appearance `sub_log_name_width`, 80–600 px, default 162)
- PARTIAL — channel topic (on join and when changed) kept in application state and shown in the window title after the channel (with its member count once the roster is known, issue #146) and network; editing only through `/topic`
- TODO — channel modes relevant to normal use
- DONE — auto-join configured channels
- DONE — IRCnet `!` safe-channel targets in auto-join, messages, rosters, channel commands and WHOIS links; preserve the full name returned by the server

## Private messages

- DONE — dedicated private conversations: an incoming PRIVMSG opens one per peer and server (RFC 1459 case mapping), our own messages to that nickname (draft, `/msg`, member/WHOIS prompt, bouncer-relayed) join it, the same renderer/scrolling/avatars/previews/bounds as channels, unread/highlight/notification as private messages, nick changes followed conservatively, quits marked, closable from the channel tree; private NOTICEs join only an existing conversation (else the server log)
- PARTIAL — conversation persistence during a session: kept across disconnects, dropped when the server is connected again (like channel logs) or closed; no private-message history (CHATHISTORY TARGETS) yet

## Messages

- PARTIAL — receive/send channel and private PRIVMSG; no delivery receipt
- PARTIAL — receive/send channel and private NOTICE
- PARTIAL — `/me` sends CTCP ACTION; received ACTION is not specially rendered
- DONE — CTCP PING, VERSION, TIME and CLIENTINFO answered by NOTICE to private requests only, rate-limited (five per ten seconds, two per user); USERINFO, DCC and channel requests are not answered; requests and replies appear as readable server lines (`CTCP VERSION request from bob`), never as chat rows (D029)
- TODO — sending CTCP requests (`/ctcp`, `/ping` with lag display)
- PARTIAL — CTCP AVATAR (KVIrc protocol, experimental, per-server opt-in): realname mark when sharing, answers to private queries with an explicitly shared URL, bounded WHO/realname discovery and queries to marked users; URL only (no DCC); checked against wire fixtures from KVIrc's source, not a running KVIrc
- PARTIAL — local receive/send timestamps; with the per-server server-time opt-in, incoming channel messages, activity and private messages show the server's time (local HH:MM, receipt-time fallback); server log lines and dates are not shown
- PARTIAL — own nick changes update send identity; other nick/member changes remain pending
- PARTIAL — own joins/parts update active channels; channel logs show JOIN, PART, QUIT, NICK and channel MODE activity in English regardless of UI language, while other membership details remain pending
- PARTIAL — server responses and errors in the selected server log
- PARTIAL — HTTP(S) URLs in the upper channel log open on double-click, and the pointer is a hand over one (not while selecting); lower combined log switches channels on double-click
- PARTIAL — drag selection and copy of channel-message body text; server and lower logs are not selectable

## Member list

- PARTIAL — selected channel's NAMES roster in the upper-right pane, refreshed on JOIN, PART, KICK, QUIT, NICK and channel MODE; operators appear first, then names sort alphabetically within each group
- PARTIAL — operator/voice prefixes track NAMES and channel MODE changes; the member menu can request +o/-o
- PARTIAL — nick changes update the roster through the IRC client's channel list
- PARTIAL — join/part/quit synchronize the roster through the IRC client's channel list
- DONE — member context menu offers Whois, private-message composition, channel invite and +o/-o commands
- DONE — WHOIS replies requested by this client open a raised per-nick WHOIS window with join, private message and update actions; replies requested by other bouncer clients stay in the server log

## Navigation

- DONE — network/channel tree in the lower-right pane for application state
- DONE — channel tree context menu joins a parted channel or parts a joined one; channels not currently joined are grayed out
- PARTIAL — reference shortcut set for unread/previous/active/all/indexed channel and
  server navigation; live message/join status drives these sets, no quick switcher
- DONE — cyclic next/previous channel and server commands
- PARTIAL — jump to unread channels in mock or live mode; highlights remain TODO
- DONE — reference four-pane placement: main log over subwindow on the left,
  users over channel tree on the right, draft between left logs
- PARTIAL — subwindow displays other conversations, including live channel messages, and jumps on double-click; channel/server labels ellipsize on one line; each server can have a display name (settings) shown instead of the first word of its host

## Unread and highlights

- PARTIAL — unread IDs, visual marks and clearing on selection for mock/live channel messages
- PARTIAL — nickname mentions (whole word including `@nick`, RFC 1459 case mapping) and keywords drive desktop notifications, are drawn bold in the configurable highlight color (light/dark) in both logs, and turn unread channels in the tree to that color until selected
- DONE — configurable notification keywords (comma-separated, case-insensitive substring), switched separately from mentions
- TODO — window/application attention indication

## Input and commands

- PARTIAL — editable single-line draft; Enter sends to a joined channel
- PARTIAL — Tab completes listed member nicknames; Ctrl+Enter sends channel NOTICE
- PARTIAL — `/` commands, common channel-target inference, `/raw`/`/quote`; command history remains pending
- TODO — command history
- PARTIAL — initial/switch focus, common OS editing shortcuts, undo/redo and
  platform-specific app bindings; user-customized macOS text bindings unsupported
  by GPUI 0.2.2, Windows/Linux runtime unverified
- PARTIAL — CR/LF normalized to spaces; 512-byte encoded IRC-line limit

## Attachments

- DONE — paste an image-only clipboard (including screenshots) or drop one image file on the draft row; both share one flow. HEIC/HEIF/AVIF are recognized, and on macOS Photos/Mail/screenshot-thumbnail file-promise drags are accepted
- DONE — confirmation before upload naming the provider and the public-link consequence; guidance to settings when no provider or account is configured, and to reconnect after an authentication failure
- DONE — ImgBB external upload with progress, cancel (abandons the wait), network/auth/rejection handling; the link is inserted into the originating draft and never sent automatically
- PARTIAL — inline previews of direct image links (PNG, JPEG, GIF and WebP, static first frame) in the main channel log, behind an Appearance setting that is off by default; bounded, HTTP(S)-only loading of public hosts. The combined log, server log and activity lines stay text-only; no video or page previews
- PARTIAL — user avatars (small static 16×16 images) beside main-log channel messages and in the member list, behind an Appearance setting that is off by default and independent of previews; loaded only for visible rows with a separate bounded cache; users without one get a client-drawn default avatar derived from the nickname (defaultAvatar.js). IRC avatars come from the experimental metadata support below (no separate switch), including publishing and removing our own from the IRCv3 tab, and, where the server has no metadata, from KVIrc-compatible CTCP AVATAR (per-server opt-in, URL only; D025); channel/network icons are not implemented
- TODO — clipboard file references (copied files) as attachments; they paste as text today
- OUT OF SCOPE (for now) — video/audio attachments, a transfer manager, Matrix native media

## Logging and history

- TODO — local logs
- TODO — searchable history
- DONE — bounded in-memory channel and server scrollback
- PARTIAL — IRCv3/server history integration where available (`draft/chathistory`, experimental, per-server opt-in, default off): recent channel history on join (up to 50 lines) and older pages when the main log is scrolled to its top (`BEFORE`, up to 50 lines a page, msgid or timestamp reference within `MSGREFTYPES`, until the server reports the beginning or the 2,000-line log bound is reached); deduplicated, quiet, and the viewport stays on the same line; after a reconnect, joined channels recover the most recent missed lines (up to 50, `LATEST` after the last msgid or server time, placed where the log was cut, with a quiet note when more may be missing); checked against Ergo v2.19.1; no private-message history or recovery, and no persistence

## Notifications

- PARTIAL — OS desktop notifications through notify-rust: org.freedesktop.Notifications on Linux, NSUserNotificationCenter on macOS (app bundle only), WinRT toasts on Windows (attributed to the PowerShell AppUserModelID until CayenChat registers its own); no action on click
- TODO — per-network/channel controls
- DONE — mention, keyword and private-message notifications, suppressed for the selected conversation of the focused chat window and limited to 5 per 10 seconds; replayed history (lines without a user mask such as Tiarra's Log::Recent, IRCv3 `chathistory`/`znc.in/playback` batches) neither notifies nor highlights; an old server-time alone does not count as history

## Settings

- DONE — single-network connection settings with a server drop-down, multiple saved custom profiles, per-profile host/port/TLS/certificate verification/encoding and optional server password (sent without TLS only after a per-server opt-in with a warning, for bouncers such as ZNC)
- DONE — persisted nickname, independent IRC `USER` username and optional SASL account name; per-server server/SASL password persistence in the credential store (system store, or an explicitly chosen local file after a plaintext warning), with immediate deletion when disabled
- DONE — Credential Storage tab: system secure storage (Keychain, Credential Manager, Secret Service) or a `0600` local file; availability probe, confirmation for the local file, migration of saved secrets when switching and of pre-version-11 plaintext passwords on startup
- DONE — Image Upload tab: provider None/ImgBB, connect/reconnect/disconnect account (account API key)
- DONE — settings open in a separate window; closing it leaves the chat window running
- DONE — settings categories are a left-hand list (Up/Down move through it) beside the selected category's page; each page except Connection and Credentials ends with a "Restore Defaults" (Japanese: すべて元に戻す) at the bottom right for exactly what it shows, disabled when nothing differs and confirmed first (D038)
- DONE — Experimental settings tab with opt-in stderr file logging, native destination picker, immediate switching, append and saved preferences; Windows release builds suppress the startup console (Windows runtime verification pending)
- DONE — settings save automatically as they change (no Save buttons); passwords are stored when their field loses focus or the window closes; removing a saved server asks first (D021)
- DONE — persisted channel auto-join list
- DONE — opt-in startup connection to the selected saved server; invalid saved settings reopen the settings window
- DONE — Japanese and English UI catalogs with a persisted language choice; System follows the OS locale (Japanese when available, English otherwise)
- DONE — Notifications tab: enable, mentions, private messages, keyword alerts and keywords (Japanese UI: キーワード通知)
- DONE — IRCv3 tab: per-server opt-ins for server timestamps, message tags, and message batches, naming the configured server; changes apply on the next connection; and our own avatar: a URL draft (typed, or filled by uploading a dropped, pasted or chosen image through the configured image host) (the uploaded one is sent at once) and Send to / Remove from IRC Server shown only when they would change something on the connected server; the experimental peer avatar option with explicit Share with Peers / Stop Sharing
- PARTIAL — separate Appearance tab persists member-list and log background colors, channel event text color, alternating message rows, and font families for logs, users, tree, input and monospace time
- PARTIAL — Windows/Linux Keyboard tab: channel-number modifier (Ctrl/Alt/Super) and, on Linux, draft editing keys (Follow GTK/Standard/Emacs, GTK 3 `gtk-key-theme` via portal or `settings.ini`); other shortcuts are fixed. The Emacs keys were confirmed working on a Debian machine on 2026-09-26

## IRCv3

- PARTIAL — shared CAP negotiation (LS 302 with continuation and values, per-capability REQ, multiline ACK, NAK, NEW/DEL, coordinated CAP END) for SASL PLAIN and opt-in extensions; public-server interoperability unverified
- PARTIAL — message tags: per-server opt-in (default off), tags parsed and normalized, TAGMSG kept out of the chat; no tag is displayed yet, and it is not requested with legacy encodings
- DONE — server-time: per-server opt-in (default off), independent of message tags
- PARTIAL — batch (opt-in per server, default off): receives batches and recognizes `chathistory`/`znc.in/playback` history (including nested batches) so it does not notify; checked against local fixtures only; the batch option itself sends no CHATHISTORY requests
- PARTIAL — metadata (`draft/metadata-2`, experimental, requested whenever offered, with batch as its prerequisite; display follows the single "Show user avatars" switch): the user `avatar` key only — SUB, METADATA/761/766, deferred SYNC, bounded GET lookups for users who join later, and explicit SET to publish or remove our own avatar with server-confirmed feedback; checked against Ergo v2.19.1 and local fixtures; no other keys, channel avatars, LIST/CLEAR, MONITOR or `before-connect`
- DONE — echo-message and labeled-response (IRCv3 standard, one per-server opt-in "Server-confirmed sending", default off): the message still appears at once; the server's echo confirms it in place (final text, msgid, server time), a labeled ACK/error/batch names it exactly, a rejected or unconfirmed message is drawn in the warning color; what other clients of the account send shows as before. `labeled-response` needs `batch` and `message-tags` (requested automatically, UTF-8 connections only); on a server with only `echo-message` an echo is matched by target and identical text
- PARTIAL — account-related capabilities: `account-notify` and `extended-join` (IRCv3 standard, per-server opt-in "User accounts", default off) track the services account and real name of channel members and complete WHOIS; initial state through one WHOX query per joined channel where the server has WHOX; `account-tag` is separate work
- DONE — extended-join (with the "User accounts" opt-in above)
- DONE — account-tag (IRCv3 standard): the sender's services account at send time is retained on each channel/private/history message (`Message::account`, not shown in the log; the WHOIS dialog keeps showing the 330 account). Requested when a CAP negotiation happens anyway, on UTF-8 connections only
- DONE — setname (IRCv3 standard): the per-server `Real name` setting is sent in `USER`; changing it while connected sends `SETNAME` when the server enabled `setname`, otherwise it applies at the next connection. Requested only when a CAP negotiation happens anyway (SASL or another opt-in), so plain registration is unchanged. Others' SETNAME changes are not tracked
- TODO — away-notify
- PARTIAL — chathistory (`draft/chathistory`, experimental, per-server opt-in): `LATEST` on join (with a reference after a reconnect) and `BEFORE` on scroll, for joined channels only (see Logging and history); `TARGETS` once per connection finds direct-message conversations with messages since the previous disconnect (default one day, at most a week, 16 peers) and asks their latest 50 lines quietly; no AFTER/BETWEEN/AROUND, no channel discovery
- TODO — other capabilities based on real-world usefulness

## Appearance

- DONE — compact default layout
- DONE — plain timestamp/right-aligned nickname/text rows with scrolling and follow-to-bottom unless the user scrolls away
- TODO — light/dark appearance
- PARTIAL — selectable installed font families per pane; default timestamp fonts are Menlo, Consolas, or DejaVu Sans Mono by platform, with Windows rendering pending; on Linux, unset pane fonts fall back through cosmic-text, whose exact locale match treats `ja-JP` as non-Japanese and renders Han characters with Simplified Chinese glyphs (Noto Sans CJK SC) — choosing a Japanese font explicitly avoids this
- TODO — restrained theming

## Platform integration

- PARTIAL — macOS native application, connection, edit and diagnostic menus; Command+, opens the separate settings window
- PARTIAL — Windows/Linux Alt alone or F10 toggles an in-window menu bar (also revealed by resting the pointer just below the title bar) in the chat window (the settings window has none, issue #145), with shared app/connection/edit/view/window actions, arrows/Enter navigation and Escape/outside-click dismissal; Ctrl+, opens settings and Ctrl+Shift+D/L controls diagnostics. Native GPUI menus remain unavailable there; platform runtime verification is pending.
- PARTIAL — notifications on macOS, Windows and Linux; macOS delivery reached the system (first-use permission banner) from the development bundle, Windows/Linux runtime unverified
- PARTIAL — common macOS/Windows editing shortcuts; Windows runtime and custom
  macOS bindings unverified
