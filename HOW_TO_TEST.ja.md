# テストの方法

[English](HOW_TO_TEST.md)

CayenChat のテストを実行し、テストカバレッジを自分で確認したい人向けの説明です。
開発者が守る手順は `spec/development.md`（Validation）にあります。

## テストを実行する

```sh
cargo test --workspace
```

テストは対象コードのそば（各ソースファイル末尾の `#[cfg(test)] mod tests`）と
`crates/*/tests/` にあります。ローカルの IRC サーバーが必要なテストは既定で
ignore されます（[相互運用テスト](#相互運用テスト)を参照）。

## カバレッジを計測して見る

### 最初に一度だけ

1. cargo-llvm-cov を `~/.cargo/bin` にインストールします。

   ```sh
   cargo install cargo-llvm-cov --locked
   ```

2. rustc と同じメジャーバージョンの LLVM ツールを用意します。rustc の
   バージョンは `rustc -vV` の `LLVM version` 行で確認できます。
   - rustup の Rust：`rustup component add llvm-tools`
   - Homebrew の Rust（rustup なし）：`brew install llvm`。keg-only なので
     PATH には何もリンクされません。スクリプトが自動で見つけ、バージョンの
     一致も確認します。
   - その他：`LLVM_COV` と `LLVM_PROFDATA` にツールのパスを設定します。

### 計測する

リポジトリのルートで実行します。

```sh
scripts/coverage.sh
```

ワークスペース全体を計測用にビルドし直して全テストを実行するため、初回は
数分かかります。結果は `target/` の下に書かれます（コミットはされません）。

| ファイル | 内容 |
| --- | --- |
| `target/coverage/tests.html` | 全テストの一覧：ファイル、行、結果、IRCv3 領域 |
| `target/llvm-cov/html/index.html` | ファイルごとに、どの行が実行されたか |
| `target/coverage/summary.json` | カバレッジの集計（JSON） |
| `target/coverage/test-run.log` | テスト実行の出力 |

ブラウザで開きます。macOS の例：

```sh
open target/coverage/tests.html
```

```sh
open target/llvm-cov/html/index.html
```

`python3 scripts/test-inventory.py` だけを実行すると、ビルドせずにソースから
IRCv3 領域ごとのテスト件数を表示します。

### `tests.html` の読み方（何が、どこでテストされているか）

- 冒頭：テストの総数、成功・ignore・失敗の件数、全体の行カバレッジ。
- **Per crate**：クレートごとのテスト件数と行カバレッジ。
- **IRCv3 areas**：CAP 交渉と SASL、message-tags / server-time / msgid、
  batch、metadata などを確かめるテストの件数と、書かれているファイル。
  領域はファイル名とテスト名から自動で振り分けているので目安として見て
  ください。ここでの ignore は実サーバーが必要なテストです。
- **Tests by file**：ファイルをクリックすると、テスト名・行番号・結果が
  出ます。「source」でそのファイルの行カバレッジに移ります。
- 入力欄でテスト名・ファイル・領域・結果を絞り込めます（`sasl`、`batch`、
  `ignored` など）。
- **Files without tests of their own**：テストを含まない 50 行以上の
  ファイルです。ほかのテストから実行されている場合もあるので、行カバレッジも
  確認してください。

テスト名は確かめている振る舞いをそのまま表しています（例：
`capability_names_are_case_sensitive`）。IRCv3 の要件を確かめるテストには、
その要件を仕様書から引用したコメントが付いています。

### 行カバレッジの読み方

- 一覧には、各ファイルで実行された関数・行・リージョン（分岐の間の区間）の
  割合が出ます。
- ファイル名をクリックするとソースが表示されます。赤い行はどのテストでも
  実行されなかった行、行の横の数字は実行回数です。
- まず見るとよい場所：`crates/irc-core/src/` の IRCv3 関連
  （`cap.rs`、`tags.rs`、`replay.rs`、`metadata.rs`）。

### カバレッジで分からないこと

- 実行された行が、正しさを確かめられた行とは限りません。仕様に沿っているかは、
  その要件を確かめるテストがあるかで判断します。100% 近いファイルでも規則の
  見落としはあり得ます。
- ソースファイル内に書かれたテストもそのファイルの行数に含まれるため、
  そうしたファイルの割合はやや高めに出ます。
- デスクトップ UI（GPUI）は画面を使わないテストでしか実行されないため、UI の
  ファイルは低めに出ます。描画・IME・各プラットフォームの挙動は手動で確認します
  （`spec/development.md` を参照）。

## 相互運用テスト

実際の IRC サーバーと通信するテストは、サーバーを指定しない限り ignore されます。
ループバック上に使い捨ての Ergo を起動します（指定したディレクトリに固定
バージョンをビルドします。Go が必要で、何もインストールされません）。

```sh
scripts/ergo-metadata-interop.sh /tmp/cayenchat-ergo 36667
```

別のターミナルで、ignore されたテストも含めて計測します。

```sh
CAYENCHAT_INTEROP_IRC=127.0.0.1:36667 scripts/coverage.sh -- --include-ignored
```

カバレッジなしで実行する場合：

```sh
CAYENCHAT_INTEROP_IRC=127.0.0.1:36667 cargo test -p cayenchat-irc-core -- --ignored --test-threads 1
```

終わったら Ctrl-C でサーバーを止め、ディレクトリを削除してください。

## うまくいかないとき

- `cargo-llvm-cov is missing`：上のインストールコマンドを実行します。PATH に
  なくても `~/.cargo/bin` は自動で探します。
- `LLVM tools are version X but rustc uses LLVM Y`：rustc の LLVM と同じ
  メジャーバージョンのツールを入れるか、`LLVM_COV`/`LLVM_PROFDATA` で指定します。
- 数値がおかしいとき：スクリプトは毎回、前回の計測データを消してから実行します。
  最初からビルドし直すには `target/llvm-cov-target` を削除します。
