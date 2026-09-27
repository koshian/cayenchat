#!/usr/bin/env python3
"""Samples memory, CPU time, thread count and idle wakeups of one process.

macOS reads `ps` (RSS, CPU time) and `top` (threads, idle wakeups,
phys_footprint as `MEM`). Linux reads /proc (RSS, CPU time, threads); idle
wakeups are not available there and are reported as null.

Usage as a script prints one JSON object per window:

  sample_process.py --pid PID --seconds 30 [--interval 1]

The summary reports CPU as a percentage of one core over the window, the
wakeup delta per second, and min/mean/max of the memory samples.
"""

import argparse
import json
import os
import platform
import re
import subprocess
import sys
import time

MAC = platform.system() == "Darwin"
UNITS = {"B": 1, "K": 1024, "M": 1024**2, "G": 1024**3}


def parse_cpu_time(text):
    """`ps` time: [[dd-]hh:]mm:ss.cc"""
    text = text.strip()
    days = 0
    if "-" in text:
        day, text = text.split("-", 1)
        days = int(day)
    parts = [float(part) for part in text.split(":")]
    seconds = 0.0
    for part in parts:
        seconds = seconds * 60 + part
    return days * 86400 + seconds


def parse_size(text):
    match = re.match(r"([\d.]+)([BKMG])", text.strip())
    if not match:
        return None
    return int(float(match.group(1)) * UNITS[match.group(2)])


def sample_mac(pid):
    ps = subprocess.run(
        ["ps", "-o", "rss=,time=", "-p", str(pid)], capture_output=True, text=True
    ).stdout.split()
    if len(ps) < 2:
        return None
    top = subprocess.run(
        ["top", "-l", "1", "-pid", str(pid), "-stats", "pid,th,idlew,mem"],
        capture_output=True,
        text=True,
    ).stdout.splitlines()
    threads = wakeups = footprint = None
    for line in top:
        fields = line.split()
        if fields and fields[0] == str(pid) and len(fields) >= 4:
            threads = int(re.match(r"\d+", fields[1]).group())
            wakeups = int(re.match(r"\d+", fields[2]).group())
            footprint = parse_size(fields[3])
    return {
        "t": time.monotonic(),
        "rss": int(ps[0]) * 1024,
        "cpu_s": parse_cpu_time(ps[1]),
        "threads": threads,
        "idle_wakeups": wakeups,
        "footprint": footprint,
    }


def sample_linux(pid):
    try:
        with open(f"/proc/{pid}/stat") as stat:
            fields = stat.read().rsplit(")", 1)[1].split()
        with open(f"/proc/{pid}/status") as status:
            info = dict(line.split(":", 1) for line in status if ":" in line)
    except FileNotFoundError:
        return None
    ticks = os.sysconf("SC_CLK_TCK")
    return {
        "t": time.monotonic(),
        "rss": int(info["VmRSS"].split()[0]) * 1024,
        "cpu_s": (int(fields[11]) + int(fields[12])) / ticks,
        "threads": int(info["Threads"]),
        "idle_wakeups": None,
        "footprint": None,
    }


def sample(pid):
    return sample_mac(pid) if MAC else sample_linux(pid)


def summarize(samples):
    first, last = samples[0], samples[-1]
    wall = last["t"] - first["t"]

    def stats(key):
        values = [s[key] for s in samples if s[key] is not None]
        if not values:
            return None
        return {
            "min": min(values),
            "mean": round(sum(values) / len(values)),
            "max": max(values),
            "last": values[-1],
        }

    wakeups = None
    if first["idle_wakeups"] is not None and last["idle_wakeups"] is not None and wall > 0:
        wakeups = round((last["idle_wakeups"] - first["idle_wakeups"]) / wall, 2)
    visibility = [s["visible"] for s in samples if s.get("visible") is not None]
    drawn = None
    if visibility:
        drawn = round(sum(1 for v in visibility if v > 0) / len(visibility), 2)
    return {
        "wall_s": round(wall, 2),
        # Share of samples in which part of the window was on screen, so GPUI
        # kept drawing it (None when not probed).
        "window_visible_share": drawn,
        "window_visible_fraction_min": min(visibility) if visibility else None,
        "samples": len(samples),
        "cpu_percent": round(100 * (last["cpu_s"] - first["cpu_s"]) / wall, 2) if wall else None,
        "cpu_s": round(last["cpu_s"] - first["cpu_s"], 2),
        "idle_wakeups_per_s": wakeups,
        "rss": stats("rss"),
        "footprint": stats("footprint"),
        "threads": stats("threads"),
    }


def window(pid, seconds, interval=1.0, visibility=None):
    """Samples for `seconds` and returns the summary, or None if the process ended.

    `visibility`, if given, returns the visible fraction of the window (0-1).
    """
    samples = []
    deadline = time.monotonic() + seconds
    while True:
        current = sample(pid)
        if current is None:
            return None
        if visibility is not None:
            current["visible"] = visibility()
        samples.append(current)
        if current["t"] >= deadline:
            break
        time.sleep(max(0.0, min(interval, deadline - time.monotonic())))
    return summarize(samples)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--pid", type=int, required=True)
    parser.add_argument("--seconds", type=float, default=30)
    parser.add_argument("--interval", type=float, default=1.0)
    args = parser.parse_args()
    result = window(args.pid, args.seconds, args.interval)
    if result is None:
        sys.exit(f"process {args.pid} is not running")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
