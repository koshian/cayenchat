#!/usr/bin/env python3
"""GUI end-to-end check of channel history: older pages on scroll and
reconnect gap recovery, with the real CayenChat binary, a real Ergo and real
X input events (spec/development.md, "GUI end-to-end test").

    scripts/e2e/history_gui.py --app target/debug/cayenchat --ergo-dir WORK_DIR

WORK_DIR is prepared by
`ERGO_SETUP_ONLY=1 ERGO_NO_FAKELAG=1 scripts/ergo-metadata-interop.sh WORK_DIR`.
Needs Linux with Xvfb, xdotool, xwd (x11-apps) and xclip, and a Vulkan driver
(lavapipe from mesa-vulkan-drivers works without a GPU). Nothing reaches the
network: Ergo listens on loopback and CayenChat connects through a local
proxy that this script uses to cut the link and to hold replies.

What is asserted, without any test hook in the application:

- on the wire (seen by the proxy): the CHATHISTORY commands CayenChat sends;
- on screen (pixels from the X server): the main log does not move when an
  older page is inserted above it;
- in the log's own text (drag-select and Ctrl+C, read with xclip): which
  lines are shown and in which order.
"""
import argparse, os, re, socket, struct, subprocess, sys, tempfile, threading, time, zlib

WIDTH, HEIGHT = 1000, 700
# Inside the main log pane (upper left) at this window size.
# The menu bar is shown all the time by default, which would move the panes
# down by 28 px and shorten the logs; the settings below hide it (as it was
# before) so these coordinates hold. History is what this script tests.
LOG_BOX = (0, 0, 560, 326)
LOG_POINT = (300, 150)
# Where message text starts in the main log: after the time and the 124 px
# nickname column.
TEXT_X = 185
CHANNEL = "#e2e"
LINES = 180


def log(*parts):
    print(time.strftime("%H:%M:%S"), *parts, flush=True)


class Failure(Exception):
    pass


def check(condition, message):
    if not condition:
        raise Failure(message)
    log("ok:", message)


