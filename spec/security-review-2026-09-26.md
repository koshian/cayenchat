# セキュリティレビュー 2026-09-26 — 軽微な指摘の検討メモ

中程度の指摘のうち 1（認証拒否後の自動再接続）、3（CI トークン権限と Actions 固定）、
4（悪意あるサーバーによるメモリ枯渇）は対応済みです。以下は検討が必要な項目です。

各項目の「対応方針」「確認事項・メモ」「判断」欄に記入してください。
判断は `採用` / `見送り` / `保留` / `受容（リスクを承知で維持）` のいずれかを想定しています。

---

## M2. 証明書検証オフの TLS でも資格情報を送信する（中程度・維持済み）

- **場所**: `crates/irc-core/src/lib.rs` `ConnectionConfig::validate`
- **内容**: `verify_tls_certificates=false` でも PASS / SASL PLAIN を送る。経路上の攻撃者（MITM）がいれば平文と同等。
- **現状の判断**: 自己署名証明書で暗号化だけしている IRC プロキシに接続する用途があるため、**維持**。
- **将来の選択肢**:
  - サーバーごとに証明書のフィンガープリントを固定する（TOFU）。検証を無効にせず自己署名証明書を受け入れられる。
  - 検証オフで資格情報を送るときに、接続ごとに一度だけ警告を出す。

**対応方針**:

**確認事項・メモ**:

**判断**: 受容

---

## L1. パスワード欄からコピー・カットできる

- **場所**: `crates/ui/src/input.rs` `TextInput::copy` / `TextInput::cut`
- **内容**: `secret` の欄でも選択範囲の平文がクリップボードに入る。クリップボード履歴アプリや他のアプリから読める。
- **案**:
  - `secret` のときは copy / cut を何もしないか、選択範囲を削除するだけにする（macOS 標準のパスワード欄と同じ挙動）。
  - あわせて `"*".repeat(content.len())` がバイト数ぶん伏せ字を並べる点も直す。日本語など多バイト文字では文字数と一致せず、長さの推測にも使われる。表示用の伏せ字の数と編集オフセットの対応付けが必要になる。
- **確認したいこと**: 伏せ字の数を固定長（例: 8 個）にしてよいか。IME（`text_for_range`）に平文が見えている点も一緒に塞ぐか。

**対応方針**:

**確認事項・メモ**:

**判断**:

---

## L2. 設定ファイルの書き込みが原子的でない

- **場所**: `crates/storage/src/lib.rs` `save_to`
- **内容**: truncate してから書き込むため、途中でクラッシュや電源断が起きると `settings.json` が空か壊れた状態になる。次回起動時に解析エラーになる。
- **案**: 同じディレクトリに一時ファイルを 0600 で作って書き込み、`sync_all` してから `rename` で置き換える。Windows では `rename` による上書きが `MoveFileEx(REPLACE_EXISTING)` 相当になるか確認が要る（std の `fs::rename` は上書きする）。
- **確認したいこと**: 読み込めない設定を見つけたとき、現在はデフォルトで上書きしているか。していればバックアップを残すべきか。Windows で ACL を明示的に絞る必要があるか（`%APPDATA%` は通常ユーザー専用）。

**対応方針**:

**確認事項・メモ**:

**判断**:

---

## L3. パスワードを平文で保存している

- **場所**: `crates/storage/src/lib.rs` `ServerProfile::{server_password, sasl_password}`
- **内容**: 明示的な確認のうえでの平文保存。ファイルの権限は 0600 だが、バックアップや同期ツール、同じユーザーで動く他のプロセスからは読める。
- **案**: OS の資格情報ストア（macOS Keychain / Windows Credential Manager / Linux Secret Service）に保存する。`keyring` クレートが候補。既存の平文値を移行してファイルから消す手順が必要。
- **確認したいこと**: Linux で Secret Service が無い環境のフォールバックをどうするか。ad hoc 署名の macOS ビルドで Keychain のアクセス許可ダイアログが毎回出ないか。

**対応方針**:

**確認事項・メモ**:

**判断**:

---

## L4. IRC 装飾コードや Unicode の双方向制御文字で表示を偽装できる

- **場所**: ログ表示全般（`crates/ui/src/main.rs` の `styled_log_text` など）、メンバー一覧、WHOIS 窓
- **内容**: `\x02` `\x03` などの IRC 装飾コードや、U+202E などの双方向制御文字を除去していない。ニックネームや URL の見た目を偽装できる。ダブルクリックで開く URL 自体は正しい。
- **案**:
  - 表示前に C0 制御文字と双方向制御文字（U+202A–202E、U+2066–2069）を除去するか、可視の記号に置き換える。
  - IRC 装飾コードは除去するか、将来は解釈して色付けする（LimeChat 相当）。
- **確認したいこと**: 装飾コードを解釈して表示する機能を将来入れるか（入れるなら、今は除去だけにしておく）。クリップボードへのコピーでは元の文字列を残すか。

**対応方針**:

**確認事項・メモ**:

**判断**:

---

## L5. `Localizer::format` の置換が連鎖する

- **場所**: `crates/ui/src/localization.rs` `Localizer::format`
- **内容**: プレースホルダーを順番に `replace` するため、サーバー由来の値（例: ニックネームや理由文が `{reason}`）が後の置換で展開される。見た目の問題だけ。
- **案**: テンプレートを 1 回だけ走査し、`{name}` を見つけたら値に置き換えて、値の中身は再解釈しない。

**対応方針**:

**確認事項・メモ**:

**判断**:

---

## L6. 通信ログで伏せ字になる対象が限られる

- **場所**: `crates/irc-core/src/lib.rs` `redacted_wire_line` / `is_service_secret`
- **内容**: NickServ / ChanServ と PASS / AUTHENTICATE / OPER だけが対象。QuakeNet の `PRIVMSG Q@CServe.quakenet.org :AUTH user pass`、Undernet の `X@channels.undernet.org :login`、`/quote AUTH ...` などは平文のまま通信ログに残り、クリップボードへの書き出しにも含まれる。
- **案**: 対象サービス（`Q`、`X`、`AuthServ` など）と命令（`AUTH`、`LOGIN`）を追加する。または、ユーザーが追加できる伏せ字パターンの設定を設ける。
- **確認したいこと**: よく使うネットワーク（IRCnet など）で使っているサービス名と認証コマンド。

**対応方針**:

**確認事項・メモ**:

**判断**:

---

## L7. 依存関係と CI の衛生

- **内容**:
  - `encoding 0.2`（`irc` の `encoding` 機能経由）にはメンテナンスされていない旨の勧告（RUSTSEC-2021-0153）が出ている。
  - CI で `cargo audit` / `cargo deny` を実行していない。
  - `ci.yml` の `cargo build` / `cargo test` に `--locked` が付いていない（リリースのワークフローには付いている）。
  - `cargo install cargo-deb --locked` のバージョンを固定していない。
- **案**: CI に `cargo deny check advisories` か `cargo audit` のジョブを足す。`--locked` を付ける。`cargo-deb` を `--version` で固定する。
- **確認したいこと**: `encoding` を置き換えるには `irc` 側の対応が要る（`encoding_rs` への移行は上流の issue を確認）。勧告を無視リストに入れるか。

**対応方針**:

**確認事項・メモ**:

**判断**:

---

## 参考: 対応済みの項目

| 項目 | コミット |
| --- | --- |
| 1. 認証拒否（SASL 失敗、464/465、UTF8ONLY 不一致）の後に自動再接続しない | `Stop automatic reconnect after authentication refusal` |
| 4. 参加中のチャンネルだけ会話を作成し、会話数を 1 ネットワーク 1,000 件に制限 | `Create conversations only for joined channels` |
| 4. 未完了の WHOIS 応答とロスターキャッシュに上限を設定 | `Bound unfinished WHOIS replies and roster cache` |
| 3. ワークフロートークンの権限を最小化し、Actions を SHA で固定、Dependabot を追加 | `Restrict workflow token permissions and pin actions` |
