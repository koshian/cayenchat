#!/usr/bin/env python3
"""CPU and memory of the full-URL bubble path of shortened URLs (issue #232),
with a real release binary under Xvfb:

    scripts/perf/url_bubble_perf.py --app target/release/cayenchat [--rounds 4] [--cycles 15]

Reuses the loopback server and window layout of scripts/e2e/url_bubble_gui.py
(one message with two long URLs, shortened URLs on). The app first idles for
`--idle` seconds. Then, in each round, the pointer repeats `--cycles` times:
off the log, onto the first URL (bubble shown), onto the second URL (bubble
replaced; skipped with `--first-only`), off the log (bubble hidden). Each phase waits a fixed time so the
bubble is up when the pointer moves on. CPU is that of the app process over
the whole round (X server and driver excluded); RSS is taken at the end of
each round. The app is idle again for `--idle` seconds at the end.

Prints one JSON object. Run the same command for the parent and the branch
binary, alternating, to compare.
"""
import argparse, importlib.util, json, os, socket, tempfile, threading, time

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location(
    "url_bubble_gui", os.path.join(HERE, "..", "e2e", "url_bubble_gui.py"))
gui = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gui)
sampler = importlib.util.spec_from_file_location("sample_process", os.path.join(HERE, "sample_process.py"))
sample_process = importlib.util.module_from_spec(sampler)
sampler.loader.exec_module(sample_process)


def measure(pid, seconds):
    return sample_process.window(pid, seconds, interval=1.0)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--app", required=True)
    parser.add_argument("--session")
    parser.add_argument("--rounds", type=int, default=4)
    parser.add_argument("--cycles", type=int, default=15)
    parser.add_argument("--idle", type=float, default=15)
    parser.add_argument("--first-only", action="store_true",
                        help="skip the second URL: off, first URL, off (the same on builds without the replacing)")
    parser.add_argument("--dwell", type=float, default=1.5, help="seconds the pointer rests on each target")
    args = parser.parse_args()

    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen(5)
    threading.Thread(target=gui.serve, args=(listener,), daemon=True).start()

    scratch = tempfile.TemporaryDirectory(prefix="url-bubble-perf-")
    settings = gui.write_settings(os.path.join(scratch.name, "settings.json"), listener.getsockname()[1])
    session = gui.Session(args.session or os.path.join(scratch.name, "gui"))
    session.run("start", "--app", args.app, "--settings", settings, "--window-size", "1100x750")
    result = {"app": os.path.abspath(args.app), "cycles_per_round": args.cycles, "dwell_s": args.dwell,
              "first_only": args.first_only}
    try:
        with open(os.path.join(session.directory, "state.json")) as file:
            pid = json.load(file)["pids"]["app"]
        time.sleep(5)
        result["idle_before"] = measure(pid, args.idle)
        result["rounds"] = []
        for _ in range(args.rounds):
            before = sample_process.sample(pid)
            for _ in range(args.cycles):
                points = [gui.AWAY, gui.FIRST_AT, gui.AWAY] if args.first_only else \
                    [gui.AWAY, gui.FIRST_AT, gui.SECOND_OUTSIDE, gui.AWAY]
                for point in points:
                    session.move(point)
                    time.sleep(args.dwell)
            after = sample_process.sample(pid)
            wall = after["t"] - before["t"]
            result["rounds"].append({
                "cpu_percent": round(100 * (after["cpu_s"] - before["cpu_s"]) / wall, 2),
                "rss_mb": round(after["rss"] / 2**20, 1),
                "threads": after["threads"],
            })
        result["idle_after"] = measure(pid, args.idle)
    finally:
        session.run("stop")
        scratch.cleanup()
    print(json.dumps(result))


if __name__ == "__main__":
    main()
