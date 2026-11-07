#!/usr/bin/env python3
"""A small base for GUI checks that assert pixel values the specification
lets us compute (spec/development.md, "Verifying what is drawn"). It sits on
`gui_session.py`: a fixed fixture starts the real app on a virtual X display,
one pane's rectangle is cut out of the screen, and the colors in it are
compared with expected values. Never the whole screen: a pane's rectangle
does not move with changes elsewhere in the window.

    with FixtureSession(app, settings, lines=["hello"]) as session:
        pane = session.pane(LOG_PANE)
        pane.expect_uniform(0x3366CC, Rect(0, 400, 100, 100))

Needs what `gui_session.py` needs. The pure parts (`Pixels`, `Rect`,
`parse_color`) are unit tested without a display in `test_pixel_check.py`.
"""
import json, os, socket, subprocess, sys, tempfile, threading
from collections import Counter

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
SESSION = os.path.join(HERE, "gui_session.py")


class PixelMismatch(AssertionError):
    pass


def parse_color(text):
    """`#rrggbb` (or `rrggbb`, or an int) as an (r, g, b) tuple."""
    if isinstance(text, tuple):
        return text
    if isinstance(text, int):
        return (text >> 16) & 255, (text >> 8) & 255, text & 255
    text = text.lstrip("#")
    if len(text) != 6:
        raise ValueError(f"not a #rrggbb color: {text!r}")
    return int(text[0:2], 16), int(text[2:4], 16), int(text[4:6], 16)


def hex_color(rgb):
    return "#%02x%02x%02x" % rgb


class Rect:
    """Pixels from (x, y), width by height; coordinates are relative to
    whatever the rectangle is cut from."""

    def __init__(self, x, y, width, height):
        if width <= 0 or height <= 0:
            raise ValueError("an empty rectangle")
        self.x, self.y, self.width, self.height = x, y, width, height

    def box(self):
        return self.x, self.y, self.x + self.width, self.y + self.height

    def __repr__(self):
        return f"Rect({self.x}, {self.y}, {self.width}, {self.height})"


class Pixels:
    """A cut-out of the screen: rows of 32-bit little-endian pixels (BGRA in
    memory), as `Display.grab` returns them."""

    def __init__(self, width, height, rows):
        self.width, self.height, self.rows = width, height, rows

    @classmethod
    def from_rgb(cls, rows):
        """For tests: rows of (r, g, b) tuples."""
        return cls(len(rows[0]), len(rows),
                   [b"".join(bytes((b, g, r, 255)) for r, g, b in row) for row in rows])

    def rgb(self, x, y):
        row = self.rows[y]
        return row[x * 4 + 2], row[x * 4 + 1], row[x * 4]

    def cut(self, rect):
        x0, y0, x1, y1 = rect.box()
        if x0 < 0 or y0 < 0 or x1 > self.width or y1 > self.height:
            raise PixelMismatch(f"{rect} lies outside the {self.width}x{self.height} picture")
        return Pixels(rect.width, rect.height, [row[x0 * 4: x1 * 4] for row in self.rows[y0:y1]])

    def colors(self):
        """How many pixels there are of each (r, g, b)."""
        counts = Counter()
        for row in self.rows:
            counts.update(zip(row[2::4], row[1::4], row[0::4]))
        return counts

    def count(self, color, tolerance=0):
        want = parse_color(color)
        return sum(n for rgb, n in self.colors().items()
                   if all(abs(a - b) <= tolerance for a, b in zip(rgb, want)))

    def expect_uniform(self, color, tolerance=0, what="the area"):
        """Every pixel is `color`, give or take `tolerance` per channel."""
        want = parse_color(color)
        for rgb, n in self.colors().items():
            if any(abs(a - b) > tolerance for a, b in zip(rgb, want)):
                raise PixelMismatch(f"{what}: want all {hex_color(want)}, got {n} pixels of "
                                    f"{hex_color(rgb)} ({self.summary()})")

    def expect_present(self, color, tolerance=0, what="the area"):
        if not self.count(color, tolerance):
            raise PixelMismatch(f"{what}: no pixel of {hex_color(parse_color(color))} "
                                f"({self.summary()})")

    def expect_absent(self, color, tolerance=0, what="the area"):
        found = self.count(color, tolerance)
        if found:
            raise PixelMismatch(f"{what}: {found} pixels of {hex_color(parse_color(color))}, "
                                f"want none ({self.summary()})")

    def summary(self, top=4):
        total = self.width * self.height
        return ", ".join(f"{hex_color(rgb)} x{n}/{total}" for rgb, n in self.colors().most_common(top))


