#!/usr/bin/env python3
"""Runs the CayenChat resource baseline against the local load fixture.

Launches the release binary with an isolated HOME (so the user's settings and
credential store are never read or written), connects it to
irc_load_server.py on 127.0.0.1 and samples the process in each scenario:

  S1 idle-unconnected   startup, settings window open, no connection
  S2 idle-connected     registered, CHANNELS joined with MEMBERS each, no traffic
  S3 idle-history       after HISTORY lines per channel (the per-channel cap)
  S4a flood-steady      FLOOD_RATE lines/s across the first server's channels;
                        other servers only answer a PING every second
  S4c flood-all-servers FLOOD_RATE split over every server (--servers > 1)
  S4b flood-max         as fast as the client reads
  S5 idle-after-cap     idle after the logs and diagnostics were saturated
  S6 idle-after-cap-2   idle after a second maximum flood (plateau check)
  S7 member-churn       only with --churn-rate: JOIN/PART lines of extra users
                        at that rate (use a large --members)

On macOS every sample also records whether part of the window is on screen
(window_visibility.swift). GPUI stops drawing a fully hidden window, so only
scenarios with "visible 1.0" include drawing cost; keep the window visible on
the current Space and do not use the machine during a run.

Input and channel switching are not driven here; see the ignored
`perf_baseline` UI test (spec/performance.md).

--previews turns image previews on and makes every --image-every-th fixture
line an image link. It needs a binary built with the `preview-fixture`
feature, which reads those links from generated local PNGs instead of the
network (the production HTTP policy never contacts 127.0.0.1):

  cargo build --release --locked -p cayenchat-ui --features preview-fixture

  python3 scripts/perf/run_baseline.py --runs 3 --out target/perf/baseline.json
  python3 scripts/perf/run_baseline.py --load-only   # S2-S4b only
  python3 scripts/perf/run_baseline.py --servers 4   # four fixture servers
  python3 scripts/perf/run_baseline.py --members 2000 --churn-rate 50   # + S7
  python3 scripts/perf/run_baseline.py --summarize target/perf/baseline.json
"""

import argparse
import json
import struct
import zlib
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


def png(width, height, rgb):
    """A solid-color 8-bit RGB PNG."""
    def chunk(tag, data):
        return (struct.pack(">I", len(data)) + tag + data
                + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF))
    row = b"\x00" + bytes(rgb) * width
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(row * height, 6))
            + chunk(b"IEND", b""))


# Photos and screenshots of common sizes; the largest is a 12 MP phone photo.
IMAGE_SIZES = [(1600, 1200), (3000, 2000), (800, 600), (4032, 3024)]


def preview_images(count):
    """Generates (once) the PNGs the preview fixture serves."""
    directory = ROOT / "target" / "perf" / "preview-images"
    directory.mkdir(parents=True, exist_ok=True)
    for index in range(count):
        path = directory / f"img{index:03d}.png"
        if not path.exists():
            width, height = IMAGE_SIZES[index % len(IMAGE_SIZES)]
            color = ((index * 53) % 256, (index * 97) % 256, (index * 29) % 256)
            path.write_bytes(png(width, height, color))
    return directory


def settings(ports, channels, connect, previews=False):
    """One server is written as version 11, which builds before and after
    multi-server support both read; several servers need version 12."""
    identity = {
        "nickname": "perfclient",
        "username": "perf",
        "channels": ",".join(channels),
        "connect_on_startup": connect,
    }
    servers = [
        {
            "id": f"custom-perf-{index}",
            "host": "127.0.0.1",
            "port": port,
            "use_tls": False,
            "verify_tls_certificates": True,
            "encoding": "utf8",
            "custom": True,
            "remember_passwords": False,
        }
        for index, port in enumerate(ports)
    ]
    common = {
        "selected_server": "custom-perf-0",
        "language": "english",
        "theme": "light",
        "credential_backend": "local_file",
    }
    if previews:
        # Older builds ignore the field; missing fields keep their defaults.
        common["appearance"] = {"image_previews": True}
    if len(ports) == 1:
        return {"version": 11, "servers": servers, **identity, **common}
    for server in servers:
        server.update(identity)
    return {"version": 12, "servers": servers, **common}


