# Project

## Goal

Build a modern cross-platform IRC client inspired by the open-source version of LimeChat, with a Chocoa-like look and feel.

The application should feel like a focused desktop tool rather than a modern social/chat product: compact, direct, keyboard-friendly, information-dense, and unobtrusive.

## Reference applications

### LimeChat

Use the open-source LimeChat implementation as the primary reference for IRC-oriented behavior, workflows, feature scope, and interaction patterns.

Important characteristics include:

- multiple IRC networks in one window
- efficient server/channel navigation
- rich keyboard operation
- fast, stable long-running operation
- IRC-specific settings and commands
- unread/highlight handling

Do not assume LimeChat's internal architecture should be reproduced.

### Chocoa

Use Chocoa as the main aesthetic and interaction reference.

The intended character is:

- simple and utilitarian
- compact rather than spacious
- desktop-native rather than web-like
- low visual noise
- obvious controls and state
- chat log first, decoration second
- suitable for leaving open all day
- four simultaneous information panes in a two-column layout: selected-channel
  log above the other-channel subwindow on the left, and selected-channel users
  above the network/channel tree on the right; draft input between the left logs

Do not attempt pixel-perfect historical emulation. Reproduce the feel, density, and directness using modern platform conventions.

## Platforms

Primary targets:

- macOS
- Windows

Linux may be considered later, but portable core logic is preferred.

## Technology direction

- Rust
- GPUI for the desktop UI
- async networking separated from UI state
- protocol/application logic must remain independent from GPUI

The initial connection layer uses `irc` 1.1.0 behind the GPUI-free `irc-core` boundary (see D007). The dependency can be revisited as IRCv3 and authentication requirements develop.

## Product principles

1. IRC is the product. Avoid generic messenger features unless they improve IRC use.
2. Keyboard operation is a first-class path, not an accessibility afterthought.
3. Prefer compact information density over large modern-chat spacing.
4. Keep common actions immediately available and predictable.
5. Preserve IRC concepts instead of hiding them behind proprietary abstractions.
6. The client should remain comfortable during long-running use with many channels.
7. Avoid unnecessary animation, decorative chrome, cards, bubbles, and oversized controls.
8. Keep text editing consistent with standard OS key bindings; app navigation must not
   take over text-editing shortcuts while the draft has focus.

## Initial milestone

The first usable milestone should support:

- native application window
- one IRC network
- TLS connection
- nickname configuration
- joining channels
- receiving and displaying messages
- sending messages
- channel/user state sufficient for normal conversation
- disconnect/reconnect
- persistent basic configuration

The first milestone does not require complete LimeChat parity.

## Later scope

Likely later features include:

- multiple networks
- private messages
- live member list with role and membership updates
- unread/highlight state
- notifications
- keyboard shortcuts
- logs/history
- additional SASL mechanisms beyond PLAIN
- IRCv3 capabilities
- themes/appearance preferences
- scripting or automation hooks