class FixtureSession:
    """The real app, started from a fixed fixture under Xvfb through
    `gui_session.py`: a loopback IRC server that registers the client, joins
    `channel`, and sends `lines` (from "bob") once, plus the settings the
    caller gives. The app's own HOME is in the session directory, so the
    user's settings are never touched."""

    def __init__(self, app, settings, lines=(), channel="#demo", window_size="1100x750",
                 directory=None):
        self.app, self.settings, self.lines, self.channel = app, dict(settings), list(lines), channel
        self.window_size = window_size
        self.scratch = tempfile.TemporaryDirectory(prefix="pixel-check-")
        self.directory = directory or os.path.join(self.scratch.name, "gui")
        self.listener = None

    def __enter__(self):
        self.listener = socket.socket()
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen(5)
        threading.Thread(target=self._accept, daemon=True).start()
        port = self.listener.getsockname()[1]
        settings = {
            "version": 15, "language": "english", "theme": "light", "linux_display": "x11",
            "show_toolbar": False, "credential_backend": "local_file", "selected_server": "local",
            "servers": [{
                "id": "local", "host": "127.0.0.1", "port": port, "use_tls": False,
                "verify_tls_certificates": True, "encoding": "utf8", "nickname": "alice",
                "username": "alice", "channels": self.channel, "connect_on_startup": True,
                "ircv3": {},
            }],
        }
        settings.update(self.settings)
        path = os.path.join(self.scratch.name, "settings.json")
        with open(path, "w") as file:
            json.dump(settings, file)
        self.run("start", "--app", self.app, "--settings", path, "--window-size", self.window_size)
        return self

    def __exit__(self, *exc):
        try:
            self.run("stop")
        finally:
            self.listener.close()
            self.scratch.cleanup()

    def run(self, *args):
        env = dict(os.environ, CAYENCHAT_GUI_SESSION=self.directory)
        result = subprocess.run([sys.executable, SESSION, *args], capture_output=True, text=True,
                                env=env, timeout=180)
        if result.returncode != 0:
            raise SystemExit(f"gui_session.py {' '.join(args)} failed: {result.stderr}")
        return result.stdout.strip()

    def screen(self, what="screen"):
        """The settled screen; the PNG is also saved under the session's shots/."""
        import gui_session
        state = gui_session.load(self.directory)
        display = gui_session.display_for(state)
        shot, still = gui_session.settle(display)
        if not still:
            raise PixelMismatch("the screen kept changing")
        display.save(shot, what)
        return Pixels(*shot)

    def pane(self, rect, what="screen"):
        """Only `rect` of the settled screen (the PNG saved is the whole screen,
        for a person looking at a failure)."""
        return self.screen(what).cut(rect)

    def _accept(self):
        while True:
            try:
                conn, _ = self.listener.accept()
            except OSError:
                return
            threading.Thread(target=self._client, args=(conn,), daemon=True).start()

    def _client(self, conn):
        file = conn.makefile("rw", newline="\n")

        def send(line):
            file.write(line + "\r\n")
            file.flush()

        nick = "me"
        try:
            for line in file:
                words = line.split()
                if not words:
                    continue
                if words[0] == "NICK":
                    nick = words[1]
                elif words[0] == "USER":
                    send(f":srv 001 {nick} :Welcome")
                    send(f":srv 376 {nick} :End")
                elif words[0] == "JOIN":
                    send(f":{nick}!u@h JOIN {words[1]}")
                    send(f":srv 353 {nick} = {words[1]} :{nick} bob")
                    send(f":srv 366 {nick} {words[1]} :End")
                    for text in self.lines:
                        send(f":bob!u@h PRIVMSG {words[1]} :{text}")
                elif words[0] == "PING":
                    send(f":srv PONG srv {words[1]}")
        except (OSError, ValueError):
            pass
