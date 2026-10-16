#!/usr/bin/env python3
"""A scripted demo conversation for store and README screenshots
(spec/development.md, "Store screenshots").

    demo.py run [--app PATH] [--language english|japanese] [--theme light|dark]
    demo.py readme --out assets/screenshots/cayenchat-en.png [--language ...]
    demo.py check
    demo.py serve --port PORT --images DIR [--language ...]
    demo.py settings --port PORT [--language ...] [--theme ...]

`run` does everything on macOS, Windows and Linux desktops: it writes
throwaway settings and images under --dir, starts the demo IRC server on
127.0.0.1, launches a `demo` build of CayenChat (cargo feature `demo`) and
waits until the conversation has been played. Take the screenshots, then press
Enter to quit. The demo build is always made first; an --app that is not
byte for byte that build is refused. `readme` does the
Linux/Xvfb flow in one go (build, picture, a toolbar-less run to prove the
toolbar is on screen, entry in assets/screenshots/provenance.json) and `check`
fails when those pictures lag behind the app. `serve` and `settings` are the
two halves of the Xvfb flow with scripts/e2e/gui_session.py.

The server speaks just enough IRC for the demo: registration, CAP with
server-time and draft/metadata-2 (avatars), JOIN with topics and member lists,
then fixed PRIVMSG lines with today's local times. It accepts one client,
listens only on 127.0.0.1 and never reaches another host. Every image
(avatars and the photo) is drawn by this script, so nothing with a license of
its own is involved, and the app reads them from --dir through the
`preview-fixture` part of the `demo` feature instead of the network.

Only the Python standard library is used.
"""
import argparse
import asyncio
import colorsys
import datetime
import hashlib
import json
import math
import os
import platform
import shutil
import struct
import subprocess
import sys
import threading
import zlib

SETTINGS_VERSION = 15
SERVER = "demo.cayenchat.test"
IMAGES = "https://images.cayenchat.test/"
PHOTO = "lake.png"

# ---------------------------------------------------------------- the script
# Per language: our nickname, the network's name, channels (first one is
# shown) and private messages. Lines are (nickname, text); `action(text)`
# makes a `/me` line (CTCP ACTION). Nicknames prefixed with @ or + are
# operators or voiced in the member list. Users in NO_AVATAR show the app's own
# default avatar.
def action(text):
    return "\x01ACTION " + text + "\x01"


