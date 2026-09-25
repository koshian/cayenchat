# CayenChat

A compact native IRC desktop client in Rust and GPUI. Configure a server in the app, connect, and auto-join channels. Connection preferences persist; chat history does not.

## License and artwork

CayenChat's original source and `icon.png` are licensed under [GPL version 3 only](LICENSE). The adapted GPUI text input in `crates/ui/src/input.rs` remains Apache-2.0, and third-party dependencies retain their own licenses; see [third-party notices](THIRD_PARTY_NOTICES.md). GPLv2 is not used because GPUI is Apache-2.0 and the two licenses are incompatible. Preserve these notices when distributing the application.

## Build and run

Use a current stable Rust toolchain with Cargo (Rust 1.95.0 is tested). Run commands from the repository root. Cargo uses the committed lockfile; the first build downloads Rust dependencies unless they are cached.

```sh
cargo build --locked -p cayenchat-ui
cargo run --locked -p cayenchat-ui
```

The debug executable is `target/debug/cayenchat`. For an optimized executable, run `cargo build --locked --release -p cayenchat-ui`; the result is `target/release/cayenchat` (or `cayenchat.exe` on Windows).

## Connect to IRC

Settings open in a separate window at startup by default, so closing that window leaves the four-pane chat window running. Reopen them with **CayenChat → 接続設定…** on macOS (`Cmd+,`), or `Ctrl+,` on Windows/Linux. Use the **接続** tab to enter a nickname, choose a server from the **接続先** drop-down, then click **保存して接続**. The default is `irc.ircnet.ne.jp` on port `6667` with TLS off; `irc6.ircnet.ne.jp` is also built in. Choose **＋ サーバーを追加…** from the list to save another host. Added servers appear before the built-in servers. The host, port, TLS certificate-verification setting, and character encoding belong to each server entry. List auto-join channels separated by commas, such as `#first,#second`. A channel list is optional: you can join later with `/join #channel`.

Choose **文字コード** for each server: UTF-8 (default), ISO-2022-JP, Shift_JIS, or EUC-JP. IRC does not mandate one character set, including for channel names. CayenChat encodes and decodes the entire IRC line, including Japanese channel names, with the selected server encoding. A channel name must use the same encoding each time it is joined or addressed; select the encoding used by that network before joining. ISO-2022-JP channel names can contain comma or colon *bytes* inside Japanese characters (for example `#がが`); whether such names work across a network depends on its servers. The local wire test verifies that CayenChat itself keeps those bytes inside one channel name. Text that cannot be represented in the selected encoding is rejected without clearing the draft. A server advertising `UTF8ONLY` requires the UTF-8 setting.

Enable **TLS/SSL** for an encrypted connection; switching it on changes the standard port from `6667` to `6697` if that port has not been customized. TLS certificates are verified by default. When TLS is on, **証明書の検証** can be turned off for that server, for example for a self-signed Tiarra certificate. With verification off, the client cannot authenticate the server and a third party could impersonate it. Turning TLS off resets certificate verification to on. Server passwords and **SASL PLAIN** require TLS, but sending them with verification off does not protect against server impersonation. SASL needs an account name and password, and the server must advertise `sasl` with PLAIN support.

The **保存** button persists preferences without reconnecting. Use **戻る** or the window close button to close only settings. **切断** disconnects the current session; **保存して接続** reconnects with the form's current values. **アプリ起動時に自動接続する** is off by default; enable and save it to connect to the selected server when the app next starts. The settings window stays closed on a valid automatic start, and opens if the saved configuration cannot start a connection. Auto-connect uses only passwords explicitly saved for that server. There is no automatic reconnect after disconnection yet.

Preferences are stored in the platform user configuration directory under `CayenChat/settings.json` (for example, `~/Library/Application Support/CayenChat/settings.json` on macOS). Existing version 1–5 settings are read and converted when next saved, with certificate verification enabled and startup connection disabled. Server and SASL passwords are masked in the form. **パスワードを保存する** is off by default for each server; turning it on asks for confirmation that both passwords will be stored as plaintext in that file. Turning it off immediately removes that server's saved passwords. With it off, passwords entered for a connection are not written to disk. A running connection is marked in the channel tree; select the server to inspect registration messages and errors.

During connection and after a failed connection, the diagnostic transcript is shown automatically in the selected channel or server view. After registration, the macOS native **表示 → 接続診断を表示** menu shows the full transcript in the server view; **接続診断をコピー** copies it, including the final disconnect reason. The same actions use `Cmd+Shift+D` / `Cmd+Shift+L` on macOS and `Ctrl+Shift+D` / `Ctrl+Shift+L` on Windows/Linux. Entries marked `→` are IRC commands queued for sending and `←` are received IRC lines, interleaved with DNS, TCP/TLS, and registration progress. The transcript includes chat message bodies and retains the latest 1,000 entries in memory, so review it before sharing a copy. Server PASS, SASL payloads, OPER passwords, channel keys, and recognized NickServ/ChanServ credential commands are masked. IRC lines are shown after character-set decoding and parsing; this is not a byte-for-byte TLS or packet capture. The library can generate other maintenance traffic that is not surfaced in this transcript. GPUI 0.2.2 does not display native menus on Windows/Linux yet; `Ctrl+,` opens the separate settings window there.

## Appearance

The separate settings window has **接続** and **外観** tabs. In **外観**, enter `#RRGGBB` colors for the window background, upper channel log, and lower combined log. Enable alternating rows to apply the corresponding alternate color to every second message. Choose installed font families for the upper and lower logs, member list, channel tree, and draft input; type in a font field to filter the selection list. Empty font fields use the system font. Timestamps use a monospaced font by default (Menlo on macOS, Consolas on Windows, and DejaVu Sans Mono on Linux). **保存して適用** persists and applies appearance changes without reconnecting. The nickname column is about 12 characters wide; longer nicks are shortened visually.

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
| Linux | Stable Rust, a Vulkan-capable GPU/driver, and native development libraries for GPUI's Wayland or X11 backend. See [Zed's Linux build prerequisites](https://zed.dev/docs/development/linux) for a broader upstream dependency reference; this smaller project may need fewer packages. | Wayland and X11 features are enabled; not yet built or run on Linux. |

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

On Windows, Send and nickname completion are suppressed while the text input has an active IME composition. GPUI 0.2.2 predates an [upstream fix for Japanese and other IME switching keys](https://github.com/zed-industries/zed/pull/41259); the mode-switching issue still needs verification on a Windows machine.

The specifications under [`spec/`](spec/) are authoritative; see [architecture](spec/architecture.md) and [feature parity](spec/feature-parity.md) for implementation scope.
