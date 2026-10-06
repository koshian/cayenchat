#!/usr/bin/env python3
"""Checks that the app quits when its X server goes away (issue #196).

    scripts/e2e/xserver_loss.py --app target/debug/cayenchat [--fonts DIR]

Starts a session with `gui_session.py start`, kills only that session's Xvfb
and asserts that the app exits and that `app.log` stops growing. Before the
fix the app stayed at 100% CPU and wrote the same warning without end.
Needs Linux with Xvfb and xdotool, and a Vulkan driver (lavapipe).
"""
import argparse, json, os, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
GUI = os.path.join(HERE, "gui_session.py")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--app", required=True)
    parser.add_argument("--display", default=":97")
    parser.add_argument("--fonts", action="append", default=[])
    args = parser.parse_args()

    session = tempfile.mkdtemp(prefix="xserver-loss-", dir=os.environ.get("TMPDIR"))
    session = os.path.join(session, "session")
    gui = [sys.executable, GUI, "--session", session]
    start = gui + ["start", "--app", args.app, "--display", args.display]
    for font in args.fonts:
        start += ["--fonts", font]
    subprocess.run(start, check=True, stdout=subprocess.DEVNULL)
    try:
        with open(os.path.join(session, "state.json")) as file:
            pids = json.load(file)["pids"]
        os.killpg(pids["xvfb"], 15)

        deadline = time.time() + 15
        while time.time() < deadline and alive(pids["app"]):
            time.sleep(0.2)
        if alive(pids["app"]):
            sys.exit("FAIL: the app is still running after its X server went away")
        print("ok: the app exited after the X server was killed")

        log = os.path.join(session, "app.log")
        size = os.path.getsize(log)
        time.sleep(2)
        if os.path.getsize(log) != size:
            sys.exit("FAIL: app.log kept growing")
        print(f"ok: app.log stays at {size} bytes")
    finally:
        subprocess.run(gui + ["stop"], stdout=subprocess.DEVNULL)


def alive(pid):
    try:
        with open(f"/proc/{pid}/stat") as file:
            return file.read().rsplit(")", 1)[1].split()[0] != "Z"
    except OSError:
        return False


if __name__ == "__main__":
    main()