SCRIPTS = {
    "english": {
        "nick": "sam",
        "network": "DemoNet",
        "channels": [
            {
                "name": "#cayenchat",
                "topic": "CayenChat - a modern IRC client | 1.0 is out!",
                "members": ["@alice", "@bob", "+carol", "+dave", "erin", "frank", "grace", "heidi",
                            "ivan", "judy", "kate", "leo", "mia", "noah"],
                "lines": [
                    ("alice", "Evening, all! Has anyone tried the new release yet?"),
                    ("bob", "Yes, avatars now show up on our old bouncer too."),
                    ("carol", "Same here, and my old ISO-2022-JP channel works fine."),
                    ("dave", "The view from my trip last weekend: " + IMAGES + PHOTO),
                    ("erin", "Beautiful! Is that Lake Kawaguchi?"),
                    ("dave", "It is. The preview loaded right in the log."),
                    ("frank", "sam: could you check the release notes before we announce? "
                              "https://github.com/koshian/cayenchat/releases/tag/v1.0.0"),
                    ("grace", "Long links are shortened in the log, but copying keeps the full URL."),
                    ("alice", "Nice. I switched to the dark theme last night and kept it."),
                    ("bob", action("raises a glass to the new release")),
                ],
            },
            {
                "name": "#rust",
                "topic": "Rust programming",
                "members": ["@ivan", "judy", "kate", "leo"],
                "lines": [
                    ("ivan", "Has anyone profiled the new borrow checker yet?"),
                    ("judy", "A bit. Compile times look about the same for us."),
                ],
            },
            {
                "name": "#random",
                "topic": "Anything goes",
                "members": ["mia", "noah", "erin"],
                "lines": [
                    ("mia", "sam, are you joining the meetup on Friday?"),
                ],
            },
        ],
        "private": [("erin", "Lunch at noon?")],
    },
    "japanese": {
        "nick": "mika",
        "network": "DemoNet",
        "channels": [
            {
                "name": "#cayenchat",
                "topic": "CayenChat - モダンな IRC クライアント | 1.0 リリースしました!",
                "members": ["@akira", "@yui", "+ken", "+mio", "haru", "taro", "sora", "rin",
                            "nao", "kai", "emi", "jun", "riku", "saki"],
                "lines": [
                    ("akira", "こんばんは。新しいリリース、もう試しました?"),
                    ("yui", "試しました。古いバウンサー経由でもアバターが出るようになってますね"),
                    ("ken", "ISO-2022-JP の古いチャンネルも問題なく読めてます"),
                    ("mio", "週末に行った湖の写真です " + IMAGES + PHOTO),
                    ("haru", "きれい!河口湖ですか?"),
                    ("mio", "そうです。ログの中でそのままプレビューが出るのが便利"),
                    ("taro", "mika さん、告知の前にリリースノートを確認してもらえますか? "
                             "https://github.com/koshian/cayenchat/releases/tag/v1.0.0"),
                    ("sora", "長い URL はログでは短く表示されて、コピーすると元の URL になります"),
                    ("akira", "昨夜ダークテーマに切り替えて、そのまま使ってます"),
                    ("yui", action("新しいリリースに乾杯")),
                ],
            },
            {
                "name": "#rust-jp",
                "topic": "Rust の話題",
                "members": ["@nao", "kai", "emi", "jun"],
                "lines": [
                    ("nao", "新しい借用チェッカーのプロファイルを取った人いますか?"),
                    ("kai", "少しだけ。うちではコンパイル時間はほぼ同じでした"),
                ],
            },
            {
                "name": "#zatsudan",
                "topic": "雑談",
                "members": ["riku", "saki", "haru"],
                "lines": [
                    ("riku", "mika さん、金曜のオフ会来ますか?"),
                ],
            },
        ],
        "private": [("haru", "お昼一緒にどうですか?")],
    },
}
NO_AVATAR = {"heidi", "noah", "rin", "saki"}


def bare(member):
    return member.lstrip("@+")


def everyone(script):
    names = {bare(member) for channel in script["channels"] for member in channel["members"]}
    names.update(nick for nick, _ in script["private"])
    return sorted(names)


# ----------------------------------------------------------------- the images
def png(width, height, pixel):
    """An RGB PNG; `pixel(x, y)` returns (r, g, b) with 0-255 integers."""
    rows = bytearray()
    for y in range(height):
        rows.append(0)
        for x in range(width):
            rows.extend(pixel(x, y))

    def chunk(kind, data):
        return (struct.pack(">I", len(data)) + kind + data
                + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF))

    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
            + chunk(b"IDAT", zlib.compress(bytes(rows), 9)) + chunk(b"IEND", b""))


def rgb(hue, saturation, value):
    return tuple(round(c * 255) for c in colorsys.hsv_to_rgb(hue % 1.0, saturation, value))


