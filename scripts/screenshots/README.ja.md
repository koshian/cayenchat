# スクリーンショット再撮影ガイド

[English](README.md)

UIの変更後、過去のIssueを探さずにLinux・macOS・Windowsの画面を撮り直すための手順です。**コマンドはリポジトリのルートで実行**します。

`demo.py`は現在のソースから`demo`機能付きのアプリをビルドし、127.0.0.1上のデモ専用IRCサーバー、架空の会話、生成した画像を使います。実際のIRCサーバーやユーザーの設定・保存パスワードにはアクセスしません。**デモビルドを配布しないでください。**

古い`--app`指定のバイナリは現在のビルドと一致しなければ拒否されるので、基本的に`--app`は不要です。`run`は会話を再生すると撮影可能な状態で停止して待つため、別のターミナルから撮影し、最後に元のターミナルでEnterを押して終了します。

## Linux：README用の画像を自動撮影

Rust/Cargo、Python 3、Xvfb、xdotool、Vulkan（GPUがなければlavapipe）、フォントに加えて、**Openbox**と**xsetroot**が必要です。Debian/Ubuntuでは既存の[テスト環境の準備](../../HOW_TO_TEST.ja.md)に加えて`openbox`と`x11-xserver-utils`をインストールしてください。

```sh
python3 scripts/screenshots/demo.py readme --out assets/screenshots/cayenchat-en.png
python3 scripts/screenshots/demo.py readme --language japanese --out assets/screenshots/cayenchat-ja.png
python3 scripts/screenshots/demo.py check
```

`--variant no-avatars`（アバターなし）、`reiwa`（令和モード）、`parts`（標準表示に4ペインの名称を付けた図：チャンネルログ・全体ログ・メンバーリスト・チャンネルツリー。ImageMagickの`convert`が必要）で、残りのREADME用画像（`cayenchat-{en,ja}-{noicon,reiwa,parts}.png`）を撮影します。コマンドは上と同様に`--language`と`--out`を指定します：

```sh
python3 scripts/screenshots/demo.py readme --variant no-avatars --out assets/screenshots/cayenchat-en-noicon.png
python3 scripts/screenshots/demo.py readme --variant reiwa --out assets/screenshots/cayenchat-en-reiwa.png
python3 scripts/screenshots/demo.py readme --variant parts --out assets/screenshots/cayenchat-en-parts.png
python3 scripts/screenshots/demo.py readme --language japanese --variant no-avatars --out assets/screenshots/cayenchat-ja-noicon.png
python3 scripts/screenshots/demo.py readme --language japanese --variant reiwa --out assets/screenshots/cayenchat-ja-reiwa.png
python3 scripts/screenshots/demo.py readme --language japanese --variant parts --out assets/screenshots/cayenchat-ja-parts.png
```

Xvfb上でOpenboxを起動し、1280×800の仮想画面より少し小さいCayenChatを中央に配置します。**タイトルバー・ウィンドウ枠・背景を含む**画像を撮影し、ツールバーが表示されたか検証します。画像の生成情報は`assets/screenshots/provenance.json`に記録します。

必要に応じて`--theme dark`、`--size 幅x高さ`（仮想画面のサイズ）、`--display :N`を指定できます。Linuxの画像はREADME用で、ストア提出用には使いません。

## macOS：Mac App Store用などの実機撮影

Rust/Cargo、Python 3、XcodeコマンドラインツールがあるMacで、次を実行します。

```sh
python3 scripts/screenshots/demo.py run --language english --theme light --size 1440x900
```

`The demo conversation has been played`と表示され、CayenChatに会話が出たら準備完了です。表示された**PID（プロセスID）**を控えてください。別のターミナルで、同じリポジトリから次を実行します。

```sh
swift scripts/screenshots/window_id.swift PID
screencapture -o -l WINDOW_ID shot.png
```

`PID`と`WINDOW_ID`は実際の数字に置き換えます。ウィンドウIDは大きなウィンドウの順に表示されます。`-o`はウィンドウの影を除外する指定です。`screencapture -o -w shot.png`で撮りたいウィンドウをクリックしても構いません。ターミナルに画面収録権限が求められた場合は「システム設定 → プライバシーとセキュリティ → 画面収録（画面とシステムオーディオの録音）」で許可してください。

メニューバーを含めたい場合は画面全体を撮影します。撮影後、最初のターミナルで**Enter**を押して終了してください。`--language japanese`、`--theme dark`を組み合わせて再撮影できます。

過去のMac App Store向けサイズ例は**1280×800、1440×900、2560×1600、2880×1800（16:10）**です。提出前に最新のストア要件を確認してください。ウィンドウ単体の画像は必ずしも16:10にならないので、必要なら適切な比率の背景に配置します。

**注意：**現在の`demo.py run`は最新のデモバイナリを一時領域にコピーして起動し、`.app`そのものを起動するわけではありません。`sh scripts/bundle-macos.sh --demo`はデモ用バンドルの作成に使えますが、署名・再パッケージ化された実行ファイルを`--app`に渡すと一致検査で拒否されることがあります。

## Windows：Microsoft Store用などの実機撮影

Rust/Cargo（Windows向けビルドツールを含む）とPython 3があるWindowsで、PowerShellから実行します。

```powershell
python scripts/screenshots/demo.py run --language english --theme light --size 1600x900
```

会話が表示されたら、上の例ではWindowsの**表示倍率を100%**にして、**Win+Shift+S → ウィンドウモード**でCayenChatを選びます。取り込んだ画像をペイントに貼り付け（Ctrl+V）、PNGで保存します。1920×1080のデスクトップ全体を撮影する方法でも構いません。

撮影後、最初のPowerShellで**Enter**を押して終了します。`--language japanese`、`--theme dark`でバリエーションを撮影できます。過去のMicrosoft Store向けの目安は**1366×768以上、16:9推奨**でしたが、提出前に最新要件を確認してください。

## 撮影時の確認事項

- 英語／日本語、ライト／ダークの必要な組み合わせを撮る。ツールバー、アバター、画像プレビュー、未読表示、複数チャンネルが写っているか確認する。
- タイトルに`[test build]`が付いていないこと、実ユーザーの発言や認証情報が写っていないことを確認する。
- Linuxで画面が黒い場合は、`VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json`を環境変数に設定して再実行する。Openboxやxsetrootがない場合はエラーになる。
- Linux画像を差し替えた後は`python3 scripts/screenshots/demo.py check`を実行する。Mac・Windowsの手動撮影画像は**自動ではリポジトリに保存されません**。
- 詳細は[開発ドキュメントの「Store screenshots」](../../spec/development.md#store-screenshots)を参照。経緯は[#333](https://github.com/koshian/cayenchat/issues/333)と[#434](https://github.com/koshian/cayenchat/issues/434)。
