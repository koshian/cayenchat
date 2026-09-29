#!/usr/bin/env python3
"""A local IRC playground for trying channel history by hand: starts the
pinned Ergo, has a user `bob` fill a channel, and puts a proxy in front of
Ergo that can cut CayenChat's link on command (HOW_TO_TEST.md, "Trying
channel history by hand"). Works on macOS and Linux; nothing leaves the
machine.

    ERGO_SETUP_ONLY=1 ERGO_NO_FAKELAG=1 scripts/ergo-metadata-interop.sh /tmp/cayenchat-ergo 36667
    python3 scripts/e2e/manual_history.py --ergo-dir /tmp/cayenchat-ergo

Then connect CayenChat to 127.0.0.1 on the proxy port (36668 by default)
with the IRCv3 tab's history option on, and type commands here.
"""
import argparse, os, re, socket, subprocess, sys, threading, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from history_gui import Proxy  # noqa: E402

HELP = """commands:
  say TEXT     bob says TEXT in the channel
  fill N       bob says N numbered lines
  delay SECS   hold each older page's answer for SECS (0: off)
  cut [SECS]   cut CayenChat's link and refuse reconnects (for SECS, or until `up`)
  up           accept connections again
  sent         CHATHISTORY commands CayenChat has sent
  quit         stop Ergo and exit"""


class Bob:
    """The other user. A reader thread answers the server's PINGs at once,
    so bob stays connected however long the playground idles, and bob
    connects again (and rejoins) if the link was lost anyway."""

    def __init__(self, port, channel):
        self.port, self.channel = port, channel
        self.lock = threading.Lock()
        self.closing = False
        self.connect()

    def connect(self):
        self.sock = socket.create_connection(("127.0.0.1", self.port))
        self.file = self.sock.makefile("r", encoding="utf-8", errors="replace")
        self.waiting = {}
        self.alive = True
        self.write("NICK bob")
        self.write("USER bob 0 * :bob")
        self.write(f"JOIN {self.channel}")
        for line in self.file:
            if line.startswith("PING"):
                self.write("PONG" + line[4:].rstrip())
            if " 366 " in line:
                break
        threading.Thread(target=self.read, daemon=True).start()

    def write(self, line):
        self.sock.sendall((line + "\r\n").encode())

    def read(self):
        reason = "closed by the server"
        try:
            for line in self.file:
                if line.startswith("PING"):
                    with self.lock:
                        self.write("PONG" + line[4:].rstrip())
                elif line.startswith("ERROR"):
                    reason = line.strip()
                for tag, event in list(self.waiting.items()):
                    if tag in line:
                        event.set()
        except OSError as error:
            reason = str(error)
        self.alive = False
        if not self.closing:
            print(f"bob lost the connection ({reason}); the next say/fill reconnects bob", flush=True)

    def send_all(self, lines):
        """Sends `lines` and waits until Ergo has handled them."""
        if not self.alive:
            self.connect()
            print("bob reconnected and rejoined", flush=True)
        tag = f"done{time.time_ns()}"
        event = threading.Event()
        self.waiting[tag] = event
        with self.lock:
            for line in lines:
                self.write(line)
            self.write(f"PING :{tag}")
        event.wait(10)
        del self.waiting[tag]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ergo-dir", required=True)
    parser.add_argument("--port", type=int, default=36668, help="where CayenChat connects")
    parser.add_argument("--channel", default="#demo")
    parser.add_argument("--lines", type=int, default=180, help="lines bob says first (Ergo keeps 2048)")
    parser.add_argument("--page-delay", type=float, default=0.0,
                        help="seconds each older page's answer is held back, to watch it arrive")
    args = parser.parse_args()

    run = os.path.join(args.ergo_dir, "run")
    config = open(os.path.join(run, "ircd.yaml")).read()
    ergo_port = int(re.search(r'"127\.0\.0\.1:(\d+)":', config).group(1))
    ergo_log = os.path.join(run, "ergo.log")
    ergo = subprocess.Popen([os.path.join(args.ergo_dir, "ergo"), "run", "--conf", "ircd.yaml"], cwd=run,
                            stdin=subprocess.DEVNULL, stdout=open(ergo_log, "w"), stderr=subprocess.STDOUT)
    bob = None
    try:
        time.sleep(2)
        channel = args.channel
        bob = Bob(ergo_port, channel)
        counter = 0

        def say(text):
            bob.send_all([f"PRIVMSG {channel} :{text}"])

        def fill(count):
            nonlocal counter
            lines = []
            for _ in range(count):
                counter += 1
                wrap = " — a longer line that wraps onto a second row in the log" * 3 if counter % 7 == 0 else ""
                lines.append(f"PRIVMSG {channel} :line {counter:03d}{wrap}")
            bob.send_all(lines)

        fill(args.lines)
        if "enabled: false" not in config.split("fakelag:", 1)[1][:200]:
            print("note: Ergo's fakelag is on, so bob's lines trickle in (configure with ERGO_NO_FAKELAG=1)")
        proxy = Proxy(("127.0.0.1", ergo_port), args.port)
        proxy.page_delay = args.page_delay
        print(f"Ergo on 127.0.0.1:{ergo_port}; {channel} has {args.lines} lines from bob.")
        print(f"Connect CayenChat to 127.0.0.1 port {proxy.port} (no TLS), history option on, channel {channel}.")
        print(f"Ergo's log: {ergo_log}")
        print(HELP)

        for line in sys.stdin:
            command, _, rest = line.strip().partition(" ")
            if command == "say" and rest:
                say(rest)
            elif command == "fill":
                fill(int(rest or 10))
            elif command == "delay":
                proxy.page_delay = float(rest or 0)
                print(f"older pages arrive {proxy.page_delay:g} s late")
            elif command == "cut":
                proxy.cut()
                print("link cut; reconnects are refused")
                if rest:
                    def reopen(seconds=float(rest)):
                        time.sleep(seconds)
                        proxy.accepting = True
                        print("accepting connections again", flush=True)
                    threading.Thread(target=reopen, daemon=True).start()
            elif command == "up":
                proxy.accepting = True
                print("accepting connections again")
            elif command == "sent":
                for sent in proxy.history_commands():
                    print(" ", sent)
            elif command in ("quit", "exit"):
                break
            elif command:
                print(HELP)
    finally:
        if bob:
            bob.closing = True
        ergo.terminate()
        ergo.wait(timeout=5)


if __name__ == "__main__":
    main()