def avatar(name, size=96):
    """A symmetric 5x5 pattern in a color derived from the name."""
    digest = hashlib.sha256(name.encode()).digest()
    hue = digest[0] / 255
    background = rgb(hue, 0.25, 0.97)
    foreground = rgb(hue, 0.65, 0.75)
    cells = [[digest[1 + row * 3 + min(col, 4 - col)] & 1 for col in range(5)] for row in range(5)]
    margin = size // 8
    cell = (size - 2 * margin) / 5

    def pixel(x, y):
        col, row = int((x - margin) // cell), int((y - margin) // cell)
        inside = 0 <= col < 5 and 0 <= row < 5 and x >= margin and y >= margin
        return foreground if inside and cells[row][col] else background

    return png(size, size, pixel)


def photo(width=640, height=400):
    """A lake at sunset: sky, sun, two mountain ridges and their reflection."""
    horizon = int(height * 0.62)
    sun_x, sun_y, sun_r = width * 0.68, height * 0.40, height * 0.09

    def ridge(x, base, amplitude, phase):
        return base - amplitude * (0.6 * math.sin(x / width * 5.1 + phase)
                                   + 0.4 * math.sin(x / width * 11.3 + 2 * phase))

    def scene(x, y):
        if (x - sun_x) ** 2 + (y - sun_y) ** 2 <= sun_r ** 2:
            return (255, 236, 179)
        if y >= ridge(x, horizon - height * 0.04, height * 0.06, 1.7):
            return (58, 52, 92)
        if y >= ridge(x, horizon - height * 0.14, height * 0.10, 0.4):
            return (101, 84, 132)
        t = y / horizon
        return (round(255 - 70 * t), round(170 - 40 * t), round(120 + 60 * t))

    def pixel(x, y):
        if y < horizon:
            return scene(x, y)
        mirrored = 2 * horizon - y - 1
        r, g, b = scene(x + 3 * math.sin(y * 0.35), max(mirrored, 0))
        return (round(r * 0.72), round(g * 0.78), round(b * 0.9))

    return png(width, height, pixel)


def write_images(directory, script):
    os.makedirs(directory, exist_ok=True)
    for name in everyone(script) + [script["nick"]]:
        if name not in NO_AVATAR:
            with open(os.path.join(directory, f"avatar-{name}.png"), "wb") as file:
                file.write(avatar(name))
    with open(os.path.join(directory, PHOTO), "wb") as file:
        file.write(photo())


# ------------------------------------------------------------------ settings
def settings(script, port, language, theme, toolbar=True):
    return {
        "version": SETTINGS_VERSION,
        "language": language,
        "theme": theme,
        "linux_display": "x11",
        "credential_backend": "local_file",
        "selected_server": "demo",
        # The toolbar is on by default; saying so keeps the picture the same
        # if the default or an older saved file ever differs.
        "show_toolbar": toolbar,
        "appearance": {"image_previews": True, "compact_urls": True},
        "servers": [{
            "id": "demo",
            "user_avatars": True,
            "host": "127.0.0.1",
            "port": port,
            "use_tls": False,
            "verify_tls_certificates": True,
            "encoding": "utf8",
            "nickname": script["nick"],
            "username": script["nick"],
            "display_name": script["network"],
            "channels": ",".join(channel["name"] for channel in script["channels"]),
            "connect_on_startup": True,
            "ircv3": {"message_tags": True, "server_time": True, "batch": True},
        }],
    }


def layout(width, height):
    return {"version": 1, "window": {"x": 40, "y": 40, "width": width, "height": height},
            "maximized": False}


# -------------------------------------------------------------------- server
def timestamp(index):
    """Today at 20:30 local time plus a minute or two per line, as UTC."""
    start = datetime.datetime.now().astimezone().replace(hour=20, minute=30, second=0, microsecond=0)
    moment = start + datetime.timedelta(seconds=97 * index)
    return moment.astimezone(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.000Z")


class Client:
    def __init__(self, script, reader, writer, played):
        self.script = script
        self.reader = reader
        self.writer = writer
        self.played = played
        self.nick = script["nick"]
        self.caps = set()
        self.registered = False
        self.got = {"NICK": False, "USER": False, "CAP": False}
        self.joined = set()

    def send(self, line, tags=None):
        if tags and "server-time" in self.caps:
            line = "@" + ";".join(f"{key}={value}" for key, value in tags.items()) + " " + line
        self.writer.write((line + "\r\n").encode())

    def numeric(self, code, *params):
        *middle, last = params
        self.send(" ".join([f":{SERVER}", code, self.nick, *middle, ":" + last]))

    def prefix(self, nick):
        return f"{nick}!{nick}@{nick}.users.cayenchat.test"

    async def run(self):
        while True:
            raw = await self.reader.readline()
            if not raw:
                return
            line = raw.decode("utf-8", "replace").rstrip("\r\n")
            if line.startswith("@"):
                line = line.split(" ", 1)[1] if " " in line else ""
            words, _, trailing = line.partition(" :")
            parts = words.split()
            if not parts:
                continue
            command, args = parts[0].upper(), parts[1:] + ([trailing] if trailing else [])
            await self.handle(command, args)
            await self.writer.drain()

    async def handle(self, command, args):
        if command == "CAP":
            sub = args[0].upper() if args else ""
            if sub == "LS":
                self.got["CAP"] = True
                self.send(f":{SERVER} CAP * LS :server-time message-tags batch draft/metadata-2")
            elif sub == "REQ":
                wanted = args[-1].split()
                self.caps.update(wanted)
                self.send(f":{SERVER} CAP * ACK :{' '.join(wanted)}")
            elif sub == "END":
                self.got["CAP"] = False
                self.register()
        elif command == "NICK" and args:
            self.nick = args[0]
            self.got["NICK"] = True
            self.register()
        elif command == "USER":
            self.got["USER"] = True
            self.register()
        elif command == "PING":
            self.send(f":{SERVER} PONG {SERVER} :{args[-1] if args else ''}")
        elif command == "JOIN" and args:
            for name in args[0].split(","):
                self.join(name)
        elif command == "METADATA" and len(args) >= 2:
            self.metadata(args)
        elif command == "QUIT":
            self.writer.close()

    def register(self):
        if self.registered or not (self.got["NICK"] and self.got["USER"]) or self.got["CAP"]:
            return
        self.registered = True
        network = self.script["network"]
        self.numeric("001", f"Welcome to {network}, {self.prefix(self.nick)}")
        self.numeric("002", f"Your host is {SERVER}")
        self.numeric("003", "This server is a demo for screenshots")
        self.send(f":{SERVER} 004 {self.nick} {SERVER} demo iow ovbt")
        self.numeric("005", f"NETWORK={network}", "CASEMAPPING=rfc1459", "CHANTYPES=#",
                      "PREFIX=(ov)@+", "CHANMODES=b,k,l,nt", "are supported by this server")
        self.numeric("422", "MOTD File is missing")

    def metadata(self, args):
        target, sub = args[0], args[1].upper()
        if sub == "SUB":
            self.send(f":{SERVER} 770 {self.nick} avatar")
        elif sub == "GET" and target in ("*", self.nick):
            self.send(f":{SERVER} 761 {self.nick} {self.nick} avatar * "
                      f":{IMAGES}avatar-{self.nick}.png")

    def channel(self, name):
        return next((c for c in self.script["channels"] if c["name"].lower() == name.lower()), None)

    def join(self, name):
        channel = self.channel(name)
        if channel is None or name.lower() in self.joined:
            return
        self.joined.add(name.lower())
        # Dated like the conversation, so the log does not start with now.
        self.send(f":{self.prefix(self.nick)} JOIN {channel['name']}", {"time": timestamp(-2)})
        self.numeric("332", channel["name"], channel["topic"])
        names = " ".join(channel["members"] + [self.nick])
        self.send(f":{SERVER} 353 {self.nick} = {channel['name']} :{names}")
        self.numeric("366", channel["name"], "End of /NAMES list")
        if "draft/metadata-2" in self.caps:
            for member in channel["members"]:
                nick = bare(member)
                if nick not in NO_AVATAR:
                    self.send(f":{SERVER} METADATA {nick} avatar * :{IMAGES}avatar-{nick}.png")
        if len(self.joined) == len(self.script["channels"]):
            asyncio.get_running_loop().create_task(self.play())

    async def play(self):
        # The shown channel's lines come last so that its log is the newest,
        # and the other channels' lines leave unread marks.
        await asyncio.sleep(1.0)
        index = 0
        for channel in self.script["channels"][1:] + self.script["channels"][:1]:
            for nick, text in channel["lines"]:
                self.say(nick, channel["name"], text, index)
                index += 1
        for nick, text in self.script["private"]:
            self.say(nick, self.nick, text, index)
            index += 1
        await self.writer.drain()
        self.played.set()

    def say(self, nick, target, text, index):
        self.send(f":{self.prefix(nick)} PRIVMSG {target} :{text}", {"time": timestamp(index)})


async def serve(script, port, played, ready=None):
    async def accept(reader, writer):
        await Client(script, reader, writer, played).run()

    server = await asyncio.start_server(accept, "127.0.0.1", port)
    if ready:
        ready.set()
    async with server:
        await server.serve_forever()


# ------------------------------------------------------- build and provenance
ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
REQUIRED_PICTURES = ("cayenchat-en.png", "cayenchat-ja.png")
PROVENANCE = os.path.join(ROOT, "assets", "screenshots", "provenance.json")
# Ends the README check: more commits than this to the app since a picture.
README_MAX_BEHIND = 25


def git(*args):
    return subprocess.run(["git", "-C", ROOT, *args], capture_output=True, text=True,
                          check=True).stdout.strip()


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as file:
        for block in iter(lambda: file.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def executable_of(messages):
    """The executable cargo reports for the `cayenchat` binary in its JSON
    messages (wherever target-dir or build.target put it)."""
    found = None
    for line in messages.splitlines():
        try:
            message = json.loads(line)
        except ValueError:
            continue
        if (message.get("reason") == "compiler-artifact" and message.get("executable")
                and message.get("target", {}).get("name") == "cayenchat"
                and "bin" in message["target"].get("kind", [])):
            found = message["executable"]
    return found


def build_demo():
    """Builds the `demo` build of the checked-out sources (cargo decides what
    is out of date, from the file contents it tracks) and returns the
    executable cargo itself reports for that build."""
    result = subprocess.run(
        ["cargo", "build", "--release", "--locked", "-p", "cayenchat-ui", "--features", "demo",
         "--message-format=json-render-diagnostics"],
        cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True)
    built = executable_of(result.stdout)
    if not built or not os.path.isfile(built):
        sys.exit("cargo did not report the cayenchat executable of the demo build")
    return built


def stage(built):
    """A private copy of the build with the current `locales/` beside it. The
    app prefers catalogs next to the executable, in a bundle or installed
    under /usr/share over the ones compiled in, so the picture is taken from
    a copy whose neighbours are the current sources."""
    directory = os.path.join(ROOT, "target", "tmp", "demo-app")
    shutil.rmtree(directory, ignore_errors=True)
    os.makedirs(directory)
    staged = os.path.join(directory, os.path.basename(built))
    shutil.copy2(built, staged)
    shutil.copytree(os.path.join(ROOT, "locales"), os.path.join(directory, "locales"))
    return staged


def resolve_app(args):
    """The binary to run: a staged copy of the demo build made now. An --app
    is accepted only when it is byte for byte that build, so it has the current
    sources and the `demo` feature; nothing is guessed from times or strings.
    The file that runs is always the staged copy, not --app."""
    built = build_demo()
    if args.app and (not os.path.isfile(args.app) or sha256(args.app) != sha256(built)):
        sys.exit(f"refusing {args.app}: it is not identical to the demo build of the current "
                 f"sources ({built}); copy that file, or leave out --app")
    return stage(built)


def provenance(app, args, size):
    """What was verified: `app` is the demo build of HEAD (resolve_app)."""
    return {
        "commit": git("rev-parse", "HEAD"),
        "dirty": bool(git("status", "--porcelain", "--", ".", ":!assets/screenshots")),
        "features": ["demo"],
        "binary_sha256": sha256(app),
        "captured": datetime.datetime.now().astimezone().isoformat(timespec="seconds"),
        "os": f"{platform.system()} {platform.release()}",
        "language": args.language,
        "theme": args.theme,
        "size": size,
        "show_toolbar": True,
    }


# ------------------------------------------------------------------ commands
def script_for(args):
    return SCRIPTS[args.language]


def command_serve(args):
    script = script_for(args)
    write_images(args.images, script)
    played = threading.Event()
    print(f"demo server on 127.0.0.1:{args.port}", flush=True)
    try:
        asyncio.run(serve(script, args.port, played))
    except KeyboardInterrupt:
        pass


def command_settings(args):
    print(json.dumps(settings(script_for(args), args.port, args.language, args.theme), indent=2,
                     ensure_ascii=False))


def private_directory(path):
    os.makedirs(path, exist_ok=True)
    if os.name == "posix":
        os.chmod(path, 0o700)


def command_run(args):
    script = script_for(args)
    app_path = resolve_app(args)
    directory = os.path.abspath(args.dir or os.path.join(
        ROOT, "target", "tmp", f"screenshots-{args.language}-{args.theme}"))
    config = os.path.join(directory, "settings")
    images = os.path.join(directory, "images")
    private_directory(directory)
    private_directory(config)
    write_images(images, script)
    width, height = (int(value) for value in args.size.split("x"))
    with open(os.path.join(config, "settings.json"), "w", encoding="utf-8") as file:
        json.dump(settings(script, args.port, args.language, args.theme), file, indent=2,
                  ensure_ascii=False)
    with open(os.path.join(config, "window.json"), "w", encoding="utf-8") as file:
        json.dump(layout(width, height), file, indent=2)
    with open(os.path.join(directory, "provenance.json"), "w", encoding="utf-8") as file:
        json.dump(provenance(app_path, args, args.size), file, indent=2)
    print(f"provenance: {directory}/provenance.json", flush=True)

    played, ready = threading.Event(), threading.Event()
    thread = threading.Thread(target=lambda: asyncio.run(serve(script, args.port, played, ready)),
                              daemon=True)
    thread.start()
    if not ready.wait(10):
        sys.exit(f"the demo server could not listen on 127.0.0.1:{args.port}")

    env = dict(os.environ, CAYENCHAT_TEST_DIR=config, CAYENCHAT_PREVIEW_FIXTURE_DIR=images)
    log = open(os.path.join(directory, "app.log"), "w")
    app = subprocess.Popen([app_path], env=env, stdin=subprocess.DEVNULL, stdout=log,
                           stderr=subprocess.STDOUT)
    print(f"CayenChat started (pid {app.pid}); settings in {config}", flush=True)
    while not played.wait(1):
        if app.poll() is not None:
            sys.exit(f"CayenChat exited; see {directory}/app.log")
    print("The demo conversation has been played. Take the screenshots now.")
    if sys.platform == "darwin":
        print(f"  window ID: swift scripts/screenshots/window_id.swift {app.pid}")
        print("  then: screencapture -o -l <ID> shot.png")
    try:
        input("Press Enter to quit CayenChat. ")
    except (EOFError, KeyboardInterrupt):
        pass
    app.terminate()
    try:
        app.wait(10)
    except subprocess.TimeoutExpired:
        app.kill()


def capture(args, script, directory, toolbar, name):
    """One Xvfb run with gui_session.py: the settled window, as raw rows and
    as the PNG gui_session.py saved."""
    sys.path.insert(0, os.path.join(ROOT, "scripts", "e2e"))
    import gui_session

    images = os.path.join(directory, "images")
    write_images(images, script)
    settings_path = os.path.join(directory, f"settings-{name}.json")
    with open(settings_path, "w", encoding="utf-8") as file:
        json.dump(settings(script, args.port, args.language, args.theme, toolbar), file, indent=2,
                  ensure_ascii=False)
    session = os.path.join(directory, f"gui-{name}")
    env = dict(os.environ, CAYENCHAT_GUI_SESSION=session)
    tool = os.path.join(ROOT, "scripts", "e2e", "gui_session.py")
    width, height = args.size.split("x")

    def run(*command):
        return subprocess.run([sys.executable, tool, *command], env=env, check=True,
                              capture_output=True, text=True).stdout

    args.played.clear()
    run("start", "--app", args.app_path, "--settings", settings_path, "--display", args.display,
        "--screen", args.size, "--window-size", args.size,
        "--env", f"CAYENCHAT_PREVIEW_FIXTURE_DIR={images}")
    try:
        if not args.played.wait(60):
            sys.exit("the demo conversation was not played")
        # Off the menu bar, so no menu is highlighted.
        out = run("move", str(int(width) - 1), str(int(height) - 1), "--still", "2")
        png = out.strip().splitlines()[-1].split(" (")[0]
        rows = gui_session.display_for(gui_session.load(session)).grab()[2]
        return png, rows
    finally:
        run("stop")


def command_readme(args):
    script = script_for(args)
    args.app_path = resolve_app(args)
    directory = os.path.join(ROOT, "target", "tmp", f"readme-shot-{args.language}-{args.theme}")
    private_directory(directory)
    args.played, ready = threading.Event(), threading.Event()
    threading.Thread(target=lambda: asyncio.run(serve(script, args.port, args.played, ready)),
                     daemon=True).start()
    if not ready.wait(10):
        sys.exit(f"the demo server could not listen on 127.0.0.1:{args.port}")
    png, shown = capture(args, script, directory, True, "toolbar")
    # The same picture with the toolbar hidden: the top of the window must
    # differ, or the toolbar did not reach the screen.
    _, hidden = capture(args, script, directory, False, "no-toolbar")
    top = slice(0, 160)
    if shown[top] == hidden[top]:
        sys.exit("the toolbar is not on the screen: the top of the window is the same "
                 "with and without it; not using this picture")
    print("ok: the toolbar is rendered (the window differs from the toolbar-less one)")
    os.makedirs(os.path.dirname(args.out) or ".", exist_ok=True)
    shutil.copyfile(png, args.out)
    try:
        with open(PROVENANCE, encoding="utf-8") as file:
            recorded = json.load(file)
    except FileNotFoundError:
        recorded = {}
    entry = provenance(args.app_path, args, args.size)
    entry["image_sha256"] = sha256(args.out)
    recorded[os.path.basename(args.out)] = entry
    os.makedirs(os.path.dirname(PROVENANCE), exist_ok=True)
    with open(PROVENANCE, "w", encoding="utf-8") as file:
        json.dump(recorded, file, indent=2, sort_keys=True)
        file.write("\n")
    print(f"wrote {args.out} and its entry in {os.path.relpath(PROVENANCE, ROOT)}")


def command_check(args):
    """Fails when a recorded picture is older than --max-behind commits to
    the app's own crates."""
    with open(PROVENANCE, encoding="utf-8") as file:
        recorded = json.load(file)
    late = False
    for name in REQUIRED_PICTURES:
        if name not in recorded:
            print(f"{name}: no entry")
            late = True
    for name, entry in sorted(recorded.items()):
        try:
            behind = int(git("rev-list", "--count", f"{entry['commit']}..HEAD", "--",
                             "crates/ui", "crates/app"))
        except subprocess.CalledProcessError:
            behind = None
        note = "unknown commit" if behind is None else f"{behind} commits behind"
        if entry.get("dirty"):
            note += ", taken from uncommitted sources"
        path = os.path.join(os.path.dirname(PROVENANCE), name)
        if not os.path.isfile(path) or sha256(path) != entry.get("image_sha256"):
            note += ", the file is not the recorded picture"
            late = True
        print(f"{name}: {entry['commit'][:10]} {entry['captured']}, {note}")
        late |= behind is None or behind > args.max_behind or bool(entry.get("dirty"))
    if late:
        sys.exit(f"take the pictures again (demo.py readme); limit is {args.max_behind} commits")


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)

    def common(command, port=True):
        command.add_argument("--language", default="english", choices=sorted(SCRIPTS))
        if port:
            command.add_argument("--port", type=int, default=36690)

    def app_options(command):
        command.add_argument("--app", help="must be identical to the demo build made now "
                             "(default: that build, target/release/cayenchat)")

    command = commands.add_parser("run", help="start the server and a demo build, then wait")
    common(command)
    app_options(command)
    command.add_argument("--theme", default="light", choices=["light", "dark"])
    command.add_argument("--size", default="1280x800",
                         help="window size in logical pixels (restored from window.json)")
    command.add_argument("--dir", help="working directory (default target/tmp/screenshots-...)")
    command.set_defaults(func=command_run)

    command = commands.add_parser(
        "readme", help="build, take the README picture under Xvfb, check the toolbar is on it")
    common(command)
    app_options(command)
    command.add_argument("--theme", default="light", choices=["light", "dark"])
    command.add_argument("--size", default="1280x800", help="screen and window size")
    command.add_argument("--display", default=":98", help="Xvfb display")
    command.add_argument("--out", required=True, help="PNG to write, e.g. assets/screenshots/cayenchat-en.png")
    command.set_defaults(func=command_readme)

    command = commands.add_parser(
        "check", help="fail when the pictures in assets/screenshots lag behind the app")
    command.add_argument("--max-behind", type=int, default=README_MAX_BEHIND,
                         help="commits to crates/ui and crates/app allowed since a picture")
    command.set_defaults(func=command_check)

    command = commands.add_parser("serve", help="only run the demo server")
    common(command)
    command.add_argument("--images", required=True, help="directory to write the images to")
    command.set_defaults(func=command_serve)

    command = commands.add_parser("settings", help="print settings.json for the demo server")
    common(command)
    command.add_argument("--theme", default="light", choices=["light", "dark"])
    command.set_defaults(func=command_settings)

    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
