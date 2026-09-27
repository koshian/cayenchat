#!/usr/bin/env python3
"""Runs the CayenChat resource baseline against the local load fixture.

Launches the release binary with an isolated HOME (so the user's settings and
credential store are never read or written), connects it to
irc_load_server.py on 127.0.0.1 and samples the process in each scenario:

  S1 idle-unconnected   startup, settings window open, no connection
  S2 idle-connected     registered, CHANNELS joined with MEMBERS each, no traffic
  S3 idle-history       after HISTORY lines per channel (the per-channel cap)
  S4a flood-steady      FLOOD_RATE lines/s across all channels
  S4b flood-max         as fast as the client reads
  S5 idle-after-cap     idle after the logs and diagnostics were saturated
  S6 idle-after-cap-2   idle after a second maximum flood (plateau check)

On macOS every sample also records whether part of the window is on screen
(window_visibility.swift). GPUI stops drawing a fully hidden window, so only
scenarios with "visible 1.0" include drawing cost; keep the window visible on
the current Space and do not use the machine during a run.

Input and channel switching are not driven here; see the ignored
`perf_baseline` UI test (spec/performance.md).

  python3 scripts/perf/run_baseline.py --runs 3 --out target/perf/baseline.json
  python3 scripts/perf/run_baseline.py --load-only   # S2-S4b only
  python3 scripts/perf/run_baseline.py --summarize target/perf/baseline.json
"""

import argparse
import json
import os
import platform
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import sample_process  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]


def settings(port, channels, connect):
    return {
        "version": 11,
        "selected_server": "custom-perf",
        "servers": [
            {
                "id": "custom-perf",
                "host": "127.0.0.1",
                "port": port,
                "use_tls": False,
                "verify_tls_certificates": True,
                "encoding": "utf8",
                "custom": True,
                "remember_passwords": False,
            }
        ],
        "nickname": "perfclient",
        "username": "perf",
        "channels": ",".join(channels),
        "connect_on_startup": connect,
        "language": "english",
        "theme": "light",
        "credential_backend": "local_file",
    }


def free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def config_dir(home):
    if platform.system() == "Darwin":
        return home / "Library" / "Application Support" / "CayenChat"
    return home / ".config" / "CayenChat"


class Control:
    def __init__(self, port):
        deadline = time.monotonic() + 10
        while True:
            try:
                self.sock = socket.create_connection(("127.0.0.1", port))
                break
            except ConnectionRefusedError:
                if time.monotonic() > deadline:
                    raise
                time.sleep(0.1)
        self.file = self.sock.makefile("rw")

    def call(self, command, timeout=600):
        self.sock.settimeout(timeout)
        self.file.write(command + "\n")
        self.file.flush()
        return self.file.readline().strip()


