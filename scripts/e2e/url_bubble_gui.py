#!/usr/bin/env python3
"""GUI end-to-end check of the full-URL bubble of shortened URLs (issue #232),
with the real CayenChat binary and real X input events:

    scripts/e2e/url_bubble_gui.py --app target/debug/cayenchat

A one-message IRC server on loopback sends two long URLs in one line. What is
asserted, through the URLs the app hands to the desktop (see gui_session.py):

- the transparent margin that bridges the bubble to the pointer, which lies
  over the log text, does not open the URL;
- clicking the visible bubble opens the URL it shows;
- moving from one URL to another, with the second inside the first bubble's
  margin or outside it, replaces the bubble, and it opens the second URL.

Positions are screen pixels of a 1100x750 window with the default font. The
bubble can take a while to appear on a pointer that stays still under Xvfb,
so each step waits for the bubble to be drawn (the screen below the line
changes) before it clicks.
"""
import argparse, hashlib, json, os, socket, subprocess, sys, tempfile, threading, time

HERE = os.path.dirname(os.path.abspath(__file__))
SESSION = os.path.join(HERE, "gui_session.py")
FIRST = "https://first.example/long/path/to/first/page"
SECOND = "https://second.example/another/long/path/to/second/page"
CHANNEL = "#demo"

FIRST_AT = (160, 351)  # on the first shortened URL
SECOND_IN_MARGIN = (400, 358)  # on the second one, under the first bubble's margin
SECOND_OUTSIDE = (500, 351)  # on the second one, above the first bubble
AWAY = (300, 200)
# Inside each bubble, away from the margin and the log text.
FIRST_BUBBLE = (250, 382)
SECOND_BUBBLE_FROM_MARGIN = (500, 390)
SECOND_BUBBLE_FROM_OUTSIDE = (600, 390)
# Below the pointer, where a bubble is drawn over the draft box.
BUBBLE_AREA = "170,386,700,396"


def log(*parts):
    print(time.strftime("%H:%M:%S"), *parts, flush=True)


def serve(listener):
    def client(conn):
        file = conn.makefile("rw", newline="\n")

        def send(line):
            file.write(line + "\r\n")
            file.flush()

        nick = "me"
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
                send(f":bob!u@h PRIVMSG {words[1]} :see {FIRST} and {SECOND} ok")
            elif words[0] == "PING":
                send(f":srv PONG srv {words[1]}")

    while True:
        conn, _ = listener.accept()
        threading.Thread(target=client, args=(conn,), daemon=True).start()


class Session:
    def __init__(self, directory):
        self.directory = directory

    def run(self, *args):
        env = dict(os.environ, CAYENCHAT_GUI_SESSION=self.directory)
        result = subprocess.run([sys.executable, SESSION, *args], capture_output=True, text=True,
                                env=env, timeout=180)
        if result.returncode != 0:
            raise SystemExit(f"gui_session.py {' '.join(args)} failed: {result.stderr}")
        return result.stdout.strip()

    def move(self, point):
        self.run("move", str(point[0]), str(point[1]), "--no-shot")

    def click(self, point):
        self.run("click", str(point[0]), str(point[1]), "--no-shot")

    def area(self):
        shot = self.run("shot", "area", "--crop", BUBBLE_AREA).splitlines()[-1]
        with open(shot, "rb") as file:
            return hashlib.sha256(file.read()).hexdigest()

    def opened(self):
        return self.run("opened").split()

    def wait_for_bubble(self, before, what):
        for _ in range(20):
            time.sleep(1)
            if self.area() != before:
                return
        raise SystemExit(f"FAIL: no bubble for {what}")


def check(name, got, want):
    if got != want:
        raise SystemExit(f"FAIL {name}: opened {got}, want {want}")
    log("ok", name)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--app", required=True)
    parser.add_argument("--session", help="an empty or new session directory (default: temporary)")
    args = parser.parse_args()

    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen(5)
    threading.Thread(target=serve, args=(listener,), daemon=True).start()
    port = listener.getsockname()[1]

    scratch = tempfile.TemporaryDirectory(prefix="url-bubble-")
    directory = args.session or os.path.join(scratch.name, "gui")
    settings = os.path.join(scratch.name, "settings.json")
    with open(settings, "w") as file:
        json.dump({
            "version": 15, "language": "english", "theme": "light", "linux_display": "x11",
            "credential_backend": "local_file", "selected_server": "local",
            "servers": [{
                "id": "local", "host": "127.0.0.1", "port": port, "use_tls": False,
                "verify_tls_certificates": True, "encoding": "utf8", "nickname": "alice",
                "username": "alice", "channels": CHANNEL, "connect_on_startup": True, "ircv3": {},
            }],
            "appearance": {"compact_urls": True},
        }, file)

    session = Session(directory)
    session.run("start", "--app", args.app, "--settings", settings, "--window-size", "1100x750")
    try:
        def hover(point, what):
            session.move(AWAY)
            time.sleep(1.5)
            session.opened()
            idle_here = session.area()
            session.move(point)
            session.wait_for_bubble(idle_here, what)

        hover(FIRST_AT, "the first URL")
        session.click((200, 358))
        check("the margin over the URL opens nothing", session.opened(), [])

        hover(FIRST_AT, "the first URL")
        session.click(FIRST_BUBBLE)
        check("the first bubble opens the first URL", session.opened(), [FIRST])

        hover(FIRST_AT, "the first URL")
        shown = session.area()
        session.move(SECOND_IN_MARGIN)
        session.wait_for_bubble(shown, "the second URL in the margin")
        session.click(SECOND_BUBBLE_FROM_MARGIN)
        check("the second URL in the margin replaces the bubble", session.opened(), [SECOND])

        hover(FIRST_AT, "the first URL")
        shown = session.area()
        session.move(SECOND_OUTSIDE)
        session.wait_for_bubble(shown, "the second URL outside the margin")
        session.click(SECOND_BUBBLE_FROM_OUTSIDE)
        check("the second URL outside the margin replaces the bubble", session.opened(), [SECOND])
    finally:
        session.run("stop")
        scratch.cleanup()
    log("all passed")


if __name__ == "__main__":
    main()
