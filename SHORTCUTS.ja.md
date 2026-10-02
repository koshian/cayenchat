# キーボードショートカット

[English](SHORTCUTS.md)

`Cmd` は macOS の Command、`Opt` は Option を表します。`Ctrl` と `Alt` は Windows/Linux の対応するキーです。

| 操作 | macOS | Windows / Linux |
| --- | --- | --- |
| 選択チャンネルのメンバーからニックネームを補完。繰り返すと候補を切り替え | `Tab` | `Tab` |
| 次の未読チャンネル | `Ctrl+Tab` または `Opt+Space` | `Ctrl+Tab` |
| 前の未読チャンネル | `Ctrl+Shift+Tab` または `Opt+Shift+Space` | `Ctrl+Shift+Tab` |
| 直前に選択したチャンネル | `Opt+Tab` | `Alt+Left` |
| 前後のアクティブなチャンネル | `Cmd+Up/Down`、`Cmd+Opt+Up/Down`、`Cmd+{/}` | `Ctrl+PageUp/PageDown` |
| 前後のチャンネル | `Ctrl+Up/Down` | `Alt+Up/Down` |
| 前後のアクティブなサーバー | `Cmd+Opt+Left/Right` | `Ctrl+Alt+PageUp/PageDown` |
| 前後のサーバー | `Ctrl+Left/Right` | `Alt+PageUp/PageDown` |
| 1～10番目のチャンネル | `Cmd+1..9, 0` | `Ctrl+1..9, 0`（変更可） |
| 1～10番目のサーバー | `Cmd+Ctrl+1..9, 0` | `Ctrl+Alt+1..9, 0` |
| チャンネル発言（`PRIVMSG`）を送信 | `Enter` | `Enter` |
| IRC `NOTICE` を送信 | `Ctrl+Enter` | `Ctrl+Enter` |
| 送信済み入力を古い方 / 新しい方へ呼び出す(直近20件、起動中のみ) | `Up` / `Down` | `Up` / `Down` |
| 接続設定を開く | `Cmd+,` | `Ctrl+,` |
| 接続診断を表示・非表示 | `Cmd+Shift+D` | `Ctrl+Shift+D` |
| 接続診断をコピー | `Cmd+Shift+L` | `Ctrl+Shift+L` |
| 上側ログの選択テキストをコピー | `Cmd+C` | `Ctrl+C` |

Windows/Linux では Alt 単独で押して離すか F10 でメニューバーを表示します。

## IRC コマンド

入力欄の先頭を `/` にすると IRC コマンドを送れます。チャンネル名を省略すると選択中のチャンネルを使います。

| 入力例 | 動作 |
| --- | --- |
| `/join #other` | 別のチャンネルへ参加 |
| `/part` または `/part Leaving now` | 選択中のチャンネルから退出 |
| `/topic New topic` | 選択中のチャンネルのトピックを変更。`/topic` だけなら照会 |
| `/mode +o alice` | 選択中のチャンネルのモードを変更 |
| `/kick bob goodbye` | 選択中のチャンネルからユーザーをキック |
| `/invite bob` | 選択中のチャンネルにユーザーを招待 |
| `/names` | 選択中のチャンネルのメンバー一覧を要求 |
| `/me waves` | 選択中のチャンネルへ CTCP ACTION を送信 |
| `/msg :hello everyone` または `/notice :hello everyone` | 選択中のチャンネルへ発言または通知 |
| `/msg bob hello` または `/notice bob hello` | 指定した相手へ直接送信 |
| `/nick newname`、`/whois bob`、`/raw WHO #channel` | その他の IRC コマンドを送信 |

`/raw` と `/quote` は残りの行を IRC コマンドとして送ります。
