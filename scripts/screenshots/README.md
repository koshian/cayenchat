# Screenshot capture guide

[日本語の手順](README.ja.md)

Use this guide to recreate the screenshots after UI changes, without looking up old Issues. Run commands **from the repository root**. The demo uses a local-only IRC server, fictional conversation and generated pictures; it builds the **current source** with the `demo` feature, isolates settings and credentials, and never connects to a real IRC network. **Never distribute the demo build.**

The starting command rebuilds as needed; do not pass an old executable with `--app` (it will be rejected). After the conversation has finished, the command waits for Enter, leaving the window visible so you can capture it from a second terminal.

## Linux — README pictures (automated)

Prerequisites: Rust/Cargo, Python 3, Xvfb, xdotool, software Vulkan (lavapipe, if there is no GPU), fonts, **Openbox** and **xsetroot**. On Debian/Ubuntu, install `openbox` and `x11-xserver-utils` in addition to the requirements in [HOW_TO_TEST.md](../../HOW_TO_TEST.md).

```sh
python3 scripts/screenshots/demo.py readme --out assets/screenshots/cayenchat-en.png
python3 scripts/screenshots/demo.py readme --language japanese --out assets/screenshots/cayenchat-ja.png
python3 scripts/screenshots/demo.py check
```

`--variant no-avatars` (avatars off), `reiwa` (Reiwa mode) and `parts` (the standard window with the four panes named: channel log, combined log, member list, channel tree; needs ImageMagick `convert`) take the other README pictures (`cayenchat-{en,ja}-{noicon,reiwa,parts}.png`, one command each, as above with `--language` and `--out`):

```sh
python3 scripts/screenshots/demo.py readme --variant no-avatars --out assets/screenshots/cayenchat-en-noicon.png
python3 scripts/screenshots/demo.py readme --variant reiwa --out assets/screenshots/cayenchat-en-reiwa.png
python3 scripts/screenshots/demo.py readme --variant parts --out assets/screenshots/cayenchat-en-parts.png
python3 scripts/screenshots/demo.py readme --language japanese --variant no-avatars --out assets/screenshots/cayenchat-ja-noicon.png
python3 scripts/screenshots/demo.py readme --language japanese --variant reiwa --out assets/screenshots/cayenchat-ja-reiwa.png
python3 scripts/screenshots/demo.py readme --language japanese --variant parts --out assets/screenshots/cayenchat-ja-parts.png
```

The script runs Openbox on Xvfb, gives CayenChat a smaller window centered in the 1280×800 virtual screen, and captures the title bar, borders and some neutral desktop background. It verifies that the toolbar was rendered, and writes the capture metadata to `assets/screenshots/provenance.json`. Use `--theme dark`, `--size WIDTHxHEIGHT` (virtual screen size), or `--display :N` if necessary. These images are for the README, **not store submissions**.

## macOS — Mac App Store / manual window capture

On the Mac with Xcode command-line tools, Rust and Python 3 installed:

```sh
python3 scripts/screenshots/demo.py run --language english --theme light --size 1440x900
```

Wait until it prints `The demo conversation has been played` and the CayenChat window appears. Note the **PID** printed by the script. In another Terminal window, from the same repository:

```sh
swift scripts/screenshots/window_id.swift PID
screencapture -o -l WINDOW_ID shot.png
```

Replace `PID` and `WINDOW_ID` with numbers from the output (the largest window is listed first). `-o` omits the window shadow. Alternatively, use `screencapture -o -w shot.png` and click the window. Terminal may need **System Settings → Privacy & Security → Screen & System Audio Recording** permission. To include the macOS menu bar, capture the whole screen at an appropriate resolution instead.

After capturing, press **Enter** in the first Terminal to stop the demo. Repeat with `--language japanese` and/or `--theme dark`. Match current Mac App Store requirements before publishing; earlier supported examples were 1280×800, 1440×900, 2560×1600 and 2880×1800 (16:10). A window-only capture is not necessarily 16:10; place it on a compliant canvas if required.

**Important:** The current `demo.py run` checks and runs a staged copy of the fresh demo binary, not the macOS `.app` bundle. The separate `sh scripts/bundle-macos.sh --demo` command is for preparing a demo app bundle, but passing an old, signed or repackaged bundle executable to `--app` is not a reliable substitute for the command above.

## Windows — Microsoft Store / manual window capture

With Rust/Cargo (including the Windows build tools) and Python 3 installed, open PowerShell in the repository root:

```powershell
python scripts/screenshots/demo.py run --language english --theme light --size 1600x900
```

Wait for the demo conversation to finish loading. Set Windows **display scaling to 100%** for the 1600×900 example (or adjust `--size` and scaling together). Press **Win+Shift+S**, select **Window** mode, and click the CayenChat window. Paste into Paint (Ctrl+V), then save as a PNG. Alternatively capture the full 1920×1080 desktop. Stop the demo by pressing **Enter** in PowerShell.

Repeat with `--language japanese` and/or `--theme dark`. The previous Microsoft Store guidance called for at least 1366×768 and recommended 16:9; **confirm current store specifications before submission**.

## Checklist and troubleshooting

- Capture English/Japanese × light/dark as needed, with toolbar, avatars, image preview, unread entries and multiple channels visible.
- Ensure the window does **not** show `[test build]`; no real users' messages, tokens or credentials should appear.
- On Linux, if the capture is black, try `VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json` before the command. Missing `openbox` or `xsetroot` produces a clear error.
- Use `python3 scripts/screenshots/demo.py check` after replacing the Linux README images. Mac/Windows captures are manual and are **not** automatically written into the repository or its README.
- See [spec/development.md — Store screenshots](../../spec/development.md#store-screenshots) for implementation details, and historical [#333](https://github.com/koshian/cayenchat/issues/333) / [#434](https://github.com/koshian/cayenchat/issues/434) for background.
