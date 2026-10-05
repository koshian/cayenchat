#!/usr/bin/env python3
"""An interactive look at the real CayenChat on a virtual X display, one
command at a time, for checks that would otherwise need someone to watch
the screen (spec/development.md, "Visual checks under Xvfb").

    scripts/e2e/gui_session.py start --app target/debug/cayenchat
    scripts/e2e/gui_session.py key ctrl+comma      # prints a screenshot path
    scripts/e2e/gui_session.py click 420 180
    scripts/e2e/gui_session.py shot settings --crop 300,100,800,400 --scale 2
    scripts/e2e/gui_session.py stop

`start` leaves Xvfb and the app running in the background and returns.
Every input command then waits until the screen stops changing and saves
it as a numbered PNG under the session's `shots/`, printing the path (pass
`--no-shot` to skip). Coordinates are screen pixels, the same as in the
screenshots. The app runs with its own HOME and settings in the session
directory, keeps passwords in the session's own file (even with `--settings`
copied from real settings) and cannot reach the desktop's D-Bus session, so
the user's settings and passwords are never touched. `start` refuses a
non-empty directory it did not make. Links the app opens reach no browser:
`opened` prints the URLs it handed to the desktop.

Needs Linux with Xvfb, xdotool and a Vulkan driver (lavapipe from
mesa-vulkan-drivers works without a GPU) and a font (`--fonts` when the
system has none); xclip only for `clipboard`. Screens are read from Xvfb's
framebuffer file, so xwd is not needed.
"""
import argparse, json, os, shlex, shutil, signal, subprocess, sys, tempfile, time
from xml.sax.saxutils import escape

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from history_gui import Display  # noqa: E402

SETTINGS_VERSION = 15
# Marks a directory as a session; only these entries are cleared again.
MARKER = ".cayenchat-gui-session"
SESSION_ENTRIES = ("state.json", "home", "test-build", "run", "fb", "shots", "app.log", "xvfb.log",
                   "bin", "opened.txt")
# What GPUI runs to open a link once the desktop portal is out of reach
# (the `open` crate's Linux commands); each stand-in only records the URL.
OPENERS = ("xdg-open", "gio", "gnome-open", "kde-open", "wslview")


def session_dir(args):
    return os.path.abspath(args.session or os.environ.get("CAYENCHAT_GUI_SESSION")
                           or os.path.join(tempfile.gettempdir(), "cayenchat-gui"))


def load(directory):
    try:
        with open(os.path.join(directory, "state.json")) as file:
            return json.load(file)
    except FileNotFoundError:
        sys.exit(f"no session in {directory}; run `start` first")


def save_state(state):
    with open(os.path.join(state["dir"], "state.json"), "w") as file:
        json.dump(state, file, indent=2)


def alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except OSError:
        return False


def display_for(state):
    out = os.path.join(state["dir"], "shots")
    display = Display(state["env"], out, framebuffer=os.path.join(state["dir"], "fb", "Xvfb_screen0"))
    display.shots = len([name for name in os.listdir(out) if name.endswith(".png")])
    return display


def settle(display, timeout=8.0, still=0.3):
    """The screen once it has not changed for `still` seconds, or the last
    grab when it keeps changing (a blinking caret, an animation); the bool
    says which."""
    deadline = time.time() + timeout
    last, since = display.grab(), time.time()
    while time.time() < deadline:
        time.sleep(0.15)
        now = display.grab()
        if now[2] != last[2]:
            last, since = now, time.time()
        elif time.time() - since >= still:
            return now, True
    return last, False


def crop_scale(shot, box, scale):
    width, height, rows = shot
    if box:
        x0, y0, x1, y1 = box
        x0, y0, x1, y1 = max(0, x0), max(0, y0), min(width, x1), min(height, y1)
        rows = [row[x0 * 4: x1 * 4] for row in rows[y0:y1]]
        width, height = x1 - x0, y1 - y0
    if scale > 1:
        scaled = []
        for row in rows:
            wide = b"".join(row[i: i + 4] * scale for i in range(0, len(row), 4))
            scaled.extend([wide] * scale)
        rows, width, height = scaled, width * scale, height * scale
    return width, height, rows


def save(display, name, box=None, scale=1, timeout=8.0, still=0.3):
    shot, still = settle(display, timeout, still)
    path = display.save(crop_scale(shot, box, scale), name)
    print(path if still else f"{path} (the screen was still changing)")