def launch(binary, home):
    env = dict(os.environ, HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"))
    env.pop("RUST_LOG", None)
    return subprocess.Popen(
        [str(binary)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
    )


def stop(process):
    if process.poll() is None:
        process.send_signal(signal.SIGTERM)
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def visibility_probe(pid):
    """Returns a callable reporting the window's visible fraction, or None."""
    helper = ROOT / "target" / "perf" / "window_visibility"
    source = Path(__file__).with_name("window_visibility.swift")
    if platform.system() != "Darwin":
        return None
    if not helper.exists() or helper.stat().st_mtime < source.stat().st_mtime:
        helper.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(["swiftc", "-O", str(source), "-o", str(helper)], check=True)

    def probe():
        output = subprocess.run([str(helper), str(pid)], capture_output=True, text=True).stdout
        try:
            state = json.loads(output)
        except ValueError:
            return None
        return state["visible_fraction"] if state["onscreen"] else 0.0

    return probe


def measure(label, pid, seconds, results):
    summary = sample_process.window(pid, seconds, visibility=visibility_probe(pid))
    if summary is None:
        raise RuntimeError(f"CayenChat exited during {label}")
    results[label] = summary
    fp = summary["footprint"] or summary["rss"]
    print(
        f"  {label:18} cpu {summary['cpu_percent']:6.2f}%  "
        f"wakeups/s {summary['idle_wakeups_per_s']}  "
        f"threads {summary['threads']['max'] if summary['threads'] else '-'}  "
        f"rss {summary['rss']['last'] / 2**20:7.1f} MiB  "
        f"footprint {fp['last'] / 2**20:7.1f} MiB  "
        f"window visible {summary['window_visible_share']}",
        flush=True,
    )


def one_run(args, binary, index):
    results = {}
    phases = {}
    base = Path(tempfile.mkdtemp(prefix="cayenchat-perf-"))
    channels = [f"#perf{i:02d}" for i in range(args.channels)]
    try:
        # S1: no connection. The settings window opens at startup.
        if not args.load_only:
            home = base / "unconnected"
            config_dir(home).mkdir(parents=True)
            (config_dir(home) / "settings.json").write_text(
                json.dumps(settings(args.port, channels, False))
            )
            app = launch(binary, home)
            try:
                time.sleep(args.settle)
                measure("S1_idle_unconnected", app.pid, args.window, results)
            finally:
                stop(app)

        home = base / "connected"
        config_dir(home).mkdir(parents=True)
        (config_dir(home) / "settings.json").write_text(
            json.dumps(settings(args.port, channels, True))
        )
        server = subprocess.Popen(
            [
                sys.executable,
                str(Path(__file__).with_name("irc_load_server.py")),
                "--port",
                str(args.port),
                "--control-port",
                str(args.control_port),
                "--members",
                str(args.members),
                "--expect-channels",
                str(args.channels),
                "--seed",
                str(index + 1),
            ],
            stderr=subprocess.DEVNULL if not args.verbose else None,
        )
        control = Control(args.control_port)
        app = launch(binary, home)
        try:
            started = time.monotonic()
            control.call("joined", timeout=60)
            phases["joined_after_launch_s"] = round(time.monotonic() - started, 2)
            time.sleep(args.settle)
            measure("S2_idle_connected", app.pid, args.window, results)

            control.call(f"history {args.history}")
            control.call("wait")
            time.sleep(args.settle)
            measure("S3_idle_history", app.pid, args.window, results)

            control.call(f"flood {args.flood_rate} {args.flood_seconds}")
            measure("S4a_flood_steady", app.pid, args.flood_seconds, results)
            control.call("wait")

            control.call(f"flood 0 {args.flood_seconds}")
            measure("S4b_flood_max", app.pid, args.flood_seconds, results)
            control.call("wait")
            if not args.load_only:
                time.sleep(args.settle)
                measure("S5_idle_after_cap", app.pid, args.window, results)

                control.call(f"flood 0 {args.flood_seconds}")
                control.call("wait")
                time.sleep(args.settle)
                measure("S6_idle_after_cap_2", app.pid, args.window, results)
            phases["server"] = json.loads(control.call("stats"))
            if phases["server"]["connections"] != 1:
                print("  WARNING: the client reconnected; this run is not comparable", flush=True)
            if app.poll() is not None:
                raise RuntimeError("CayenChat exited")
        finally:
            stop(app)
            try:
                control.call("quit", timeout=5)
            except OSError:
                pass
            server.wait(timeout=10)
    finally:
        shutil.rmtree(base, ignore_errors=True)
    return {"scenarios": results, "fixture": phases}


def summarize(report):
    """Prints the median and range across runs for each scenario."""
    runs = report["runs"]
    print("scenario             cpu% median [min-max]    wakeups/s   threads  "
          "footprint MiB median [min-max]  rss MiB median   visible runs")
    for name in runs[0]["scenarios"]:
        rows = [run["scenarios"][name] for run in runs if name in run["scenarios"]]

        def spread(values, scale=1.0, digits=2):
            values = sorted(v / scale for v in values if v is not None)
            if not values:
                return "-"
            mid = values[len(values) // 2]
            return f"{mid:.{digits}f} [{values[0]:.{digits}f}-{values[-1]:.{digits}f}]"

        print(
            f"{name:20} {spread([r['cpu_percent'] for r in rows]):24} "
            f"{spread([r['idle_wakeups_per_s'] for r in rows]):11} "
            f"{spread([r['threads']['max'] for r in rows if r['threads']], digits=0):8} "
            f"{spread([r['footprint']['last'] for r in rows if r['footprint']], 2**20, 1):31} "
            f"{spread([r['rss']['last'] for r in rows], 2**20, 1):22} "
            f"{sum(1 for r in rows if r.get('window_visible_share') == 1)}/{len(rows)}"
        )
    for index, run in enumerate(runs):
        fixture = run["fixture"]
        server = fixture.get("server", {})
        print(f"run {index + 1}: joined after {fixture.get('joined_after_launch_s')} s, "
              f"connections {server.get('connections')}")
        for phase in server.get("phases", []):
            rtt = phase.get("ping_rtt_ms", {})
            print(f"  {phase['phase']:8} lines {phase['lines']:>8} "
                  f"rate {phase.get('achieved_rate', '-'):>9} lines/s  "
                  f"drain lag {phase['drain_lag_s']} s  ping rtt median {rtt.get('median', '-')} ms "
                  f"max {rtt.get('max', '-')} ms")


def environment(binary):
    def run(*command):
        try:
            return subprocess.run(command, capture_output=True, text=True, cwd=ROOT).stdout.strip()
        except OSError:
            return None

    info = {
        "date": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "git": run("git", "rev-parse", "--short", "HEAD"),
        "git_dirty": bool(run("git", "status", "--porcelain", "--untracked-files=no")),
        "rustc": run("rustc", "--version"),
        "os": f"{platform.system()} {platform.release()}",
        "machine": platform.machine(),
        "binary": str(binary),
        "binary_bytes": binary.stat().st_size,
    }
    if platform.system() == "Darwin":
        info["os_version"] = run("sw_vers", "-productVersion")
        info["cpu"] = run("sysctl", "-n", "machdep.cpu.brand_string")
        info["memory_bytes"] = int(run("sysctl", "-n", "hw.memsize") or 0)
        info["cpus"] = int(run("sysctl", "-n", "hw.ncpu") or 0)
    return info


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--binary", default=str(ROOT / "target" / "release" / "cayenchat"))
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--channels", type=int, default=10)
    parser.add_argument("--members", type=int, default=50)
    parser.add_argument("--history", type=int, default=2000, help="lines per channel")
    parser.add_argument("--flood-rate", type=int, default=200, help="lines/s for S4a")
    parser.add_argument("--flood-seconds", type=float, default=30)
    parser.add_argument("--window", type=float, default=30, help="idle sampling seconds")
    parser.add_argument("--settle", type=float, default=10)
    parser.add_argument("--port", type=int, default=0, help="IRC port (0: pick a free one)")
    parser.add_argument("--control-port", type=int, default=0)
    parser.add_argument("--out", default=str(ROOT / "target" / "perf" / "baseline.json"))
    parser.add_argument("--verbose", action="store_true")
    parser.add_argument(
        "--load-only", action="store_true", help="run only S2-S4b (for a watched, visible window)"
    )
    parser.add_argument("--summarize", metavar="JSON", help="only summarize an earlier report")
    args = parser.parse_args()
    if args.summarize:
        summarize(json.loads(Path(args.summarize).read_text()))
        return

    args.port = args.port or free_port()
    args.control_port = args.control_port or free_port()
    binary = Path(args.binary)
    if not binary.exists():
        sys.exit(f"{binary} not found; run: cargo build --release --locked -p cayenchat-ui")
    report = {
        "environment": environment(binary),
        "parameters": {
            key: getattr(args, key)
            for key in [
                "channels", "members", "history", "flood_rate", "flood_seconds", "window", "settle",
                "load_only",
            ]
        },
        "runs": [],
    }
    for index in range(args.runs):
        print(f"run {index + 1}/{args.runs}", flush=True)
        report["runs"].append(one_run(args, binary, index))
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2, ensure_ascii=False))
    print(f"wrote {out}")
    summarize(report)


if __name__ == "__main__":
    main()
