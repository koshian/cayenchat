# CayenChat

[English README](README.md)

Rust と GPUI で作った、macOS・Windows・Linux 向けのコンパクトなネイティブ IRC クライアントです。

- 複数サーバーへの同時接続（サーバーごとにニックネーム、自動参加チャンネル、TLS、SASL を設定）
- UTF-8、ISO-2022-JP、Shift_JIS、EUC-JP に対応
- パスワードは OS の資格情報ストアに保存
- 自分の画像ホスティングアカウント（ImgBB）経由で画像を共有
- 日本語・英語 UI

チャット履歴は保存しません。

## ビルドと起動

Rust 1.88 以降が必要です。

```sh
cargo run --locked -p cayenchat-ui
```

macOS では開発用の `.app` バンドルも作れます。

```sh
sh scripts/bundle-macos.sh
open target/CayenChat.app
```

プラットフォームごとの前提条件と確認手順は[開発要件](spec/development.md)を参照してください。

## 使い方

起動時に開く設定ウィンドウ（`Cmd+,` / `Ctrl+,`）でサーバーを追加し、**接続**を押します。[キーボードショートカットと IRC コマンド](SHORTCUTS.ja.md)も参照してください。

## UI 文言のカスタマイズ

CayenChat は、同梱の `locales/ja.json` と `locales/en.json` を実行時に読み込みます。これらのファイルを編集すると、UI の文言をローカルで変更（上書き）できます。外部ファイルが見つからない場合は、実行ファイルに埋め込まれたカタログを使います。

## ドキュメント

[`spec/`](spec/) 以下の仕様書が基準です。まず[アーキテクチャ](spec/architecture.md)と[機能の実装状況](spec/feature-parity.md)を参照してください。テストの実行とカバレッジの確認は[テストの方法](HOW_TO_TEST.ja.md)を参照してください。

## ライセンス

GPL バージョン 3 のみ（[LICENSE](LICENSE)）。GPUI 由来の `crates/ui/src/input.rs` は Apache-2.0 のままで、外部依存にはそれぞれのライセンスが適用されます（[告知](THIRD_PARTY_NOTICES.md)）。
