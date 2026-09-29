#!/usr/bin/env python3
"""Lists every test in the workspace with its file, line, last result and
IRCv3 area, next to each file's line coverage.

    scripts/test-inventory.py [--log RUN.log] [--summary SUMMARY.json] [--out tests.html]

Tests are found in the sources (`#[test]`, `#[gpui::test]`,
`#[tokio::test]`), results are read from a `cargo test` / `cargo llvm-cov`
run log, and coverage from `cargo llvm-cov report --json --summary-only`.
Every input is optional; without --out a text summary is printed only.
`scripts/coverage.sh` runs everything and calls this.
"""

import argparse
import html
import json
import os
import re
import sys
from collections import Counter, defaultdict

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# IRCv3 areas by file, then by words in the test name (first match wins).
# Only a navigation aid: the name of a test says what it checks.
AREA_BY_FILE = {
    "crates/irc-core/src/cap.rs": "CAP negotiation / SASL",
    "crates/irc-core/src/tags.rs": "message-tags / server-time / msgid",
    "crates/irc-core/src/replay.rs": "batch",
    "crates/irc-core/src/history.rs": "chathistory",
    "crates/irc-core/src/metadata.rs": "metadata-2",
    "crates/irc-core/tests/metadata_interop.rs": "metadata-2",
    "crates/irc-core/tests/chathistory_interop.rs": "chathistory",
    "crates/app/src/timeline.rs": "msgid (dedupe)",
}
AREA_BY_NAME = [
    ("chathistory", "chathistory"),
    ("history", "chathistory"),
    ("older_page", "chathistory"),
    ("metadata", "metadata-2"),
    ("sasl", "CAP negotiation / SASL"),
    ("server_time", "message-tags / server-time / msgid"),
    ("msgid", "message-tags / server-time / msgid"),
    ("message_id", "message-tags / server-time / msgid"),
    ("tag", "message-tags / server-time / msgid"),
    ("batch", "batch"),
    ("ircv3", "IRCv3 settings"),
    ("utf8", "UTF8ONLY"),
    ("cap_", "CAP negotiation / SASL"),
]
IRCV3_CRATES = {"irc-core", "app", "ui", "storage", "model"}

TEST_ATTR = re.compile(r"^\s*#\[(?:gpui::|tokio::)?test\b")
IGNORE_ATTR = re.compile(r"^\s*#\[ignore")
FN = re.compile(r"^\s*(?:pub\s+)?(?:async\s+)?fn\s+([A-Za-z0-9_]+)")
RUNNING = re.compile(r"^\s*Running (?:unittests )?(\S+) \((\S+)\)")
RESULT = re.compile(r"^test (\S+) \.\.\. (ok|ignored|FAILED)")


def find_tests():
    tests = []
    crates_dir = os.path.join(ROOT, "crates")
    for crate in sorted(os.listdir(crates_dir)):
        for base, _, files in os.walk(os.path.join(crates_dir, crate)):
            for name in sorted(files):
                if not name.endswith(".rs"):
                    continue
                path = os.path.join(base, name)
                rel = os.path.relpath(path, ROOT)
                with open(path, encoding="utf-8") as source:
                    lines = source.read().split("\n")
                for index, line in enumerate(lines):
                    if not TEST_ATTR.match(line):
                        continue
                    ignored = False
                    for offset in range(1, 8):
                        if index + offset >= len(lines):
                            break
                        following = lines[index + offset]
                        if IGNORE_ATTR.match(following):
                            ignored = True
                        match = FN.match(following)
                        if match:
                            tests.append(
                                {
                                    "crate": crate,
                                    "file": rel,
                                    "line": index + offset + 1,
                                    "name": match.group(1),
                                    "declared_ignored": ignored,
                                    "result": None,
                                    "path": None,
                                }
                            )
                            break
    return tests


def binary_crate(target, source):
    """The crate directory a test binary belongs to."""
    if source.startswith("tests/"):
        stem = os.path.splitext(os.path.basename(source))[0]
        for crate in os.listdir(os.path.join(ROOT, "crates")):
            if os.path.exists(os.path.join(ROOT, "crates", crate, "tests", stem + ".rs")):
                return crate
        return None
    binary = os.path.basename(target).rsplit("-", 1)[0]
    name = binary.removeprefix("cayenchat").lstrip("_-")
    return name.replace("_", "-") if name else "ui"


def read_results(log_path):
    results = defaultdict(list)  # (crate, fn name) -> [(module path, result)]
    crate = None
    with open(log_path, encoding="utf-8", errors="replace") as log:
        for line in log:
            running = RUNNING.match(line)
            if running:
                crate = binary_crate(running.group(2), running.group(1))
                continue
            if "Doc-tests" in line:
                crate = None
                continue
            result = RESULT.match(line.strip())
            if result and crate:
                path = result.group(1)
                results[(crate, path.rsplit("::", 1)[-1])].append((path, result.group(2)))
    return results


