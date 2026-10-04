# Keyboard shortcuts

[日本語](SHORTCUTS.ja.md)

`Cmd` means Command and `Opt` means Option on macOS. `Ctrl` and `Alt` refer to the corresponding Windows/Linux keys.

| Action | macOS | Windows / Linux |
| --- | --- | --- |
| Complete a nickname from the selected channel's member list; repeat to cycle matches | `Tab` | `Tab` |
| Next unread channel | `Ctrl+Tab` or `Opt+Space` | `Ctrl+Tab` |
| Previous unread channel | `Ctrl+Shift+Tab` or `Opt+Shift+Space` | `Ctrl+Shift+Tab` |
| Previously selected channel | `Opt+Tab` | `Alt+Left` |
| Previous / next active channel | `Cmd+Up/Down` or `Cmd+Opt+Up/Down` | `Ctrl+PageUp/PageDown` |
| Previous / next channel | `Cmd+[` / `Cmd+]` or `Ctrl+Up/Down` | `Alt+Up/Down` |
| Previous / next active server | `Cmd+Opt+Left/Right` | `Ctrl+Alt+PageUp/PageDown` |
| Previous / next server | `Cmd+Shift+[` / `Cmd+Shift+]` or `Ctrl+Left/Right` | `Alt+PageUp/PageDown` |
| First through tenth channel | `Cmd+1..9, 0` | `Ctrl+1..9, 0` (configurable) |
| First through tenth server | `Cmd+Ctrl+1..9, 0` | `Ctrl+Alt+1..9, 0` |
| Send a channel message (`PRIVMSG`) | `Enter` | `Enter` |
| Send as IRC `NOTICE` | `Ctrl+Enter` | `Ctrl+Enter` |
| Recall an older / newer sent draft (last 20, this session only) | `Up` / `Down` | `Up` / `Down` |
| Open connection settings | `Cmd+,` | `Ctrl+,` |
| Show/hide connection diagnostics | `Cmd+Shift+D` | `Ctrl+Shift+D` |
| Copy connection diagnostics | `Cmd+Shift+L` | `Ctrl+Shift+L` |
| Copy selected upper-log text | `Cmd+C` | `Ctrl+C` |

On Windows/Linux, press and release Alt alone or press F10 to show the menu bar.

## While a list has focus

Click the channel tree or the member list to focus it. These keys act only on the focused list; in the draft, `J`, `K` and the arrow keys keep their text meaning.

| Action | Keys |
| --- | --- |
| Previous / next row | `Up` / `Down`, or `K` / `J` |
| First / last row | `Home` / `End` |
| One page up / down | `PageUp` / `PageDown` |
| Channel tree: to the server / its first channel | `Left` / `Right` |
| Channel tree: open the selected channel and return to the draft | `Enter` |

## IRC commands

Type `/` at the beginning of the draft to send an IRC command. When the channel argument is omitted, the selected channel is used.

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

`/raw` and `/quote` send the rest of the line as an IRC command.