def prepare_directory(directory, keep):
    """Creates the session directory, or clears what an earlier session left
    in it. A directory this script did not make is refused when it is not
    empty, and clearing removes only this script's own entries."""
    marker = os.path.join(directory, MARKER)
    if os.path.isdir(directory) and os.listdir(directory) and not os.path.exists(marker):
        sys.exit(f"{directory} is not empty and was not made by gui_session.py; "
                 "pass a new or empty --session")
    if not keep:
        for name in SESSION_ENTRIES:
            path = os.path.join(directory, name)
            if os.path.isdir(path) and not os.path.islink(path):
                shutil.rmtree(path)
            elif os.path.lexists(path):
                os.remove(path)
    # state.json holds the app's environment.
    os.makedirs(directory, mode=0o700, exist_ok=True)
    open(marker, "a").close()


def settings_json(args):
    if args.settings:
        with open(args.settings) as file:
            settings = json.load(file)
        # Passwords go to the session's own file even from a copy of real
        # settings: the system store would be the user's (its service name
        # does not change with HOME).
        if settings.get("credential_backend", "system") != "local_file":
            print("credential_backend set to local_file", file=sys.stderr)
        settings["credential_backend"] = "local_file"
        return json.dumps(settings, indent=2)
    settings = {"version": SETTINGS_VERSION, "language": args.language, "theme": args.theme,
                "linux_display": "x11", "credential_backend": "local_file"}
    if args.server:
        host, _, port = args.server.rpartition(":")
        settings["selected_server"] = "local"
        settings["servers"] = [{
            "id": "local", "host": host, "port": int(port), "use_tls": False,
            "verify_tls_certificates": True, "encoding": "utf8", "nickname": args.nick,
            "username": args.nick, "channels": args.channel, "connect_on_startup": True,
            "ircv3": {name: True for name in args.ircv3.split(",") if name}}]
    return json.dumps(settings, indent=2)


def app_env(base, directory, display, extra=()):
    """The app's environment: everything it stores stays in the session
    directory, and the desktop's D-Bus session (with its Secret Service
    holding the user's passwords) is out of reach. Links go to stand-in
    openers that append them to the session's opened.txt. `extra` (KEY=VALUE)
    cannot undo that."""
    env = dict(base)
    for pair in extra:
        key, _, value = pair.partition("=")
        env[key] = value
    home = os.path.join(directory, "home")
    runtime = os.path.join(directory, "run")
    os.makedirs(runtime, mode=0o700, exist_ok=True)
    env.update(DISPLAY=display, HOME=home, XDG_CONFIG_HOME=os.path.join(home, "config"),
               XDG_DATA_HOME=os.path.join(home, "data"), XDG_CACHE_HOME=os.path.join(home, "cache"),
               XDG_RUNTIME_DIR=runtime, CAYENCHAT_TEST_DIR=os.path.join(directory, "test-build"),
               CAYENCHAT_DISPLAY="x11")
    env["PATH"] = os.pathsep.join(filter(None, (install_openers(directory), env.get("PATH"))))
    env.setdefault("RUST_LOG", "warn")
    for key in ("WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS"):
        env.pop(key, None)
    return env


def install_openers(directory):
    """Writes the stand-in openers and returns their directory. The URL is
    the last argument (`gio open URL`)."""
    bin_dir = os.path.join(directory, "bin")
    os.makedirs(bin_dir, exist_ok=True)
    opened = os.path.join(directory, "opened.txt")
    script = "#!/bin/sh\nfor url; do :; done\nprintf '%s\\n' \"$url\" >> " + shlex.quote(opened) + "\n"
    for name in OPENERS:
        path = os.path.join(bin_dir, name)
        with open(path, "w") as file:
            file.write(script)
        os.chmod(path, 0o755)
    return bin_dir


def launch_app(state):
    log = open(os.path.join(state["dir"], "app.log"), "a")
    app = subprocess.Popen([state["app"]], env=state["env"], stdin=subprocess.DEVNULL, stdout=log,
                           stderr=subprocess.STDOUT, start_new_session=True)
    state["pids"]["app"] = app.pid


def wait_for_window(display, state, timeout=60):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if not alive(state["pids"]["app"]):
            sys.exit(f"the app exited; see {state['dir']}/app.log")
        found = display.run("xdotool", "search", "--onlyvisible", "--name", "CayenChat",
                            capture_output=True, text=True).stdout.split()
        if found:
            return found[0]
        time.sleep(0.5)
    sys.exit(f"no CayenChat window after {timeout} s; see {state['dir']}/app.log")