def attach_results(tests, results):
    for test in tests:
        candidates = results.get((test["crate"], test["name"]), [])
        if len(candidates) > 1:
            # Same name in several modules: prefer the one named after the file.
            stem = os.path.splitext(os.path.basename(test["file"]))[0]
            named = [c for c in candidates if stem in c[0].split("::")]
            candidates = named or candidates
        if candidates:
            test["path"], test["result"] = candidates[0]


def area(test):
    if test["crate"] not in IRCV3_CRATES:
        return ""
    if test["file"] in AREA_BY_FILE:
        return AREA_BY_FILE[test["file"]]
    if test["file"] == "crates/irc-core/src/peer_avatar.rs":
        return ""
    name = test["name"]
    for word, label in AREA_BY_NAME:
        if word in name:
            return label
    return ""


def read_coverage(summary_path):
    with open(summary_path, encoding="utf-8") as summary:
        data = json.load(summary)["data"][0]
    files = {}
    for entry in data["files"]:
        rel = os.path.relpath(entry["filename"], ROOT)
        if rel.startswith("crates/"):
            lines = entry["summary"]["lines"]
            files[rel] = (lines["covered"], lines["count"], entry["filename"])
    totals = data["totals"]["lines"]
    return files, (totals["covered"], totals["count"])


def percent(covered, count):
    return 100.0 * covered / count if count else 0.0


CSS = """
:root { --bg:#fff; --fg:#1d1d1f; --muted:#6e6e73; --line:#e5e5ea; --ok:#1a7f37;
  --warn:#9a6700; --bad:#cf222e; --bar:#34c759; --barbg:#e5e5ea; --head:#f5f5f7; }
@media (prefers-color-scheme: dark) { :root { --bg:#1c1c1e; --fg:#f2f2f7; --muted:#98989d;
  --line:#3a3a3c; --ok:#3fb950; --warn:#d29922; --bad:#f85149; --bar:#30d158; --barbg:#3a3a3c; --head:#2c2c2e; } }
body { background:var(--bg); color:var(--fg); font:14px/1.45 -apple-system, "Segoe UI", sans-serif;
  margin:0 auto; max-width:1100px; padding:16px; }
h1 { font-size:22px; } h2 { font-size:17px; margin-top:28px; }
table { border-collapse:collapse; width:100%; margin:8px 0 16px; }
th, td { text-align:left; padding:3px 8px; border-bottom:1px solid var(--line); vertical-align:top; }
th { background:var(--head); font-weight:600; }
td.num { text-align:right; font-variant-numeric:tabular-nums; white-space:nowrap; }
code { font-size:12.5px; } .muted { color:var(--muted); }
.ok { color:var(--ok); } .ignored { color:var(--warn); } .FAILED, .missing { color:var(--bad); }
.bar { display:inline-block; width:80px; height:8px; background:var(--barbg); border-radius:4px;
  vertical-align:middle; margin-right:6px; overflow:hidden; }
.bar > span { display:block; height:100%; background:var(--bar); }
details { margin:6px 0; } summary { cursor:pointer; }
.filter { margin:8px 0; } input { font:inherit; padding:4px 8px; width:280px; }
"""

SCRIPT = """
const input = document.getElementById('filter');
input.addEventListener('input', () => {
  const q = input.value.toLowerCase();
  document.querySelectorAll('tr.test').forEach(row => {
    row.style.display = row.dataset.search.includes(q) ? '' : 'none';
  });
  document.querySelectorAll('details.file').forEach(block => {
    const any = [...block.querySelectorAll('tr.test')].some(r => r.style.display !== 'none');
    block.style.display = any ? '' : 'none';
    if (q) block.open = any;
  });
});
"""


def bar(value):
    return f'<span class="bar"><span style="width:{value:.0f}%"></span></span>{value:.1f}%'


