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

## GUI の E2E テスト（Linux のみ）

実際にビルドした `cayenchat` を仮想ディスプレイ上で起動し、ローカルの Ergo に
つないで、ユーザーと同じようにマウスで操作して確かめるテストです
（`scripts/e2e/history_gui.py`）。いまはチャンネル履歴（スクロールで古い履歴を
読み込む、再接続で取りこぼしを回収する）を確認します。所要時間は約 2 分です。

### 手でチャンネル履歴を試す（macOS・Linux）

自分の画面でクライアントを動かして確かめるための遊び場です。
`scripts/e2e/manual_history.py` がローカルの Ergo を起動し、`bob` にチャンネルを
埋めさせ、CayenChat との間に切断を起こせる中継を置きます。コマンドで bob に
発言させたり、接続を切ったりできます。ネットワークには出ません。

```sh
# 1. Ergo を用意（初回はビルドに少し時間がかかります）
ERGO_SETUP_ONLY=1 ERGO_NO_FAKELAG=1 scripts/ergo-metadata-interop.sh /tmp/cayenchat-ergo 36667
# 2. 遊び場を起動（このターミナルでコマンドを打ちます）
python3 scripts/e2e/manual_history.py --ergo-dir /tmp/cayenchat-ergo
# 3. 別のターミナルでクライアントを起動
cargo run --locked -p cayenchat-ui
```

クライアントの設定でサーバーを追加します：ホスト `127.0.0.1`、ポート `36668`
（中継。Ergo の 36667 ではありません）、TLS オフ、ニックネームは任意、チャンネル
`#demo`。IRCv3 タブで履歴のオプションをオンにします（batch・server-time・
message-tags はこれだけで要求されます）。普段の設定を汚したくなければ
`cargo run --locked -p cayenchat-ui --features test-build` で起動すると、毎回空の
設定で始まります（その場合サーバーは毎回追加します）。

遊び場のコマンド：`say テキスト`（bob が発言）、`fill 数`（番号付きの行をまとめて
発言）、`cut [秒]`（接続を切り、指定秒または `up` まで再接続を拒否）、`up`、
`delay 秒`（古いページの返答をその秒数だけ遅らせる。0 で解除）、
`sent`（クライアントが送った CHATHISTORY コマンドの一覧）、`quit`。

ローカルでは返答が一瞬で届くので、ページが届く瞬間を見たいときは遅らせます。
行数は `--lines` で増やせます（Ergo のメモリ上の履歴は 2048 行まで、クライアントが
保持するのは 2000 行まで）。例：
`python3 scripts/e2e/manual_history.py --ergo-dir /tmp/cayenchat-ergo --lines 1000 --page-delay 2`

試し方の例：

- **古い履歴の読み込み**：接続すると最新 50 行が出ます。ログを上へスクロール
  すると 50 行ずつ読み込まれ、`line 001` まで遡れます。読み込みのたびに表示中の
  行が動かないこと、先頭まで行ったらそれ以上要求しないこと（`sent` で確認）を
  見ます。7 行ごとに折り返す長い行があります。`--page-delay 2` にすると、上端で
  止まってから 2 秒後にページが入り、その瞬間に表示中の行が動かないかを確かめ
  やすくなります。遅延中にさらにスクロールしても、要求は 1 つだけです（`sent`）。
- **取りこぼしの回収**：`say A` → `cut 5` → `say B` → `say C` と打つと、5 秒後
  にクライアントが自動で再接続し、B と C が A の直後（再参加の行より前）に入り
  ます。その後 `say D` がライブで続きます。`sent` に
  `CHATHISTORY LATEST #demo msgid=… 50` が出ます。
- **回収しきれない場合**：`cut` の後に `fill 80` で 50 行を超えて発言してから
  `up` すると、回収は最新 50 行までで、その前に「Some messages sent while
  disconnected are not shown.」の行が入ります。
- **履歴オプションがオフの場合**：オプションを外して接続し直すと、履歴の要求も
  回収も行われません。

