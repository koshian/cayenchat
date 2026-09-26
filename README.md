# CayenChat

[Japanese README](README.ja.md)

A compact native IRC desktop client in Rust and GPUI. Configure a server in the app, connect, and auto-join channels. Connection preferences persist; chat history does not.

## License and artwork

CayenChat's original source and `icon.png` are licensed under [GPL version 3 only](LICENSE). The adapted GPUI text input in `crates/ui/src/input.rs` remains Apache-2.0, and third-party dependencies retain their own licenses; see [third-party notices](THIRD_PARTY_NOTICES.md). GPLv2 is not used because GPUI is Apache-2.0 and the two licenses are incompatible. Preserve these notices when distributing the application.

## Build and run

Use Rust 1.88 or later with Cargo (1.95.0 on macOS and 1.98.1 on Linux are tested). GPUI uses `let` chains, which older compilers reject. Debian's stock rustc (1.85) is too old; use backports or rustup. Run commands from the repository root. Cargo uses the committed lockfile; the first build downloads Rust dependencies unless they are cached.

```sh
cargo build --locked -p cayenchat-ui
cargo run --locked -p cayenchat-ui
```

The debug executable is `target/debug/cayenchat`. For an optimized executable, run `cargo build --locked --release -p cayenchat-ui`; the result is `target/release/cayenchat` (or `cayenchat.exe` on Windows).

## Connect to IRC

Settings open in a separate window at startup by default, so closing that window leaves the four-pane chat window running. Reopen them with **CayenChat → Settings…** on macOS (`Cmd+,`), or `Ctrl+,` on Windows/Linux. Use the **Connection** tab to enter a nickname, choose a server from the **Server** drop-down, then click **Save and connect**. The default is `irc.ircnet.ne.jp` on port `6667` with TLS off; `irc6.ircnet.ne.jp` is also built in. Choose **+ Add server…** from the list to save another host. Added servers appear before the built-in servers. The host, port, TLS certificate-verification setting, and character encoding belong to each server entry. List auto-join channels separated by commas, such as `#first,#second`. A channel list is optional: you can join later with `/join #channel`.

Choose **Character encoding** for each server: UTF-8 (default), ISO-2022-JP, Shift_JIS, or EUC-JP. IRC does not mandate one character set, including for channel names. CayenChat encodes and decodes the entire IRC line, including Japanese channel names, with the selected server encoding. A channel name must use the same encoding each time it is joined or addressed; select the encoding used by that network before joining. ISO-2022-JP channel names can contain comma or colon *bytes* inside Japanese characters (for example two U+304C characters after `#`); whether such names work across a network depends on its servers. The local wire test verifies that CayenChat itself keeps those bytes inside one channel name. Text that cannot be represented in the selected encoding is rejected without clearing the draft. A server advertising `UTF8ONLY` requires the UTF-8 setting.

Enable **TLS/SSL** for an encrypted connection; switching it on changes the standard port from `6667` to `6697` if that port has not been customized. TLS certificates are verified by default. When TLS is on, **Verify certificates** can be turned off for that server, for example for a self-signed Tiarra certificate. With verification off, the client cannot authenticate the server and a third party could impersonate it. Turning TLS off resets certificate verification to on. Server passwords and **SASL PLAIN** require TLS, but sending them with verification off does not protect against server impersonation. SASL needs an account name and password, and the server must advertise `sasl` with PLAIN support.

The **Save** button persists preferences without reconnecting. Use **Back** or the window close button to close only settings. **Disconnect** disconnects the current session; **Save and connect** reconnects with the form's current values. **Connect when the app starts** is off by default; enable and save it to connect to the selected server when the app next starts. The settings window stays closed on a valid automatic start, and opens if the saved configuration cannot start a connection. Auto-connect uses only passwords explicitly saved for that server. There is no automatic reconnect after disconnection yet.

Preferences are stored in the platform user configuration directory under `CayenChat/settings.json` (for example, `~/Library/Application Support/CayenChat/settings.json` on macOS). Existing version 1–8 settings are read and converted when next saved. Missing certificate-verification and startup-connection values default to on and off respectively; saved choices are retained. The old default background `#ECECEC` becomes white in version 8, and the old default channel event color `#3B7655` becomes `#007D00` in version 9; other saved colors remain unchanged. Server and SASL passwords are masked in the form. **Save passwords** is off by default for each server; turning it on asks for confirmation that both passwords will be stored as plaintext in that file. Turning it off immediately removes that server's saved passwords. With it off, passwords entered for a connection are not written to disk. A running connection is marked in the channel tree; select the server to inspect registration messages and errors.

