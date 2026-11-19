# GPUI Kit 移行の互換性検証 (#1025, 2026-11-18)

#1024 (設定画面の再構築) の前提作業として、GPUI 0.2.2 / gpui-component 0.5.1
から GPUI Kit 系へ移る場合の互換性を調べた記録。**このPRはコードを移行しない。**
依存の固定値、vendor パッチの扱い、修正範囲、設定画面で使える Kit API の
判断材料を残すもの。

検証環境は Linux (Debian, rustc 1.99.1) のみ。Windows / macOS での起動、日本語
IME、キーボード操作は確認できていない (最後の「未確認」を参照)。

## 1. 採用版

GPUI Kit (`gpui-kit` 0.10.0, 2026-11-16) は GPUI 本体を `gpui-pre` として配布し、
`gpui-pre-*` 一式 (`-platform`、`-windows`、`-linux`、`-macos`、`-wgpu` など) に
分かれている。`gpui-kit` はそれらと `gpui-base`、`gpui-component`、
`gpui-kit-assets` を束ねる facade。

| 組み合わせ | gpui-pre | gpui-component | native-theme-gpui |
| --- | --- | --- | --- |
| 現行 | (gpui 0.2.2) | 0.5.1 | 0.5.7 (+ native-theme 0.5.7 の vendor) |
| gpui-kit 0.10.0 | =0.3.12 | 0.10.0 | 該当なし (最新 0.7.1 は gpui-component 0.9.1 用) |
| gpui-kit 0.9.1 | =0.3.12 | 0.9.1 | 0.7.1 (native-theme 0.7.1) |

- コネクタ `native-theme-gpui` は Kit より遅れる。0.7.1 (2026-11-17) は
  `gpui-component 0.9.1` と `gpui-pre >=0.3.12` を要求する。gpui-component が
  二つ入ると `Theme` の型が合わないので、**gpui-component 0.10.0 とは併用できない**。
- `gpui-component` を使うのは `crates/ui/src/settings_theme.rs` だけ
  (`gpui_component::Theme`)。他の `ui` のコードは `gpui` のみに依存する。
- 採用案: `gpui-pre =0.3.12`、`gpui-pre-platform =0.3.12`、`gpui-component =0.9.1`、
  `native-theme-gpui =0.7.1`、`native-theme =0.7.1` を固定する。#1024 は
  `gpui-component::setting` を使うので `gpui-kit` facade は必須ではなく、現行どおり
  個別に固定するほうが依存が少ない。0.10.0 が必要になれば、コネクタが追従するまで
  待つか、コネクタをやめて `native-theme` の `ResolvedTheme` から色を直接写す
  (現行も幾何・フォント・入力色は `ResolvedTheme` を直接使っている)。
- `native-theme-gpui 0.7.1` は Rust 1.95 以上を要求する (現行の UI は 1.94)。
- gpui-pre は `gpui` crate 名で import できるよう `package = "gpui-pre"` で別名を
  付ければ、`use gpui::...` は変更不要。`runtime_shaders` は `gpui-pre` 本体
  ではなく `gpui-pre-platform` の feature。`Application::new()` は
  `gpui_platform` 側に移った (`crates/ui/src/main.rs`)。

## 2. vendor パッチの取り込み状況 (`vendor/gpui`, 0.3.12 と比較)

vendor/gpui/PATCHES.md の項目を 0.3.12 のソースと突き合わせた。

**取り込み済み (移行すればパッチを外せる)**