bob はサーバーの PING に即座に応えるので、放置しても切断されません。万一
切れた場合はその旨が表示され、次の `say`・`fill` で自動的に接続し直します。
理由は起動時に表示される Ergo のログ（`run/ergo.log`）で確かめられます。
終わったら `quit` で Ergo が止まります。Ergo の履歴はメモリ上だけなので、
遊び場を起動し直すと空から始まります。

### 何をどう確かめているか

アプリ本体にテスト用の仕組みは入れず、外から観測できるものだけで判定します。

- **通信**：アプリと Ergo の間に置いた中継（プロキシ）が、アプリの送った
  `CHATHISTORY` コマンドを記録します。中継は返答を一時的に止めたり、接続を
  切ったりもできます（予期しない切断の再現）。
- **画面のピクセル**：X サーバーから画面を取り込み、古いページを挿入する前と
  後でメインログの領域が 1 ピクセルも変わらないことを確かめます。
- **表示中の文字**：ログをドラッグで選択して Ctrl+C でコピーし、クリップボード
  から読み取って、どの行がどの順で表示されているかを確かめます。

各段階のスクリーンショットと、アプリ・Ergo のログが `--out` に残ります。

### 必要なもの

Linux 専用です（仮想ディスプレイと操作に X11 の道具を使うため）。OS の
パッケージは Cargo では宣言できないので、ここと `.github/workflows/ci.yml` の
`gui-e2e-linux` ジョブに同じ一覧を書いています。変えるときは両方を直してください。

| 用途 | Debian/Ubuntu のパッケージ |
| --- | --- |
| アプリのビルド | `libxcb1-dev` `libxkbcommon-dev` `libxkbcommon-x11-dev` |
| GPU なしでの描画（Vulkan のソフトウェア実装 lavapipe） | `mesa-vulkan-drivers`（`libvulkan1` も入ります） |
| 仮想ディスプレイ | `xvfb` |
| マウス・キー操作 | `xdotool` |
| 画面の取り込み（`xwd`） | `x11-apps` |
| クリップボードの読み取り | `xclip` |
| Ergo のビルド | Go 1.26 以上（古い Go しかない場合は下記） |

Python 3 は標準ライブラリだけを使います。

```sh
sudo apt-get install libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev \
  mesa-vulkan-drivers xvfb xdotool x11-apps xclip
```

### 実行する

```sh
cargo build --locked -p cayenchat-ui
ERGO_SETUP_ONLY=1 ERGO_NO_FAKELAG=1 scripts/ergo-metadata-interop.sh /tmp/cayenchat-ergo 36667
python3 scripts/e2e/history_gui.py --app target/debug/cayenchat \
  --ergo-dir /tmp/cayenchat-ergo --out /tmp/cayenchat-e2e
```

- 2 行目は固定バージョンの Ergo をビルドして設定だけ行います（起動はテストが
  行います）。`ERGO_NO_FAKELAG=1` は、テスト用の相手がチャンネルを素早く埋め
  られるように Ergo の連投制限を外します。
- インストール済みの Go が 1.26 より古いときは `GOTOOLCHAIN=go1.26.4` を
  付けると、Go が必要なツールチェーンを作業ディレクトリ内に取得します。
- 仮想ディスプレイは `:99` を使います。使用中なら `--display :98` などで変えます。
- 終わるとテストが Ergo と仮想ディスプレイを止めます。ネットワークには出ません
  （Ergo はループバックのみ）。

### 結果の見方

確認ごとに `ok: …` が出て、最後に `passed` が出れば成功です。失敗すると
`FAILED: …` の後に理由（期待した内容と実際の値）が出て、終了コードが 1 に
なります。`--out` の中身：

- `01_joined.png`、`02_page_requested.png` … 各段階の画面（番号順）。
  `*_selection.png` は文字を読み取るためにドラッグ選択した直後の画面です。
- `app.log`：アプリの標準出力・標準エラー。起動しない・すぐ落ちるときに見ます。
- `ergo.log`：Ergo のログ。