def first_frame(display, state, args):
    window = wait_for_window(display, state)
    if args.window_size:
        width, height = args.window_size.split("x")
        display.xdo("windowmove", window, "0", "0")
        display.xdo("windowsize", window, width, height)
    # New windows are opaque black until the app draws into them (a few
    # seconds with software rendering, sooner after an input event); that
    # black screen must not count as settled. Pixels are B, G, R, alpha.
    deadline = time.time() + 30
    while time.time() < deadline:
        display.xdo("mousemove", "5", "5")
        display.xdo("mousemove", "6", "6")
        if any(row[0::4].strip(b"\0") or row[1::4].strip(b"\0") or row[2::4].strip(b"\0")
               for row in display.grab()[2]):
            break
        time.sleep(0.5)
    save(display, "started", timeout=15, still=1.5)


# ----------------------------------------------------------------- commands
def start(args):
    directory = session_dir(args)
    if os.path.exists(os.path.join(directory, "state.json")):
        state = load(directory)
        if any(alive(pid) for pid in state["pids"].values()):
            sys.exit(f"a session is running in {directory}; `stop` it or pass another --session")
    if not os.access(args.app, os.X_OK):
        sys.exit(f"{args.app} is not an executable; build it first")
    prepare_directory(directory, args.keep)
    home = os.path.join(directory, "home")
    config = os.path.join(home, "config")
    test_dir = os.path.join(directory, "test-build")
    for path in (os.path.join(config, "CayenChat"), os.path.join(directory, "fb"),
                 os.path.join(directory, "shots"), test_dir):
        os.makedirs(path, exist_ok=True)
    os.chmod(test_dir, 0o700)
    if args.fonts:
        # The app cannot start without a font. GPUI finds fonts through
        # fontconfig files only; this user file adds to /etc/fonts.
        os.makedirs(os.path.join(config, "fontconfig"), exist_ok=True)
        with open(os.path.join(config, "fontconfig", "fonts.conf"), "w") as file:
            dirs = "".join(f"  <dir>{escape(os.path.abspath(path))}</dir>\n" for path in args.fonts)
            file.write(f"<?xml version=\"1.0\"?>\n<fontconfig>\n{dirs}</fontconfig>\n")
    if not args.keep or not os.path.exists(os.path.join(config, "CayenChat", "settings.json")):
        settings = settings_json(args)
        # A normal build reads the first, a `test-build` one the second.
        for path in (os.path.join(config, "CayenChat", "settings.json"),
                     os.path.join(test_dir, "settings.json")):
            with open(path, "w") as file:
                file.write(settings)

    env = app_env(os.environ, directory, args.display, args.env)
    xvfb = subprocess.Popen(["Xvfb", args.display, "-screen", "0", f"{args.screen}x24",
                             "-fbdir", os.path.join(directory, "fb"), "-nolisten", "tcp"],
                            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                            stderr=open(os.path.join(directory, "xvfb.log"), "w"), start_new_session=True)
    state = {"dir": directory, "app": os.path.abspath(args.app), "env": env, "pids": {"xvfb": xvfb.pid}}
    time.sleep(1)
    if xvfb.poll() is not None:
        sys.exit(f"Xvfb did not start on {args.display}; see {directory}/xvfb.log (try --display :98)")
    if args.wm:
        wm = subprocess.Popen([args.wm], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                              stderr=subprocess.DEVNULL, start_new_session=True)
        state["pids"]["wm"] = wm.pid
    launch_app(state)
    save_state(state)
    print(f"session {directory} on {args.display}")
    first_frame(display_for(state), state, args)


def quit_app(state, timeout=10):
    """Quits the app as a user does, with its Quit shortcut (Ctrl+Q) in one
    of its windows; true once it has exited."""
    pid = state["pids"]["app"]
    if not alive(pid):
        return True
    display = display_for(state)
    found = display.run("xdotool", "search", "--onlyvisible", "--name", "CayenChat",
                        capture_output=True, text=True).stdout.split()
    if found:
        display.xdo("windowfocus", found[0])
        time.sleep(0.2)
        display.xdo("key", "--clearmodifiers", "ctrl+q")
    deadline = time.time() + timeout
    while alive(pid) and time.time() < deadline:
        time.sleep(0.2)
    return not alive(pid)