# ---------------------------------------------------------------- X helpers
class Display:
    def __init__(self, env, out):
        self.env, self.out = env, out
        self.shots = 0

    def run(self, *args, **kw):
        return subprocess.run(args, env=self.env, **kw)

    def xdo(self, *args):
        self.run("xdotool", *args, check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def grab(self):
        """The screen as (width, height, rows of 32-bit pixels, little
        endian), compared as they are and converted only when saved."""
        data = self.run("xwd", "-root", "-silent", capture_output=True, check=True).stdout
        header = struct.unpack(">25I", data[:100])
        size, width, height = header[0], header[4], header[5]
        bpp, stride, ncolors, lsb = header[11], header[12], header[19], header[7] == 0
        if bpp != 32:
            raise Failure(f"unexpected xwd depth {bpp}")
        if not lsb:
            raise Failure("unexpected xwd byte order")
        start = size + ncolors * 12
        rows = [data[start + y * stride: start + y * stride + width * 4] for y in range(height)]
        return width, height, rows

    def crop(self, shot, box):
        x0, y0, x1, y1 = box
        return b"".join(row[x0 * 4: x1 * 4] for row in shot[2][y0:y1])

    def save(self, shot, name):
        width, height, rows = shot
        self.shots += 1
        path = os.path.join(self.out, f"{self.shots:02d}_{name}.png")

        def chunk(kind, body):
            return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))

        raw = bytearray()
        for row in rows:
            raw.append(0)
            rgb = bytearray(len(row) // 4 * 3)
            rgb[0::3], rgb[1::3], rgb[2::3] = row[2::4], row[1::4], row[0::4]
            raw += rgb
        png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        png += chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b"")
        with open(path, "wb") as file:
            file.write(png)
        return path

    def settle(self, name, timeout=15):
        """Waits until the screen stops changing, saves and returns it."""
        deadline = time.time() + timeout
        last = self.grab()
        while time.time() < deadline:
            time.sleep(0.4)
            now = self.grab()
            if now[2] == last[2]:
                self.save(now, name)
                return now
            last = now
        raise Failure(f"screen did not settle ({name})")

    def wheel(self, up, clicks):
        self.xdo("mousemove", *map(str, LOG_POINT))
        for _ in range(clicks):
            self.xdo("click", "4" if up else "5")
            time.sleep(0.04)

    def visible_text(self):
        """The main log's visible message bodies, through its own selection
        and copy: drag across the pane, Ctrl+C, read the clipboard."""
        self.run("xclip", "-selection", "clipboard", "-i", "/dev/null", check=False)
        x0, y0, x1, y1 = LOG_BOX
        # Press on the first row's text, drag to the end of the last one.
        self.xdo("mousemove", str(x0 + TEXT_X), str(y0 + 14))
        time.sleep(0.2)
        self.xdo("mousedown", "1")
        for step in range(1, 11):
            self.xdo("mousemove", str(x0 + TEXT_X + (x1 - x0 - TEXT_X - 5) * step // 10), str(y0 + 14 + (y1 - 8 - 14 - y0) * step // 10))
            time.sleep(0.08)
        self.xdo("mouseup", "1")
        time.sleep(0.3)
        self.save(self.grab(), "selection")
        self.xdo("key", "ctrl+c")
        time.sleep(0.5)
        text = self.run("xclip", "-selection", "clipboard", "-o", capture_output=True, text=True).stdout
        # A click clears the selection again.
        self.xdo("mousemove", *map(str, LOG_POINT))
        self.xdo("click", "1")
        return text.splitlines()


# -------------------------------------------------------------------- proxy
class Proxy:
    """Forwards CayenChat's connection to Ergo, records what it sends, can
    hold the server's answers after a CHATHISTORY BEFORE, and can cut and
    refuse the link."""

    def __init__(self, upstream, port=0):
        self.upstream = upstream
        self.sent = []
        self.accepting = True
        self.hold_before = False
        # Seconds each older page's answer is held back (by hand: to watch
        # a page arrive); 0 forwards it at once.
        self.page_delay = 0.0
        self.holding = False
        self.held = []
        self.links = []
        self.lock = threading.Lock()
        self.server = socket.socket()
        self.server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.server.bind(("127.0.0.1", port))
        self.server.listen()
        self.port = self.server.getsockname()[1]
        threading.Thread(target=self.accept, daemon=True).start()

    def accept(self):
        while True:
            client, _ = self.server.accept()
            if not self.accepting:
                client.close()
                continue
            server = socket.create_connection(self.upstream)
            with self.lock:
                self.links.append((client, server))
            threading.Thread(target=self.client_to_server, args=(client, server), daemon=True).start()
            threading.Thread(target=self.server_to_client, args=(server, client), daemon=True).start()

    def client_to_server(self, client, server):
        pending = b""
        try:
            while data := client.recv(4096):
                pending += data
                *lines, pending = pending.split(b"\r\n")
                for line in lines:
                    text = line.decode("utf-8", "replace")
                    with self.lock:
                        self.sent.append(text)
                        if text.startswith("CHATHISTORY BEFORE"):
                            if self.hold_before:
                                self.holding = True
                            elif self.page_delay > 0 and not self.holding:
                                self.holding = True
                                threading.Timer(self.page_delay, self.release).start()
                    server.sendall(line + b"\r\n")
        except OSError:
            pass

    def server_to_client(self, server, client):
        try:
            while data := server.recv(4096):
                with self.lock:
                    if self.holding:
                        self.held.append((client, data))
                        continue
                client.sendall(data)
        except OSError:
            pass

    def release(self):
        with self.lock:
            self.holding = False
            self.hold_before = False
            held, self.held = self.held, []
        for client, data in held:
            client.sendall(data)

    def cut(self):
        self.accepting = False
        with self.lock:
            links, self.links = self.links, []
        for pair in links:
            for sock in pair:
                try:
                    sock.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass

    def history_commands(self):
        with self.lock:
            return [line for line in self.sent if line.startswith("CHATHISTORY")]

    def wait_for(self, what, predicate, timeout=30):
        deadline = time.time() + timeout
        while time.time() < deadline:
            found = [line for line in self.history_commands() if predicate(line)]
            if found:
                return found[-1]
            time.sleep(0.1)
        raise Failure(f"CayenChat did not send {what}; sent {self.history_commands()}")


# --------------------------------------------------------------------- peer
class Peer:
    """Another user, speaking raw IRC to Ergo."""

    def __init__(self, port, nick):
        self.sock = socket.create_connection(("127.0.0.1", port))
        self.file = self.sock.makefile("r", encoding="utf-8", errors="replace")
        self.send(f"NICK {nick}")
        self.send(f"USER {nick} 0 * :{nick}")
        self.until(lambda line: " 376 " in line or " 422 " in line)

    def send(self, line):
        self.sock.sendall((line + "\r\n").encode())

    def until(self, predicate):
        for line in self.file:
            if line.startswith("PING"):
                self.send("PONG" + line[4:].rstrip())
            if predicate(line):
                return line
        raise Failure("peer connection closed")

    def say(self, text):
        self.send(f"PRIVMSG {CHANNEL} :{text}")
        tag = f"said{time.time_ns()}"
        self.send(f"PING :{tag}")
        self.until(lambda line: tag in line)


# --------------------------------------------------------------------- test
def older_pages(display, proxy):
    first = proxy.wait_for("recent history", lambda l: l.startswith(f"CHATHISTORY LATEST {CHANNEL}"))
    check(first == f"CHATHISTORY LATEST {CHANNEL} * 50", f"joining asks for the latest lines: {first!r}")
    display.settle("joined")
    check(not [l for l in proxy.history_commands() if "BEFORE" in l],
          "an open channel asks for nothing older until scrolled")

    # Scroll up until the first older page is asked for; hold its answer.
    proxy.hold_before = True
    for _ in range(120):
        display.wheel(True, 1)
        if proxy.holding:
            break
    check(proxy.holding, "scrolling to the top asks for an older page")
    before = display.settle("page_requested")
    befores = [l for l in proxy.history_commands() if "BEFORE" in l]
    check(len(befores) == 1 and " msgid=" in befores[0] and befores[0].endswith(" 50"),
          f"one BEFORE with a msgid reference and our limit: {befores}")
    proxy.release()
    time.sleep(1.5)
    after = display.settle("page_inserted")
    check(display.crop(before, LOG_BOX) == display.crop(after, LOG_BOX),
          "the rows on screen stay exactly in place when the page is inserted above them")

    # Keep scrolling to the beginning of the server's history.
    quiet = 0
    while quiet < 60:
        count = len(proxy.history_commands())
        display.wheel(True, 5)
        time.sleep(0.3)
        quiet = quiet + 5 if len(proxy.history_commands()) == count else 0
    befores = [l for l in proxy.history_commands() if "BEFORE" in l]
    references = [l.split()[3] for l in befores]
    check(len(befores) >= 3 and len(set(references)) == len(references),
          f"each page refers to a new oldest line: {befores}")
    display.settle("beginning")
    text = display.visible_text()
    check("old line 001" in text, f"the first line of the channel is reached: {text[:6]}")
    display.wheel(True, 40)
    time.sleep(1)
    check(len([l for l in proxy.history_commands() if "BEFORE" in l]) == len(befores),
          "no request is repeated once the beginning is reached")


def reconnect_gap(display, proxy, peer):
    display.wheel(False, 400)
    peer.say("A last before the cut")
    display.settle("before_cut")
    proxy.cut()
    log("link cut")
    peer.say("B sent while away")
    peer.say("C also missed")
    time.sleep(1)
    display.settle("disconnected")
    proxy.accepting = True
    resumed = proxy.wait_for("gap recovery",
                             lambda l: l.startswith(f"CHATHISTORY LATEST {CHANNEL} msgid="), timeout=40)
    check(resumed.endswith(" 50"), f"the reconnect asks only for what came after the cut: {resumed!r}")
    time.sleep(2)
    peer.say("D live after the reconnect")
    display.settle("recovered")
    text = display.visible_text()
    wanted = ["A last before the cut", "B sent while away", "C also missed", "D live after the reconnect"]
    for line in wanted:
        check(text.count(line) == 1, f"{line!r} is shown once")
    positions = [text.index(line) for line in wanted]
    check(positions == sorted(positions), f"missed lines sit between A and D: {text[-10:]}")
    rejoined = [i for i, line in enumerate(text) if line.startswith("alice has joined")]
    check(len(rejoined) == 1 and positions[2] < rejoined[0] < positions[3],
          f"missed lines go where the log was cut off, before our rejoin: {text[-10:]}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--app", required=True)
    parser.add_argument("--ergo-dir", required=True)
    parser.add_argument("--out", default=None, help="screenshots and logs")
    parser.add_argument("--display", default=":99")
    args = parser.parse_args()
    out = args.out or tempfile.mkdtemp(prefix="cayenchat-e2e-")
    os.makedirs(out, exist_ok=True)
    home = tempfile.mkdtemp(prefix="cayenchat-e2e-home-")
    processes = []
    try:
        run = os.path.join(args.ergo_dir, "run")
        port = int(re.search(r'"127\.0\.0\.1:(\d+)":', open(os.path.join(run, "ircd.yaml")).read()).group(1))
        ergo = subprocess.Popen([os.path.join(args.ergo_dir, "ergo"), "run", "--conf", "ircd.yaml"], cwd=run,
                                stdout=open(os.path.join(out, "ergo.log"), "w"), stderr=subprocess.STDOUT)
        processes.append(ergo)
        time.sleep(2)
        peer = Peer(port, "bob")
        peer.send(f"JOIN {CHANNEL}")
        peer.until(lambda line: " 366 " in line)
        for index in range(1, LINES + 1):
            wrap = " — a longer line that wraps onto a second row in the log" * 3 if index % 7 == 0 else ""
            peer.send(f"PRIVMSG {CHANNEL} :old line {index:03d}{wrap}")
        peer.say("filled")
        log(f"{LINES} lines stored")
        proxy = Proxy(("127.0.0.1", port))

        config = os.path.join(home, "config")
        os.makedirs(os.path.join(config, "CayenChat"))
        with open(os.path.join(config, "CayenChat", "settings.json"), "w") as file:
            file.write("""{
  "version": 15, "selected_server": "e2e", "language": "english", "theme": "light",
  "linux_display": "x11", "credential_backend": "local_file",
  "menu_bar_auto_hide": true,
  "servers": [{"id": "e2e", "host": "127.0.0.1", "port": %d, "use_tls": false,
    "verify_tls_certificates": true, "encoding": "utf8", "nickname": "alice",
    "username": "alice", "channels": "%s", "connect_on_startup": true,
    "ircv3": {"message_tags": true, "server_time": true, "batch": true,
              "peer_avatars": false, "chathistory": true}}]
}""" % (proxy.port, CHANNEL))
        env = dict(os.environ, DISPLAY=args.display, HOME=home, XDG_CONFIG_HOME=config,
                   XDG_DATA_HOME=os.path.join(home, "data"), CAYENCHAT_DISPLAY="x11", RUST_LOG="warn")
        env.pop("WAYLAND_DISPLAY", None)
        xvfb = subprocess.Popen(["Xvfb", args.display, "-screen", "0", f"{WIDTH}x{HEIGHT}x24", "-nolisten", "tcp"],
                                stderr=subprocess.DEVNULL)
        processes.append(xvfb)
        time.sleep(1)
        display = Display(env, out)
        app = subprocess.Popen([args.app], env=env, stdout=open(os.path.join(out, "app.log"), "w"),
                               stderr=subprocess.STDOUT)
        processes.append(app)
        deadline = time.time() + 60
        window = []
        while not window and time.time() < deadline:
            time.sleep(1)
            window = display.run("xdotool", "search", "--name", "CayenChat", capture_output=True,
                                 text=True).stdout.split()
        check(bool(window), "the chat window opened")
        display.xdo("windowmove", window[0], "0", "0")
        display.xdo("windowsize", window[0], str(WIDTH), str(HEIGHT))

        older_pages(display, proxy)
        reconnect_gap(display, proxy, peer)
        check(app.poll() is None, "CayenChat is still running")
        log(f"passed; screenshots in {out}")
        return 0
    except Failure as failure:
        log("FAILED:", failure)
        log(f"screenshots and logs in {out}")
        return 1
    finally:
        for process in reversed(processes):
            process.terminate()
        for process in processes:
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()


if __name__ == "__main__":
    sys.exit(main())