CI（`gui-e2e-linux` ジョブ）では成否にかかわらず、この一式が成果物
`gui-e2e-linux` としてアップロードされます。

### root 権限なしで動かす（パッケージを入れられない環境）

パッケージをシステムに入れずに、展開して使うこともできます（開発時に
コンテナで実際にこの方法で動かしました）。

```sh
mkdir -p ~/e2e-libs/debs && cd ~/e2e-libs/debs
apt-get download libvulkan1 mesa-vulkan-drivers xdotool libxdo3 x11-apps xclip libxmu6 \
  libxkbcommon-x11-0 libxcb-xkb1
for f in *.deb; do dpkg-deb -x "$f" ~/e2e-libs/root; done
R=~/e2e-libs/root
export PATH="$R/usr/bin:$PATH"
export LD_LIBRARY_PATH="$R/usr/lib/x86_64-linux-gnu"
export LIBRARY_PATH="$R/usr/lib/x86_64-linux-gnu"   # ビルド時のリンク用
export VK_ICD_FILENAMES="$R/usr/share/vulkan/icd.d/lvp_icd.json"
```

- `apt-get download` がパッケージ一覧の版と合わず 404 になるときは、
  `http://archive.ubuntu.com/ubuntu/pool/main/m/mesa/` などから同じ系列の
  新しい版の `.deb` を直接取得します。
- リンク用の `libxkbcommon.so`・`libxkbcommon-x11.so` がない（`-dev` がない）
  ときは、`$R/usr/lib/x86_64-linux-gnu` に `.so.0` へのシンボリックリンクを作ります。
- `ldd target/debug/cayenchat | grep "not found"` で足りないライブラリが
  分かります。

### テストを足すとき

- 窓の大きさは 1000×700 に固定しています。ピクセル比較の範囲（`LOG_BOX`）と
  操作する座標はこの大きさが前提です。
- 「表示が変わらないこと」はピクセル比較、「何がどの順で出ているか」はコピー
  した文字、「何を送ったか」は中継の記録で確かめるのが扱いやすいです。
- 画面が落ち着くのを待つには `Display.settle()`（連続した取り込みが一致する
  まで待つ）を使います。固定の待ち時間より安定します。
- 作ったテストが本当に失敗を検出できるかは、アプリを一時的にわざと壊して
  （例：ログ一覧を毎回リセットする）失敗することで確かめます。今のテストは
  この方法で 2 通り確認済みです。

### macOS・Windows について

中継・相手役のクライアント・判定の部分は OS に依存しませんが、画面の取り込み
と入力の再現は OS ごとに別の道具が必要です（macOS なら `screencapture` と合成
イベント、Windows なら UI Automation など）。ログイン済みのデスクトップ
セッションも必要です。まだ用意していません。

## うまくいかないとき

- `cargo-llvm-cov is missing`：上のインストールコマンドを実行します。PATH に
  なくても `~/.cargo/bin` は自動で探します。
- `LLVM tools are version X but rustc uses LLVM Y`：rustc の LLVM と同じ
  メジャーバージョンのツールを入れるか、`LLVM_COV`/`LLVM_PROFDATA` で指定します。
- 数値がおかしいとき：スクリプトは毎回、前回の計測データを消してから実行します。
  最初からビルドし直すには `target/llvm-cov-target` を削除します。
- E2E テストで `the chat window opened` が失敗する：アプリが起動していません。
  `--out/app.log` を見ます。`error while loading shared libraries` なら
  ライブラリ不足、Vulkan 関連のエラーなら `mesa-vulkan-drivers`（または
  `VK_ICD_FILENAMES`）を確認します。
- E2E テストで文字の確認が空（`[]`）になる：ドラッグ選択が効いていません。
  `*_selection.png` で選択範囲が青く表示されているかを見ます。
- Ergo のビルドで `go.mod requires go >= 1.26`：`GOTOOLCHAIN=go1.26.4` を付けます。