def quit_command(args):
    state = load(session_dir(args))
    if quit_app(state):
        print("the app quit")
    else:
        sys.exit("the app did not quit within 10 s; `stop` ends it")


def restart_app(args):
    """Quits the app as a user does and starts it again in the same session,
    to check what survives a restart. Only when it does not quit is it
    terminated by a signal, which skips what quitting saves; that is said."""
    state = load(session_dir(args))
    if not quit_app(state):
        stop_process(state["pids"]["app"])
        print("the app did not quit with Ctrl+Q and was terminated; what it saves on quitting was skipped")
    launch_app(state)
    save_state(state)
    first_frame(display_for(state), state, args)


def stop_process(pid):
    try:
        os.killpg(pid, signal.SIGTERM)
    except OSError:
        return
    deadline = time.time() + 5
    while alive(pid) and time.time() < deadline:
        time.sleep(0.1)
    if alive(pid):
        os.killpg(pid, signal.SIGKILL)


def stop(args):
    state = load(session_dir(args))
    for name in ("app", "wm", "xvfb"):
        if name in state["pids"]:
            stop_process(state["pids"][name])
    print(f"stopped; screenshots and app.log stay in {state['dir']}")


def windows(args):
    display = display_for(load(session_dir(args)))
    ids = display.run("xdotool", "search", "--onlyvisible", "--name", ".",
                      capture_output=True, text=True).stdout.split()
    for window in ids:
        name = display.run("xdotool", "getwindowname", window, capture_output=True, text=True).stdout.strip()
        geometry = display.run("xdotool", "getwindowgeometry", "--shell", window,
                               capture_output=True, text=True).stdout
        values = dict(line.split("=", 1) for line in geometry.split())
        print(f"{window}\t{values.get('X')},{values.get('Y')} {values.get('WIDTH')}x{values.get('HEIGHT')}\t{name}")


