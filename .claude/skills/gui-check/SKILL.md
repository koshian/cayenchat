---
name: gui-check
description: Look at the real CayenChat app yourself on a virtual X display (Xvfb) - start it, click, type, press keys and read the screenshots - before asking a person to check something visually. Use whenever a change affects what the user sees or does in the GUI (layout, settings screens, dialogs, menus, scrolling, focus, persistence across restarts), when an issue or PR would otherwise ask someone to "try it and see", or when asked to run or screenshot the app on Linux.
---

# Visual check under Xvfb

`scripts/e2e/gui_session.py` keeps the real app running on a virtual display
between commands. Each input command prints the path of a screenshot taken
once the screen has settled; open it with the Read tool and judge it. The
rule for when this replaces a person, and what to report, is in
`spec/development.md` ("Visual checks under Xvfb"). Linux/X11 only.

## 1. Prepare (once per machine)

```sh
which Xvfb xdotool                    # required; xclip only for `clipboard`
cargo build --locked -p cayenchat-ui  # target/debug/cayenchat
```

- Missing system packages are not installed silently (`spec/development.md`,
  "Development environment"). Without root, unpack them into the scratchpad
  as in `HOW_TO_TEST.md` ("Without root"), then export `PATH`,
  `LD_LIBRARY_PATH` and (for the link step) `LIBRARY_PATH` before the build
  and before every `gui_session.py` command.
- If the app panics with `failed to resolve font '.SystemUIFont'`, the
  machine has no fonts: unpack `fonts-dejavu-core` and
  `fonts-ipafont-gothic` the same way and pass
  `--fonts <root>/usr/share/fonts` to `start`.
- Use a session directory in your scratchpad:
  `export CAYENCHAT_GUI_SESSION=<scratchpad>/gui` (or `--session` on every
  command). It holds the app's own HOME and settings, `shots/`, `app.log`.
  Use a new or empty directory: `start` refuses a non-empty one it did not
  make, and later clears only its own entries there.

## 2. Drive the app

```sh
G=scripts/e2e/gui_session.py
python3 $G start --app target/debug/cayenchat   # prints 01_started.png
python3 $G windows                               # id, x,y, size, title
python3 $G click 700 308                         # screen pixels, as in the shots
python3 $G type "alice" ; python3 $G key Tab
python3 $G key ctrl+comma --still 2              # a window opens: wait longer
python3 $G scroll 400 300 up 5
python3 $G shot detail --crop 390,410,1090,640 --scale 2   # zoom to read text
python3 $G click 400 407 --repeat 2 --no-shot    # double-click a link in the log
python3 $G opened                                # URLs opened since the last `opened`
python3 $G restart-app                           # Ctrl+Q, then start again
python3 $G quit                                  # Ctrl+Q only
python3 $G stop
```

- `restart-app` quits with Ctrl+Q as a user does, so what the app saves on
  quitting is part of the check. If it says the app was terminated instead,
  that saving was skipped: do not report it as verified.
- `start` writes fresh settings (Japanese, light theme, local credential
  file, no servers, so the settings window opens). `--language english`,
  `--theme dark`, `--settings FILE` for a prepared file (its
  `credential_backend` is forced to the session's local file, and the app
  never sees the desktop's D-Bus session, so the user's stored passwords
  stay out of reach), `--server
  127.0.0.1:36668 --channel '#demo' --ircv3 message_tags,server_time,batch,chathistory`
  to connect on startup, `--window-size 1000x700` to place the main window.
- Pass `--no-shot` for intermediate steps and take one `shot` at the end.
- A real IRC server: run the local Ergo playground in the background with
  its commands read from a file you append to:

  ```sh
  ERGO_SETUP_ONLY=1 ERGO_NO_FAKELAG=1 scripts/ergo-metadata-interop.sh /tmp/cayenchat-ergo 36667
  touch <scratchpad>/bob.txt
  tail -f <scratchpad>/bob.txt | python3 scripts/e2e/manual_history.py --ergo-dir /tmp/cayenchat-ergo &
  echo "say hello from bob" >> <scratchpad>/bob.txt
  ```

  (Building Ergo needs Go; see `HOW_TO_TEST.md`.)

## 3. Pitfalls

- New windows are opaque black until the app draws them (a few seconds with
  software rendering). `start` waits for that; after opening a window use
  `--still 2` or `wait --still 2`.
- Layout moves after toggles (a TLS switch adds a row): take a shot before
  clicking the next control instead of reusing old coordinates.
- A drop-down or control may react only on its label, not its whole box.
- There is no window manager: windows do not get decorations and keyboard
  focus follows the pointer (move or click inside a window before typing).
  `--wm openbox` if a check needs one.
- `type` sends key events without an input method: ASCII only. Japanese
  input (IME) cannot be checked here.
- Opening a link starts no browser: the URL goes to the session's
  `opened.txt` and `opened` prints it. Check the exact URL there rather
  than in a screenshot. `Failed to open with dbus` in `app.log` is expected.
- "(the screen was still changing)" after a path usually means a blinking
  caret; the shot is still usable.
- `app.log` has the app's stderr; look there when a command says the app
  exited.

## 4. Report

In the PR or issue, list what you verified this way (with the screenshots
that matter, attached or described) and what is left unverified. Never call
a macOS, Windows, Wayland, IME or HiDPI behavior verified from an Xvfb run.
Something left unverified holds the merge for a person only in the cases
listed in `spec/development.md` ("Changes that need a person's
confirmation"); otherwise it is checked on the beta after merging.
