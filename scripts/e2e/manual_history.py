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
import argparse, os, re, subprocess, sys, threading, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from history_gui import Peer, Proxy  # noqa: E402

HELP = """commands:
  say TEXT     bob says TEXT in the channel
  fill N       bob says N numbered lines
  cut [SECS]   cut CayenChat's link and refuse reconnects (for SECS, or until `up`)
  up           accept connections again
  sent         CHATHISTORY commands CayenChat has sent
  quit         stop Ergo and exit"""


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ergo-dir", required=True)
    parser.add_argument("--port", type=int, default=36668, help="where CayenChat connects")
    parser.add_argument("--channel", default="#demo")
    parser.add_argument("--lines", type=int, default=180, help="lines bob says first")
    args = parser.parse_args()

    run = os.path.join(args.ergo_dir, "run")
    config = open(os.path.join(run, "ircd.yaml")).read()
    ergo_port = int(re.search(r'"127\.0\.0\.1:(\d+)":', config).group(1))
    ergo = subprocess.Popen([os.path.join(args.ergo_dir, "ergo"), "run", "--conf", "ircd.yaml"], cwd=run,
                            stdout=open(os.path.join(run, "ergo.log"), "w"), stderr=subprocess.STDOUT)
    try:
        time.sleep(2)
        channel = args.channel
        bob = Peer(ergo_port, "bob")
        lock = threading.Lock()

        def say(text):
            with lock:
                bob.send(f"PRIVMSG {channel} :{text}")
                tag = f"said{time.time_ns()}"
                bob.send(f"PING :{tag}")
                bob.until(lambda line: tag in line)

        bob.send(f"JOIN {channel}")
        bob.until(lambda line: " 366 " in line)
        counter = 0

        def fill(count):
            nonlocal counter
            for _ in range(count):
                counter += 1
                wrap = " — a longer line that wraps onto a second row in the log" * 3 if counter % 7 == 0 else ""
                say(f"line {counter:03d}{wrap}")

        fill(args.lines)
        if "enabled: false" not in config.split("fakelag:", 1)[1][:200]:
            print("note: fakelag is on; filling was slow (use ERGO_NO_FAKELAG=1 when configuring)")
        proxy = Proxy(("127.0.0.1", ergo_port), args.port)
        print(f"Ergo on 127.0.0.1:{ergo_port}; {channel} has {args.lines} lines from bob.")
        print(f"Connect CayenChat to 127.0.0.1 port {proxy.port} (no TLS), history option on, channel {channel}.")
        print(HELP)
        # Keep answering the server's pings while waiting for commands.
        def keepalive():
            while ergo.poll() is None:
                time.sleep(60)
                with lock:
                    tag = f"alive{time.time_ns()}"
                    bob.send(f"PING :{tag}")
                    bob.until(lambda line: tag in line)
        threading.Thread(target=keepalive, daemon=True).start()

        for line in sys.stdin:
            command, _, rest = line.strip().partition(" ")
            if command == "say" and rest:
                say(rest)
            elif command == "fill":
                fill(int(rest or 10))
            elif command == "cut":
                proxy.cut()
                print("link cut; reconnects are refused")
                if rest:
                    def reopen(seconds=float(rest)):
                        time.sleep(seconds)
                        proxy.accepting = True
                        print("accepting connections again")
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
        ergo.terminate()
        ergo.wait(timeout=5)


if __name__ == "__main__":
    main()
