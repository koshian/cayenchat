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
import json, os, socket, struct, subprocess, sys, tempfile, threading, zlib
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

    def to_png(self):
        """8-bit RGB PNG, filter 0 on every row (what `from_png` reads)."""
        raw = b"".join(b"\0" + b"".join(bytes((r[i + 2], r[i + 1], r[i]))
                                          for i in range(0, len(r), 4)) for r in self.rows)

        def chunk(kind, data):
            body = kind + data
            return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

        return (b"\x89PNG\r\n\x1a\n"
                + chunk(b"IHDR", struct.pack(">IIBBBBB", self.width, self.height, 8, 2, 0, 0, 0))
                + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))

    @classmethod
    def from_png(cls, data):
        """Reads only what `to_png` writes; anything else is refused."""
        if data[:8] != b"\x89PNG\r\n\x1a\n":
            raise ValueError("not a PNG")
        pos, header, idat = 8, None, b""
        while pos + 8 <= len(data):
            size, kind = struct.unpack(">I4s", data[pos:pos + 8])
            body = data[pos + 8:pos + 8 + size]
            pos += 12 + size
            if kind == b"IHDR":
                header = struct.unpack(">IIBBBBB", body)
            elif kind == b"IDAT":
                idat += body
        if header is None or header[2:] != (8, 2, 0, 0, 0):
            raise ValueError("only 8-bit RGB, non-interlaced PNGs written by to_png are read")
        width, height = header[:2]
        raw = zlib.decompress(idat)
        stride = 1 + width * 3
        if len(raw) != stride * height:
            raise ValueError("unexpected PNG data size")
        rows = []
        for y in range(height):
            line = raw[y * stride:(y + 1) * stride]
            if line[0] != 0:
                raise ValueError("only PNG filter 0 is read")
            rows.append(b"".join(bytes((line[i + 2], line[i + 1], line[i], 255))
                                 for i in range(1, len(line), 3)))
        return cls(width, height, rows)

    def difference(self, other, tolerance=0):
        """(number of pixels that differ by more than `tolerance` in some
        channel, the first such (x, y) or None). A different size differs
        entirely."""
        if (self.width, self.height) != (other.width, other.height):
            return self.width * self.height, None
        count, first = 0, None
        for y, (a, b) in enumerate(zip(self.rows, other.rows)):
            if a == b:
                continue
            for x in range(self.width):
                if any(abs(a[x * 4 + c] - b[x * 4 + c]) > tolerance for c in range(3)):
                    count += 1
                    first = first or (x, y)
        return count, first

    def expect_same_as(self, golden, tolerance=0, what="the area"):
        count, first = self.difference(golden, tolerance)
        if count:
            raise PixelMismatch(f"{what}: {count} of {golden.width * golden.height} pixels differ "
                                f"from the reference, the first at {first}")

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