def write_html(tests, coverage, out_path):
    files, totals = coverage if coverage else ({}, None)
    e = html.escape
    results = Counter(test["result"] or "not run" for test in tests)
    parts = [
        "<!doctype html><html><head><meta charset='utf-8'>",
        "<meta name='viewport' content='width=device-width, initial-scale=1'>",
        f"<title>CayenChat tests</title><style>{CSS}</style></head><body>",
        "<h1>CayenChat tests and coverage</h1>",
        "<p>",
        f"{len(tests)} tests in the sources: ",
        f"<span class='ok'>{results['ok']} passed</span>, ",
        f"<span class='ignored'>{results['ignored']} ignored</span>, ",
        f"<span class='FAILED'>{results['FAILED']} failed</span>, ",
        f"<span class='muted'>{results['not run']} not in the run log</span>.",
    ]
    if totals:
        parts.append(f" Line coverage: {bar(percent(*totals))} ({totals[0]:,} / {totals[1]:,} lines).")
    parts.append(" Line-by-line coverage: <a href='../llvm-cov/html/index.html'>llvm-cov report</a>.</p>")

    by_crate = defaultdict(list)
    for test in tests:
        by_crate[test["crate"]].append(test)
    parts.append("<h2>Per crate</h2><table><tr><th>Crate</th><th>Tests</th><th>Passed</th><th>Ignored</th><th>Line coverage</th></tr>")
    for crate, crate_tests in sorted(by_crate.items()):
        counts = Counter(t["result"] for t in crate_tests)
        covered = sum(v[0] for k, v in files.items() if k.startswith(f"crates/{crate}/"))
        count = sum(v[1] for k, v in files.items() if k.startswith(f"crates/{crate}/"))
        cov = bar(percent(covered, count)) if count else "<span class='muted'>–</span>"
        parts.append(
            f"<tr><td>{e(crate)}</td><td class='num'>{len(crate_tests)}</td>"
            f"<td class='num'>{counts['ok']}</td><td class='num'>{counts['ignored']}</td><td>{cov}</td></tr>"
        )
    parts.append("</table>")

    areas = defaultdict(list)
    for test in tests:
        label = area(test)
        if label:
            areas[label].append(test)
    parts.append("<h2>IRCv3 areas</h2><p class='muted'>Assigned by file and test name; see spec/development.md.</p>")
    parts.append("<table><tr><th>Area</th><th>Tests</th><th>Passed</th><th>Ignored (need a server)</th><th>Files</th></tr>")
    for label, area_tests in sorted(areas.items()):
        counts = Counter(t["result"] for t in area_tests)
        where = Counter(t["file"] for t in area_tests)
        files_cell = "<br>".join(f"<code>{e(f)}</code> ({n})" for f, n in where.most_common())
        parts.append(
            f"<tr><td>{e(label)}</td><td class='num'>{len(area_tests)}</td><td class='num'>{counts['ok']}</td>"
            f"<td class='num'>{counts['ignored']}</td><td>{files_cell}</td></tr>"
        )
    parts.append("</table>")

    parts.append("<h2>Tests by file</h2><div class='filter'><input id='filter' placeholder='Filter by name, file or area'></div>")
    by_file = defaultdict(list)
    for test in tests:
        by_file[test["file"]].append(test)
    for path in sorted(by_file):
        file_tests = by_file[path]
        cov = ""
        if path in files:
            covered, count, absolute = files[path]
            link = "../llvm-cov/html/coverage" + absolute + ".html"
            cov = f" — lines {bar(percent(covered, count))} <a href='{e(link)}'>source</a>"
        parts.append(f"<details class='file'><summary><code>{e(path)}</code> — {len(file_tests)} tests{cov}</summary>")
        parts.append("<table><tr><th>Test</th><th>Line</th><th>Result</th><th>IRCv3 area</th></tr>")
        for test in file_tests:
            result = test["result"] or "not run"
            label = area(test)
            search = f"{test['name']} {path} {label} {result}".lower()
            parts.append(
                f"<tr class='test' data-search='{e(search)}'><td><code>{e(test['name'])}</code></td>"
                f"<td class='num'>{test['line']}</td><td class='{e(result.replace(' ', '-'))}'>{e(result)}</td>"
                f"<td>{e(label)}</td></tr>"
            )
        parts.append("</table></details>")

    uncovered = sorted(
        (percent(c, n), path, n) for path, (c, n, _) in files.items() if n >= 50 and path not in by_file
    )
    if uncovered:
        parts.append("<h2>Files without tests of their own</h2><p class='muted'>At least 50 lines; they may still be exercised by tests elsewhere.</p>")
        parts.append("<table><tr><th>File</th><th>Lines</th><th>Coverage</th></tr>")
        for value, path, count in uncovered:
            parts.append(f"<tr><td><code>{e(path)}</code></td><td class='num'>{count}</td><td>{bar(value)}</td></tr>")
        parts.append("</table>")
    parts.append(f"<script>{SCRIPT}</script></body></html>")
    with open(out_path, "w", encoding="utf-8") as out:
        out.write("".join(parts))


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--log", help="cargo test / cargo llvm-cov output")
    parser.add_argument("--summary", help="cargo llvm-cov report --json --summary-only output")
    parser.add_argument("--out", help="HTML file to write")
    args = parser.parse_args()

    tests = find_tests()
    if args.log:
        attach_results(tests, read_results(args.log))
    coverage = read_coverage(args.summary) if args.summary else None

    results = Counter(test["result"] or "not run" for test in tests)
    print(f"{len(tests)} tests: " + ", ".join(f"{n} {k}" for k, n in sorted(results.items())))
    areas = Counter(area(test) for test in tests if area(test))
    for label, count in sorted(areas.items()):
        print(f"  IRCv3 {label}: {count}")
    if args.out:
        write_html(tests, coverage, args.out)
        print(f"Wrote {args.out}")
    return 1 if results["FAILED"] else 0


if __name__ == "__main__":
    sys.exit(main())