def free_ports(count):
    """Distinct free ports; the probes stay bound until all are chosen."""
    probes = [socket.socket() for _ in range(count)]
    try:
        for probe in probes:
            probe.bind(("127.0.0.1", 0))
        return [probe.getsockname()[1] for probe in probes]
    finally:
        for probe in probes:
            probe.close()


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


def launch(binary, home, image_dir=None):
    env = dict(os.environ, HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"))
    env.pop("RUST_LOG", None)
    env.pop("CAYENCHAT_PREVIEW_FIXTURE_DIR", None)
    if image_dir:
        env["CAYENCHAT_PREVIEW_FIXTURE_DIR"] = str(image_dir)
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


def start_fixture(args, port, control_port, seed):
    return subprocess.Popen(
        [
            sys.executable,
            str(Path(__file__).with_name("irc_load_server.py")),
            "--port",
            str(port),
            "--control-port",
            str(control_port),
            "--members",
            str(args.members),
            "--expect-channels",
            str(args.channels),
            "--seed",
            str(seed),
            "--image-every",
            str(args.image_every if args.previews else 0),
            "--image-links",
            str(args.image_links),
        ],
        stderr=subprocess.DEVNULL if not args.verbose else None,
    )


def call_all(controls, command, timeout=600):
    for control in controls:
        control.call(command, timeout=timeout)


def one_run(args, binary, index):
    results = {}
    phases = {}
    base = Path(tempfile.mkdtemp(prefix="cayenchat-perf-"))
    channels = [f"#perf{i:02d}" for i in range(args.channels)]
    ports = free_ports(2 * args.servers)
    ports, control_ports = ports[: args.servers], ports[args.servers :]
    try:
        # S1: no connection. The settings window opens at startup.
        if not args.load_only:
            home = base / "unconnected"
            config_dir(home).mkdir(parents=True)
            (config_dir(home) / "settings.json").write_text(
                json.dumps(settings(ports, channels, False, args.previews))
            )
            app = launch(binary, home, args.image_dir)
            try:
                time.sleep(args.settle)
                measure("S1_idle_unconnected", app.pid, args.window, results)
            finally:
                stop(app)

        home = base / "connected"
        config_dir(home).mkdir(parents=True)
        (config_dir(home) / "settings.json").write_text(
            json.dumps(settings(ports, channels, True, args.previews))
        )
        fixtures = [
            start_fixture(args, port, control, index * 16 + number + 1)
            for number, (port, control) in enumerate(zip(ports, control_ports))
        ]
        controls = [Control(port) for port in control_ports]
        busy, others = controls[0], controls[1:]
        app = launch(binary, home, args.image_dir)
        try:
            started = time.monotonic()
            call_all(controls, "joined", timeout=60)
            phases["joined_after_launch_s"] = round(time.monotonic() - started, 2)
            time.sleep(args.settle)
            measure("S2_idle_connected", app.pid, args.window, results)

            call_all(controls, f"history {args.history}")
            call_all(controls, "wait")
            time.sleep(args.settle)
            measure("S3_idle_history", app.pid, args.window, results)

            # One busy server; the others only answer a PING every second,
            # which shows whether their lines wait behind the busy one.
            busy.call(f"flood {args.flood_rate} {args.flood_seconds}")
            call_all(others, f"probe {args.flood_seconds}")
            measure("S4a_flood_steady", app.pid, args.flood_seconds, results)
            call_all(controls, "wait")

            if others:
                # The same total rate spread over every server.
                rate = max(1, args.flood_rate // len(controls))
                call_all(controls, f"flood {rate} {args.flood_seconds}")
                measure("S4c_flood_all_servers", app.pid, args.flood_seconds, results)
                call_all(controls, "wait")

            busy.call(f"flood 0 {args.flood_seconds}")
            call_all(others, f"probe {args.flood_seconds}")
            measure("S4b_flood_max", app.pid, args.flood_seconds, results)
            call_all(controls, "wait")
            if not args.load_only:
                time.sleep(args.settle)
                measure("S5_idle_after_cap", app.pid, args.window, results)

                busy.call(f"flood 0 {args.flood_seconds}")
                busy.call("wait")
                time.sleep(args.settle)
                measure("S6_idle_after_cap_2", app.pid, args.window, results)
            if args.churn_rate:
                # Member list changes: every JOIN or PART re-publishes the
                # channel's roster.
                busy.call(f"churn {args.churn_rate} {args.flood_seconds}")
                call_all(others, f"probe {args.flood_seconds}")
                measure("S7_member_churn", app.pid, args.flood_seconds, results)
                call_all(controls, "wait")
            phases["servers"] = [json.loads(control.call("stats")) for control in controls]
            if any(server["connections"] != 1 for server in phases["servers"]):
                print("  WARNING: the client reconnected; this run is not comparable", flush=True)
            if app.poll() is not None:
                raise RuntimeError("CayenChat exited")
        finally:
            stop(app)
            for control in controls:
                try:
                    control.call("quit", timeout=5)
                except OSError:
                    pass
            for fixture in fixtures:
                fixture.wait(timeout=10)
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
        # Reports before multi-server support had a single "server".
        servers = fixture.get("servers") or [fixture.get("server", {})]
        print(f"run {index + 1}: joined after {fixture.get('joined_after_launch_s')} s, "
              f"connections {[server.get('connections') for server in servers]}")
        for number, server in enumerate(servers):
            for phase in server.get("phases", []):
                rtt = phase.get("ping_rtt_ms", {})
                print(f"  server {number} {phase['phase']:8} lines {phase.get('lines', '-'):>8} "
                      f"rate {phase.get('achieved_rate', '-'):>9} lines/s  "
                      f"drain lag {phase.get('drain_lag_s', '-')} s  "
                      f"ping rtt median {rtt.get('median', '-')} ms max {rtt.get('max', '-')} ms")


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
    parser.add_argument("--churn-rate", type=int, default=0,
                        help="JOIN/PART lines/s for S7 (0: skip S7); use with a large --members")
    parser.add_argument("--window", type=float, default=30, help="idle sampling seconds")
    parser.add_argument("--settle", type=float, default=10)
    parser.add_argument("--servers", type=int, default=1, help="fixture servers to connect")
    parser.add_argument("--out", default=str(ROOT / "target" / "perf" / "baseline.json"))
    parser.add_argument("--verbose", action="store_true")
    parser.add_argument(
        "--load-only", action="store_true", help="run only S2-S4b (for a watched, visible window)"
    )
    parser.add_argument("--summarize", metavar="JSON", help="only summarize an earlier report")
    parser.add_argument("--previews", action="store_true",
                        help="image previews on, with image links (preview-fixture build)")
    parser.add_argument("--image-every", type=int, default=20)
    parser.add_argument("--image-links", type=int, default=120)
    args = parser.parse_args()
    args.image_dir = None
    if args.summarize:
        summarize(json.loads(Path(args.summarize).read_text()))
        return

    binary = Path(args.binary)
    if not binary.exists():
        sys.exit(f"{binary} not found; run: cargo build --release --locked -p cayenchat-ui")
    if args.previews:
        args.image_dir = preview_images(args.image_links)
    report = {
        "environment": environment(binary),
        "parameters": {
            key: getattr(args, key)
            for key in [
                "servers", "channels", "members", "history", "flood_rate", "flood_seconds",
                "churn_rate", "window", "settle", "load_only", "previews", "image_every",
                "image_links",
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