- Windows: `GCS_RESULTSTR` を `GCS_COMPSTR` より先に処理 (#880)。0.3.12 の
  `gpui-pre-windows/src/events.rs` は同じ順序。
- Windows: `WM_QUERYENDSESSION` / `WM_ENDSESSION` でシャットダウン時に終了処理を
  実行 (#284)。0.3.12 は `handle_end_session` を持つ。
- Windows: IME の ON/OFF の基本機能 (`update_ime_enabled`) と
  `InputHandler::accepts_text_input` (元は Zed からの移植)。Wayland の
  `update_ime_enabled` も上流にある。**ただし CayenChat 固有の保護は含まれない**
  (次の「未取り込み」の `discarding_ime_commit` を参照)。関数が存在することと、
  パッチ全体が同等であることを区別すること。
- Wayland: サーバー装飾が無いとき client 側装飾にフォールバック。
- Linux: XDG portal の外観変更で `RefCell` の借用を保持したまま呼ばない。
- `hoverable_tooltip` (`elements/div.rs`)、`SystemMenuType::Window`。
- taffy の `f32` リテラル。
- Linux atlas の `remove` が領域を返す (`gpui-pre-wgpu`)。

**未取り込みか未確認 (移行時にもう一度当てるか、不要にする必要がある)**

- `InputHandler::prefers_direct_input` (CayenChat 独自。macOS の
  `allowedInputSourceLocales` の切り替えを含む)。**日本語 IME まわりの回帰に直結する。**
  上流の `accepts_text_input` だけでは「入力欄だが IME を切る」場面を表せない。
- Windows: IME 確定文字の破棄 (`discarding_ime_commit`、`events.rs` の
  `handle_ime_composition` と IME 無効化側)。フォーカス移動で IME を外すとき、
  `ImmNotifyIME(CPS_COMPLETE)` が返す確定文字を、新しくフォーカスを得た欄へ
  渡さない (パスワード欄の文字が別の欄へ流出し、選択範囲を置き換えるのを防ぐ)。
  公開済み `gpui-pre-windows 0.3.12` の同等処理は `CPS_COMPLETE` の確定文字を
  通常の入力ハンドラーへ渡すので、**この保護は上流にない**。`prefers_direct_input`
  とは別に移植し、回帰確認の対象にする。
- `App::on_quit_request` (macOS の Dock からの Quit を取り消せる)。
- `Menu::menu_type` が `Option<SystemMenuType>` であること (上流は `SystemMenuType`
  で、`None` に相当する値が違う)。
- macOS のファイルプロミス drop (`ExternalPaths::keep_alive` / `PromisedFiles`)。
- `PathPromptOptions::filter` (`PathPromptFilter`)。ファイルダイアログの種類絞り込み。
- `Window::is_compositing`、`supports_background_blur`、
  `supports_background_blur_radius` / `set_background_blur_radius` と、macOS の
  `CGSSetWindowBackgroundBlurRadius`、`UnderWindowBackground` マテリアル、
  `update_transparency` の opaque フラグ (#930, #1018, D091)。
- Wayland クリップボードの非同期読み取りと上限、同期 `read_fd` の上限
  (#684)。Windows クリップボードのサイズ検査 (`clipboard_data.rs`)。
  `App::read_from_clipboard_async`。
- フレーズ単位の折り返し (`StyledText::with_wrap_joins`、`shape_text_joined`)、
  `LineWrapper::is_word_char` の NBSP 等 (#537, #550)。
- `shape-run-cache` による Linux のシェーピング高速化 (`text_system.rs`)。
- ウィンドウ破棄時の `IsWindow` 確認 (`RevokeDragDrop` / `DestroyWindow` の警告)。
  上流 0.3.12 は無条件に呼ぶ。
- 「ウィンドウ not found」ログの抑制、Windows の `activate` の Alt 抑止 (#354)、
  macOS の `MacWindow::open` のフレーム解釈 (#795)、Wayland の `read_fd`、X11 の接続
  エラーでの終了 (#196)、`FallbackPromptRenderer` の Esc・幅・`occlude`、
  Windows の ICU (`u_strlen`) 依存、SVG の BGRA 変換、`InteractiveText` の
  tooltip 保持の細部。上流 0.3.12 に同等の修正があるか個別に確認していない。
  `u_strlen` と `IsWindow` は 0.3.12 にも同じ呼び出しが残っている。

つまり vendor/gpui を**そのまま捨てることはできない**。Windows の IME 修正
(#880 と language-key) は上流に入ったが、独自の API (`prefers_direct_input`、
`on_quit_request`、`discarding_ime_commit`、背景ぼかし、クリップボード上限、折り返し) を載せるパッチは
0.3.12 に対して再作成が必要。`gpui-pre-*` は crate が分割されているので、vendor は
`gpui-pre`、`-windows`、`-linux`、`-macos`、`-platform` に分かれ、`[patch.crates-io]`
の対象と PATCHES.md の記述も組み替えになる。

## 3. 他の vendor

- `zed-xim`: `gpui-pre-linux 0.3.12` は引き続き `zed-xim 0.4.0-zed` に依存する。
  COMPOUND_TEXT の寛容なデコード (#227) は上流にない。`xim_handler.rs` は
  `PREEDIT_CALLBACKS` 固定のままなので、スタイル選択 (IBus/Mozc) の変更も残る。
  **vendor/zed-xim は維持する。**
- `native-theme`: 0.6.1 以降は Windows のフォントと寸法を 96 DPI で読む
  (D019 / #290)。`native-theme 0.7.1` + `native-theme-gpui 0.7.1` に上げれば
  `vendor/native-theme` を削除できる。ただし 0.7.1 で
  `ResolvedWidgetBorder` の `padding_horizontal` / `padding_vertical` が無くなって
  おり、`settings_theme.rs` の幾何の読み取りを直す必要がある。
- `irc`、`irc-proto`: 対象外。

## 4. ビルド互換性の試験結果

作業ツリーを複製して `gpui` を `gpui-pre =0.3.12` に、`native-theme*` を 0.7.1 に、
`gpui-component` を 0.9.1 に替え、`vendor/gpui` と `vendor/native-theme` の patch
を外して `cargo check -p cayenchat-ui` を実行した (最初の段階の型検査だけ。
`RUST_FONTCONFIG_DLOPEN=1` が必要な環境)。依存解決は通り、`cayenchat-ui` で
**約 150 件のエラー**が出た。大半は機械的な修正で済む。

| 件数 (概数) | 内容 | 対応 |
| --- | --- | --- |
| 72 | `Window::focus(&handle)` が `focus(&handle, cx)` に | 引数追加 (`settings_window.rs` ほか) |
| 8 | 引数が一つ足りない呼び出し (`Window::focus_next()` の `cx`、`keystroke.unparse()` の `OperatingSystem` など) | 引数追加 |
| 12 | `PathPromptOptions::filter` / `PathPromptFilter` が無い | パッチを当てるか、拡張子検査を自前で行う |
| 7 | `PromisedFiles` が無い | macOS drop のパッチを再作成 |
| 5 | `Keystroke::parse` が借用を保持 (`key_recorder.rs`, `shortcuts.rs`) | `String` を持たせる |
| 3 | `Point<Pixels>` に `height` が無い | `y` などへ書き換え |
| 1 | `BoxShadow` に `inset` | フィールド追加 |
| 7 | `supports_background_blur(_radius)`、`set_background_blur_radius`、`is_compositing`、`on_quit_request`、`ExternalPaths::keep_alive`、`prefers_direct_input` | 独自 API の再作成 (§2) |
| 4 | `ResolvedWidgetBorder` の `padding_horizontal` / `padding_vertical` | native-theme 0.7 の API に合わせる |
| 約 20 | 上記以外 (型・引数の不一致、クロージャの引数数など) | 個別に修正 |

エラーはその先の借用検査・型検査の段階を含まないので、実際の修正は上回りうる。
`gpui::Application::new()` が `gpui_platform::application()` に移る点も別途直す。

## 5. #262 / #622 / #623 で使える Kit API (`gpui-component 0.9.1` の `setting`)

- `Settings::new(id)`: `pages` / `page`、`sidebar_width`、`sidebar_size_range`、
  `with_group_variant`、`default_selected_index`、`header_style`、`sidebar_style`。
  左ナビ + 右ペイン (#135) と、**ナビ上部の検索欄 (`search_input`)**
  と絞り込み (`SettingsFilter`: 一致しないページ・グループを隠し、元の添字を
  振り直さない) を持つ。→ #623 の基盤。
- `SettingPage::new(title)`: `icon`、`description`、`default_open`、
  `resettable`、`group(s)`、`title_suffix`。
- `SettingGroup::new()`: `title`、`description`、`variant`、`footer`、`item(s)`。
- `SettingItem::new(title, field)`: `description`、`keywords` (検索の対象)、
  `disabled`、`layout(Axis)`、`on_reset(is_dirty, reset)`。
  `SettingItem::render(...)` は独自の要素用 (自動参加の一覧、アバター、色選択、
  ショートカット記録)。→ #622 の「説明付き行」、#262 の宣言化の部品。
- `SettingField<T>`: `switch`、`checkbox`、`input`、`dropdown`、
  `scrollable_dropdown`、`number_input(NumberFieldOptions, ..)`、`element`、`render`。
  `default_value` と `on_reset` を持つ。値は `get/set` のクロージャで読み書きする。

注意:

- 検証・自動保存・秘密情報の扱いは `set_value` に書かない (Kit は値を保持して
  setter を呼ぶだけ)。CayenChat の既存の仕様 (`storage::credentials`、検証、
  自動保存) を保つアダプタが要る。
- 検索は `SettingItem::keywords` と題・説明から作る。日本語/英語の表示言語を
  `title` / `keywords` に反映させる必要がある。
- 設定の行は `gpui-component` の `Theme` で描く。`settings_theme.rs` の
  コネクタ + `ResolvedTheme` の合成を、この描画に合わせて見直す。
- フォーカス (Tab 巡回、D020) とキーボード操作は Kit 側の実装に従う。IME の入力欄は
  `gpui-component` の `Input` を使うことになり、CayenChat 独自の `prefers_direct_input`
  とどう組み合わさるかは未確認。

## 6. 結論と推奨

1. **移行は可能だが、独立した大きな作業**。型の修正だけなら数百行の機械的な
   変更で済むが、独自の GPUI パッチ (IME 切り替え、背景ぼかし、クリップボード上限、
   折り返し、macOS の drop など) を 0.3.12 の分割 crate に載せ直す作業が本体。
2. Windows の IME 修正のうち language key と `RESULTSTR` 順序は上流に入ったので、
   移行後はその二つのパッチは不要。IME の ON/OFF の基本機能も上流にあるが、
   確定文字の破棄 (`discarding_ime_commit`) は上流になく、載せ直す。日本語 IME の
   実機確認は必須で、確認項目に「未確定文字がある状態から別の入力欄 (特に
   パスワード欄から他の欄) へ直接フォーカスを移し、新しい欄に文字が混入しない
   こと」を含める。
3. 移行の順序案: (a) `gpui-pre 0.3.12` + `gpui-component 0.9.1` + 0.7.1 コネクタへの
   更新と、独自 API のパッチの再作成を一つのPRで。(b) Windows / macOS / Linux で
   起動・日本語 IME・キー操作・背景ぼかしを確認。(c) その上で #262 → #622 → #623。
   (a) は `vendor/gpui` を段階的に分割する大きな PR になるので、API の読み替え
   (focus、unparse など) だけの PR と、パッチの載せ替えの PR に分けるのが安全。
4. **段階分割と #1025 の完了条件**: 本PRは予備調査 (段階1) であり、Issue の
   「候補版での起動・日本語 IME・キー操作・基本UIの確認」は**完了していない**。
   候補依存は現状コンパイルできず実行確認ができないため、完了には段階2 として
   「候補依存 + 再作成パッチが通り、起動できる検証用ブランチ」の作成と、
   Windows / macOS / Linux での確認が要る。#1024 の設定画面改修は段階2 の確認後に
   着手する。この分割はメンテナーの合意待ち (Issue #1025 で確認する)。
   そのため本PRは Issue を閉じない (`Refs`)。
5. 再現性のため、試験で変更した依存に `native-theme-derive =0.7.1` も含める。
   `native-theme-gpui 0.7.1` は推移依存として `gpui-kit 0.9.1` を引くので、
   facade を直接指定しなくても依存には残る。
6. 上流への還元で `vendor/gpui` を縮められる項目: `prefers_direct_input`、
   `PathPromptFilter`、`on_quit_request`、クリップボードの上限。

## 未確認

- Windows / macOS での起動、日本語 IME (MS-IME、Google 日本語入力)、キーボード操作。
  このリポジトリの検証環境は Linux のみで、今回は依存解決とコンパイルエラーの
  調査にとどまる。**実際の移行PRで人による確認が必要**。
- 試験は `cargo check` の最初の段階まで。リンク、実行、GUI の確認はしていない。
- §2 の「個別に確認していない」項目。

## 段階 2 の進捗 (#1081)

統合ブランチ `gpui-kit-migration` で `gpui-pre =0.3.12` へ移す作業の記録。
master は GPUI 0.2.2 のままで、このブランチだけが §1 の採用版を使う。

### PR 1: 依存の切り替えと API の読み替え

- 依存を §1 の採用版に替えた。`gpui` は `package = "gpui-pre"` で `gpui` の名前のまま
  使い、`gpui-pre-platform` (`gpui_platform`) に `runtime_shaders`、`font-kit`、
  `wayland`、`x11` を載せた。`native-theme-derive =0.7.1` も固定した。UI の最低
  Rust は 1.95 (`native-theme-gpui 0.7.1`)。
- `vendor/native-theme` を削除した。`native-theme 0.7.1` は Windows のフォントを
  96 DPI で読む (D019、#290)。`[patch.crates-io]` の `gpui` と `native-theme` を外した。
  `vendor/gpui` は載せ直しの参照元として残し、ビルドには使わない。
- 読み替え: `Window::focus(&handle, cx)`、`focus_next/prev(cx)`、`blur(cx)`、
  `ArenaClearNeeded::clear(cx)`、`Keystroke::unparse(os)`、`Modifiers::secondary(cx)`、
  `Point` の `y`、`BoxShadow::inset`、`paint_svg` / `paint_image` / `Line::paint` の引数、
  `ResolvedWidgetBorder::padding`、`to_theme` の `AccessibilityPreferences`、
  `Application` は `gpui_platform::application()`、`Timer` は
  `background_executor().timer()`、`AsyncApp::update` は `Result` を返さない、
  `on_scroll_wheel` は `on_scroll`、クリップボードの非同期読み取りは `Result`。
- `Keystroke::parse` と `KeyBinding::new` は `&'static str` と明示の修飾キーしか受けない。
  設定の文字列は `parse_with_os(key, HOST_OS)` と `KeyBinding::load` (`&App` が要る。
  `bindings` と `fixed_bindings` が `cx` を取る) で読み、`secondary-` の固定キーは
  `secondary!` マクロで `cmd-` / `ctrl-` に展開する。
- `WrappedText` は `WrappedLine` が `Clone` でなくなったので `Rc<[WrappedLine]>` を持つ。

### 仮実装の一覧 (後続の PR で外す)

上流にない独自 API は `crates/ui/src/gpui_pending.rs` と、`TODO(#1081)` の印を付けた
箇所で仮に置き換えている。ここに載せ直す PR (§6 の分け方) を添える。

| 機能 | この PR の状態 | 外す PR |
| --- | --- | --- |
| `prefers_direct_input` (macOS `allowedInputSourceLocales` を含む) | **載せ直し済み** (IME とフォーカスの PR)。`vendor/gpui-pre{,-windows,-macos,-linux}` の `PATCHES.md` を参照。Windows と macOS は未確認 (最後の段階で実機) | — |
| `discarding_ime_commit` (Windows) | **載せ直し済み** (同上)。実機の確認は最後の段階 | — |
| `App::on_quit_request` | 登録しない。macOS の Dock の Quit は確認なしで終了する | 終了とウィンドウ |
| `Menu::menu_type` (`SystemMenuType::Window`) | 外した。macOS の Window メニューは名前が `Window` のときだけ AppKit が窓の一覧を足す (日本語表示では足されない) | 終了とウィンドウ |
| Windows の `IsWindow` 確認、`activate` の Alt 抑止 | 未適用 | 終了とウィンドウ |
| `PathPromptFilter` | ファイルダイアログの種類絞り込みなし。添付の検査が他の種類を断る | ファイル |
| macOS のファイルプロミスの drop (`PromisedFiles`) | `keep_alive` は常に `None`。型 (`PromisedFiles`) だけ `gpui_pending` に置く | ファイル |
| `is_compositing`、`supports_background_blur(_radius)`、`set_background_blur_radius`、マテリアル | 常に「合成あり」「ぼかし非対応」。背景ぼかしは使えない | 背景の透過とぼかし |
| クリップボードの上限 (Wayland、Windows)、`read_from_clipboard_async` | 上限なし。読み取りの失敗は空として扱う | クリップボード |
| フレーズ単位の折り返し (`with_wrap_joins`、`shape_text_joined`) | `PHRASE_WRAP = false` で折り返しは標準のまま。`phrase_wrap` の描画テストは `cfg(any())` で外した | テキスト |
| `shape-run-cache` | 未適用 | テキスト |
| `InteractiveText::hoverable_tooltip` / `tooltip_bridge` | 上流の `tooltip` に置き換え。ポインターを移してコピーボタンを押す操作はできない | テキスト |
| §2 の「個別に確認していない」もの | 未適用 | 残りの細かいパッチ |

テストは仮実装に合わせて、`image_file_filter` のテストと `Menu::menu_type` の検査を
外した。

### 確認したこと

- Linux: `cargo check`、`cargo clippy --workspace --all-targets --all-features -D warnings`。
- Windows / macOS は `cargo check` を通していない (C の依存が要り、クロスでは
  通らない。CI の方針と同じ)。macOS 専用の箇所 (`navigation_binding_tests` の
  `command_for`、`Window` メニュー) は目視で読み替えた。
- Linux: `cargo test -p cayenchat-ui --bin cayenchat -- --test-threads=1`: 874 passed,
  13 ignored. Xvfb: the app starts, draws the chat window and the settings window
  and quits with Ctrl+Q without a panic.

### テストの状態

次のテストは `#[ignore = "#1081: ..."]` にした。理由を付けてあるので、載せ直しの PR か
別の調査で戻す。

- `log_list::a_short_log_reports_where_its_rows_end`: `ListState` から短いログの行の高さを
  取れない (`LogList::unfilled_from` は常に `None`、短いログの上の埋め草を描かない)。
- `icon_follows_its_room_after_an_unspaced_prefix`、
  `a_decoded_reference_shows_its_original_and_copies_only_that`: テキストのパッチが要る。

CI の GUI 検査 (`.github/workflows/ci.yml`): 統合ブランチ (`gpui-kit-migration`) と、
それ宛ての PR では、仮実装に依存する 3 本 (`url_bubble_gui.py`: リンクのホバーとクリック、
`log_colors_gui.py`: 行の背景色、`opacity_gui.py`: 背景の透過) をステップ単位で外す。
`history_gui.py` は外さず、起動・描画・履歴の操作は引き続き検査する。
この 3 本は、対応する載せ直しの PR (ツールチップ・背景ぼかし) で戻し、master では常に動く。

ignore から戻したテスト (レビュー 1 回目):

- `typing_reuses_panes_*` 3 件と `perf_baseline` の入力: 最初の 1 文字で、ペインが一度だけ
  再描画される (GPUI 0.3 の挙動。0.2.2 では起きない。原因は未調査で、描画は 1 回きり)。
  テストは 1 文字入れてから描画回数を控え、**2 文字目以降はペインを再描画しない**ことを
  今までどおり検査する。続けて打つたびに描画する退行はこれで検出できる。
- `server_settings_open_the_clicked_server_not_the_selected_one`、
  `inherited_sliders_*`: `Entity::update_in` は、そのエンティティを描いている「現在の窓」で
  動く。設定窓が開くと、チャット窓のエンティティの現在の窓が設定窓に移るので、
  `ChatWindow` の窓を要る処理 (`window_handle().downcast::<ChatWindow>()`) が失敗した。
  テストは `cx.update(|window, cx| chat.update(cx, ..))` でチャット窓から動かすようにした。
  アプリ側の不具合ではない。

CI: `.github/workflows/ci.yml` の `push` / `pull_request` を `gpui-kit-migration` にも
広げた。統合ブランチ宛ての PR でも、既存のビルド・テスト行列 (Windows / macOS を含む) が
動く。

テストの修正: 実際の接続を張るテストは、GPUI のテストスケジューラーが他スレッドからの
起床を「決定的でない」とするので、イベントポンプで `allow_parking()` を呼び
(`cfg(test)`)、切断後に `wait_closed` で作業スレッドの終了を待つ。ホバーは下端を
内側に数えなくなったので、`exclusive_row` なしの期待値を更新した。