During connection and after a failed connection, the diagnostic transcript is shown automatically in the selected channel or server view. On every platform, the **Connection diagnostics** button above the main log opens the server transcript at its start, and **Copy diagnostics** copies its contents. After registration, the macOS native **View → Show Connection Diagnostics** menu shows the full transcript in the server view; **Copy Connection Diagnostics** copies it, including the final disconnect reason. The same actions use `Cmd+Shift+D` / `Cmd+Shift+L` on macOS and `Ctrl+Shift+D` / `Ctrl+Shift+L` on Windows/Linux. Entries marked `→` are IRC commands queued for sending and `←` are received IRC lines, interleaved with DNS, TCP/TLS, and registration progress. The transcript includes chat message bodies and retains the latest 1,000 entries in memory, so review it before sharing a copy. Server PASS, SASL payloads, OPER passwords, channel keys, and recognized NickServ/ChanServ credential commands are masked. IRC lines are shown after character-set decoding and parsing; this is not a byte-for-byte TLS or packet capture. The library can generate other maintenance traffic that is not surfaced in this transcript. GPUI 0.2.2 does not display native menus on Windows/Linux yet; `Ctrl+,` opens the separate settings window there.

## Appearance

The separate settings window has **Connection** and **Appearance** tabs. In **Appearance**, enter `#RRGGBB` colors for the member list background (white by default), upper channel log, and lower combined log. Enable alternating rows to apply the corresponding alternate color to every second message. Choose installed font families for the upper and lower logs, member list, channel tree, and draft input; type in a font field to filter the selection list. Empty font fields use the system font. Timestamps use a monospaced font by default (Menlo on macOS, Consolas on Windows, and DejaVu Sans Mono on Linux). **Save and apply** persists and applies appearance changes without reconnecting. The nickname column is about 12 characters wide; longer nicks are shortened visually.

## Language

The **Language** control in the Connection tab offers **System language**, **Japanese**, and **English**. System language is the default: a Japanese OS locale selects Japanese; all other locales select English. Selecting a language previews it in Settings; **Save** applies it to the chat window and menus without reconnecting. The catalogs are `locales/en.json` and `locales/ja.json`. The app reads packaged files at runtime and falls back to bundled copies if those files are unavailable.

## IRC commands

Type `/` at the beginning of the draft and press Enter to send an IRC command after registration. Ordinary draft text sends `PRIVMSG` to the selected joined channel; `Ctrl+Enter` sends an IRC `NOTICE`. The following commands use the selected channel when the channel argument is omitted:

| Example | Effect |
| --- | --- |
| `/join #other` | Join another channel |
| `/part` or `/part Leaving now` | Leave the selected channel |
| `/topic New topic` | Set the selected channel's topic; `/topic` queries it |
| `/mode +o alice` | Change a mode on the selected channel |
| `/kick bob goodbye` | Kick a user from the selected channel |
| `/invite bob` | Invite a user to the selected channel |
| `/names` | Request the selected channel's member list |
| `/me waves` | Send a CTCP ACTION to the selected channel |
| `/msg :hello everyone` or `/notice :hello everyone` | Message or notify the selected channel |
| `/msg bob hello` or `/notice bob hello` | Send directly to a named target |
| `/nick newname`, `/whois bob`, `/raw WHO #channel` | Send other IRC commands |

For `MSG`, `PRIVMSG`, and `NOTICE`, the leading `:` in the message form above explicitly selects the current channel. `/raw` and `/quote` send the remaining line as an IRC command. Server responses appear in the server log; private-message conversations do not yet have their own pane. Commands and messages reject protocol line breaks and oversized lines.

Platform prerequisites:

| Platform | Required environment | Verification |
| --- | --- | --- |
| macOS | Full Xcode with its macOS SDK and command-line tools selected via `xcode-select`; a Metal-capable Mac. | Built and run on Apple Silicon/macOS 26.6.2 with Rust 1.95.0 and Xcode 26.6. |
| Windows | Stable Rust MSVC toolchain, Visual Studio or Build Tools with Desktop development with C++, and a Windows SDK. Run from a Developer shell if required. | Not yet built or run on Windows. |
| Linux | Rust 1.88+, a C toolchain and `pkg-config`, and a Vulkan-capable GPU/driver. On Debian/Ubuntu: `sudo apt install build-essential pkg-config libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libvulkan1 mesa-vulkan-drivers`. See [Zed's Linux build prerequisites](https://zed.dev/docs/development/linux) for a broader upstream dependency reference; this smaller project may need fewer packages. | Wayland and X11 features are enabled; not yet built or run on Linux. |

On macOS, an optional development `.app` bundle with the `icon.png` artwork can be created with:

```sh
sh scripts/bundle-macos.sh
open target/CayenChat.app
```

The bundle stays in `target/`; it is not signed, notarized, or installed. It contains the icon derived from `icon.png`. The Windows executable embeds an `.ico` derived from the same image. The generated `.icns` and `.ico` files are committed so normal builds need no image tools; after changing `icon.png`, run `python3 scripts/generate-icons.py` with Pillow installed. After code changes, quit every running CayenChat instance with Cmd+Q, rerun `sh scripts/bundle-macos.sh`, and then open the bundle again; `cargo build` alone does not replace its executable. For code changes, run:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --all-features --locked
cargo test --workspace --locked
```

See [development requirements](spec/development.md) for more platform detail.

## Layout and current behavior

Drag across message bodies in the upper channel log and use `Cmd+C` / `Ctrl+C` to copy the selected text. Double-click an HTTP or HTTPS URL there to open it in the system browser. The lower combined log keeps single-click channel switching and does not open URLs.

The main log is above the other-channel subwindow on the left. The draft input sits between them. The selected channel's members are above the server/channel tree on the right. Selecting a server shows its connection status and server messages, with an empty member list. Clicking a subwindow message opens its source channel. Long channel and server names in the subwindow are shortened with an ellipsis to keep each label on one line. The subwindow orders messages by arrival, even when different channels receive lines during the same displayed minute. The main and subwindow logs follow new messages to the bottom, including a burst of channel history during initial connection. Scrolling up pauses following for that log; scrolling back to the bottom resumes it. Each server or channel keeps a separate in-memory draft until exit.

Messages arriving in other channels mark them unread; opening one clears its mark. Active navigation follows registered/joined state. When no unread channel remains, the unread shortcuts leave the selection unchanged. Channel number shortcuts use the visible channel order across the server tree; `1` selects the first and `0` the tenth. A number beyond the available channels leaves the selection unchanged.

## Keyboard shortcuts

`Cmd` means Command and `Opt` means Option on macOS. `Ctrl` and `Alt` refer to the corresponding Windows/Linux keys. The Windows/Linux assignments avoid OS-reserved Alt+Tab/Alt+Space and leave Ctrl+Left/Right available for word movement in the draft.

| Action | macOS | Windows / Linux |
| --- | --- | --- |
| Complete a nickname from the selected channel's member list; repeat to cycle matches | `Tab` | `Tab` |
| Next unread channel | `Ctrl+Tab` or `Opt+Space` | `Ctrl+Tab` |
| Previous unread channel | `Ctrl+Shift+Tab` or `Opt+Shift+Space` | `Ctrl+Shift+Tab` |
| Previously selected channel | `Opt+Tab` | `Alt+Left` |
| Previous / next active channel | `Cmd+Up/Down`, `Cmd+Opt+Up/Down`, or `Cmd+{/}` | `Ctrl+PageUp/PageDown` |
| Previous / next channel | `Ctrl+Up/Down` | `Alt+Up/Down` |
| Previous / next active server | `Cmd+Opt+Left/Right` | `Ctrl+Alt+PageUp/PageDown` |
| Previous / next server | `Ctrl+Left/Right` | `Alt+PageUp/PageDown` |
| First through tenth channel | `Cmd+1..9, 0` | `Ctrl+1..9, 0` |
| First through tenth server | `Cmd+Ctrl+1..9, 0` | `Ctrl+Alt+1..9, 0` |
| Send a channel message (`PRIVMSG`) | `Enter` | `Enter` |
| Send as IRC `NOTICE` | `Ctrl+Enter` | `Ctrl+Enter` |
| Open connection settings | `Cmd+,` | `Ctrl+,` |
| Show/hide connection diagnostics | `Cmd+Shift+D` | `Ctrl+Shift+D` |
| Copy connection diagnostics | `Cmd+Shift+L` | `Ctrl+Shift+L` |
| Copy selected upper-log text | `Cmd+C` | `Ctrl+C` |

Ordinary text sends only after the selected channel has been joined; slash commands can also be entered from the server view after registration. Successful local queueing clears the draft. Sending while disconnected or joining displays feedback and retains the draft. The input implements common platform editing bindings, selection, paste, undo/redo, and IME text input; GPUI 0.2.2 does not yet honor user-defined macOS `DefaultKeyBinding.dict` mappings. The app shortcuts above take precedence where they overlap input editing keys.

On Windows, Send and nickname completion are suppressed while the text input has an active IME composition. The included GPUI 0.2.2 copy patches its handling of unrecognized language keys and IME-processed keys, following the relevant parts of an [upstream fix](https://github.com/zed-industries/zed/pull/41259). MS-IME switching with the Half-width/Full-width key still needs verification on a Windows 11 machine.

The specifications under [`spec/`](spec/) are authoritative; see [architecture](spec/architecture.md) and [feature parity](spec/feature-parity.md) for implementation scope.