def act(args):
    """Input commands: send the events, then save the settled screen."""
    state = load(session_dir(args))
    display = display_for(state)
    command = args.command
    if command == "click":
        display.xdo("mousemove", str(args.x), str(args.y))
        time.sleep(0.1)
        display.xdo("click", "--repeat", str(args.repeat), "--delay", "80", str(args.button))
    elif command == "move":
        display.xdo("mousemove", str(args.x), str(args.y))
    elif command == "drag":
        display.xdo("mousemove", str(args.x0), str(args.y0))
        time.sleep(0.1)
        display.xdo("mousedown", "1")
        for step in range(1, 11):
            display.xdo("mousemove", str(args.x0 + (args.x1 - args.x0) * step // 10),
                        str(args.y0 + (args.y1 - args.y0) * step // 10))
            time.sleep(0.05)
        display.xdo("mouseup", "1")
    elif command == "scroll":
        display.xdo("mousemove", str(args.x), str(args.y))
        for _ in range(args.clicks):
            display.xdo("click", "4" if args.direction == "up" else "5")
            time.sleep(0.04)
    elif command == "key":
        for key in args.keys:
            display.xdo("key", "--clearmodifiers", key)
            time.sleep(0.1)
    elif command == "type":
        display.xdo("type", "--delay", "30", args.text)
    elif command == "focus":
        display.xdo("windowfocus", args.window)
    elif command != "wait":
        raise AssertionError(command)
    if not alive(state["pids"]["app"]):
        sys.exit(f"the app has exited; see {state['dir']}/app.log")
    if not args.no_shot:
        time.sleep(args.pause)
        save(display, args.name or command, still=args.still)


def shot(args):
    display = display_for(load(session_dir(args)))
    box = tuple(int(value) for value in args.crop.split(",")) if args.crop else None
    save(display, args.name, box, args.scale)


def clipboard(args):
    display = display_for(load(session_dir(args)))
    result = display.run("xclip", "-selection", "clipboard", "-o", capture_output=True, text=True)
    sys.stdout.write(result.stdout)


def opened(args):
    """Prints the URLs opened since the last `opened` (all with --all)."""
    state = load(session_dir(args))
    try:
        with open(os.path.join(state["dir"], "opened.txt")) as file:
            urls = file.read().splitlines()
    except FileNotFoundError:
        urls = []
    start = 0 if args.all else state.get("opened_seen", 0)
    for url in urls[start:]:
        print(url)
    state["opened_seen"] = len(urls)
    save_state(state)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--session", help="session directory (default $CAYENCHAT_GUI_SESSION or $TMPDIR/cayenchat-gui)")
    commands = parser.add_subparsers(dest="command", required=True)

    def launch_options(command):
        command.add_argument("--window-size", help="move the main window to 0,0 and size it, e.g. 1000x700")

    command = commands.add_parser("start", help="start Xvfb and the app")
    command.add_argument("--app", required=True, help="the cayenchat binary")
    command.add_argument("--display", default=":98")
    command.add_argument("--screen", default="1280x900", help="virtual screen size")
    command.add_argument("--settings", help="settings.json to start with instead of the generated one")
    command.add_argument("--language", default="japanese", choices=["japanese", "english", "system"])
    command.add_argument("--theme", default="light", choices=["light", "dark", "system"])
    command.add_argument("--server", help="HOST:PORT to connect to on startup (no TLS), e.g. a local Ergo")
    command.add_argument("--channel", default="#demo")
    command.add_argument("--nick", default="alice")
    command.add_argument("--ircv3", default="", help="IRCv3 options to turn on, e.g. message_tags,server_time,batch,chathistory")
    command.add_argument("--wm", help="window manager to run first, e.g. openbox (focus and stacking)")
    command.add_argument("--fonts", action="append", default=[],
                         help="a font directory to add when the system has none (repeatable)")
    command.add_argument("--env", action="append", default=[], help="KEY=VALUE for the app (repeatable)")
    command.add_argument("--keep", action="store_true", help="reuse the session's HOME and settings")
    launch_options(command)
    command.set_defaults(func=start)

    command = commands.add_parser("restart-app", help="quit the app with Ctrl+Q and start it again in this session")
    launch_options(command)
    command.set_defaults(func=restart_app)
    commands.add_parser("quit", help="quit the app with Ctrl+Q, as a user does").set_defaults(func=quit_command)
    commands.add_parser("stop", help="stop the app and Xvfb").set_defaults(func=stop)
    commands.add_parser("windows", help="list visible windows: id, position, size, title").set_defaults(func=windows)

    command = commands.add_parser("shot", help="save the screen")
    command.add_argument("name", nargs="?", default="shot")
    command.add_argument("--crop", help="x0,y0,x1,y1 in screen pixels")
    command.add_argument("--scale", type=int, default=1, help="enlarge by this factor to read small text")
    command.set_defaults(func=shot)
    commands.add_parser("clipboard", help="print the clipboard (needs xclip)").set_defaults(func=clipboard)
    command = commands.add_parser("opened", help="print the URLs the app opened since the last `opened`")
    command.add_argument("--all", action="store_true", help="every URL opened in this session")
    command.set_defaults(func=opened)

    def input_command(name, help):
        command = commands.add_parser(name, help=help)
        command.add_argument("--name", help="screenshot name (default: the command)")
        command.add_argument("--no-shot", action="store_true")
        command.add_argument("--pause", type=float, default=0.3, help="seconds before the screenshot")
        command.add_argument("--still", type=float, default=0.3,
                             help="seconds the screen must stay unchanged; about 2 when a window opens")
        command.set_defaults(func=act)
        return command

    command = input_command("click", "click at X Y")
    command.add_argument("x", type=int)
    command.add_argument("y", type=int)
    command.add_argument("--button", type=int, default=1, help="1 left, 2 middle, 3 right")
    command.add_argument("--repeat", type=int, default=1, help="2 for a double click")
    command = input_command("move", "move the pointer to X Y (hover)")
    command.add_argument("x", type=int)
    command.add_argument("y", type=int)
    command = input_command("drag", "drag with the left button from X0 Y0 to X1 Y1")
    for name in ("x0", "y0", "x1", "y1"):
        command.add_argument(name, type=int)
    command = input_command("scroll", "turn the wheel at X Y")
    command.add_argument("x", type=int)
    command.add_argument("y", type=int)
    command.add_argument("direction", choices=["up", "down"])
    command.add_argument("clicks", type=int, nargs="?", default=3)
    command = input_command("key", "press keys in xdotool syntax, e.g. ctrl+comma Tab Return")
    command.add_argument("keys", nargs="+")
    command = input_command("type", "type TEXT (ASCII; no input method)")
    command.add_argument("text")
    command = input_command("focus", "give a window (an id from `windows`) the keyboard focus")
    command.add_argument("window")
    input_command("wait", "only wait for the screen to settle and save it")

    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
