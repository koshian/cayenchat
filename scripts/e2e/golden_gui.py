#!/usr/bin/env python3
"""Reference images (golden) for looks that no formula gives, and their
provenance (spec/development.md, "Verifying what is drawn"; issue #959).

    scripts/e2e/golden_gui.py compare --app target/debug/cayenchat
    scripts/e2e/golden_gui.py check
    scripts/e2e/golden_gui.py update  --app target/debug/cayenchat   # then look, then:
    scripts/e2e/golden_gui.py confirm --by "NAME"

The references are in `assets/golden/`, each with an entry in
`assets/golden/provenance.json` (commit, binary hash, system, fixture, image
hash, who confirmed it and when). Each reference is one pane of a fixed
fixture, started like `pixel_check.py` does it.

- `compare` renders the fixture with --app and fails when any pixel differs
  from the reference (the actual picture is saved under --out). It never
  changes a reference. Text is drawn with the system's fonts, so it is only
  meaningful on the system recorded in the entry; it is not run in CI.
- `check` needs no display or build. It fails when a reference is more than
  --max-behind commits to `crates/ui` or `crates/app` behind HEAD, was taken
  from uncommitted sources, or its file is not the recorded image. It reports
  a reference nobody has confirmed. This is the same idea as `demo.py check`.
- `update` is the only way to write a reference. It leaves the entry
  unconfirmed. A person then looks at the picture (in a test build), and only
  `confirm` records that this is the intended look. A reference is replaced
  only together with such a confirmation; no script does it on its own.
"""
import argparse, datetime, json, os, platform, sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "screenshots"))
from pixel_check import FixtureSession, PixelMismatch, Pixels, Rect  # noqa: E402
import log_colors_gui as fixture  # noqa: E402
from demo import ROOT, git, sha256  # noqa: E402

GOLDEN_DIR = os.path.join(ROOT, "assets", "golden")
PROVENANCE = os.path.join(GOLDEN_DIR, "provenance.json")
# Ends the freshness check: more commits than this to the app since a reference.
MAX_BEHIND = 100
GUARDED = ("crates/ui", "crates/app")

# name -> what it shows. The fixture is log_colors_gui.py's: six lines in the
# log with alternate rows. The cut leaves out the timestamps at the left (they
# are the wall clock and change every minute) and the empty top of the pane.
LINES_AREA = Rect(50, 180, 805, 162)  # inside the log pane
GOLDENS = {
    "log-rows": "channel log, text of 7 rows and alternate rows on, light theme, 1100x750 window, "
                "right of the timestamps",
}


def render(app, name):
    with FixtureSession(app, fixture.settings(True), lines=fixture.LINES) as session:
        return session.pane(fixture.LOG_PANE, name).cut(LINES_AREA)


def image_path(name):
    return os.path.join(GOLDEN_DIR, name + ".png")


def load_entries():
    try:
        with open(PROVENANCE, encoding="utf-8") as file:
            return json.load(file)
    except FileNotFoundError:
        return {}


def save_entries(entries):
    os.makedirs(GOLDEN_DIR, exist_ok=True)
    with open(PROVENANCE, "w", encoding="utf-8") as file:
        json.dump(entries, file, indent=2, sort_keys=True)
        file.write("\n")


def command_compare(args):
    entries, failed = load_entries(), False
    for name in GOLDENS:
        if name not in entries:
            sys.exit(f"{name}: no reference; make one with `update`")
        golden = Pixels.from_png(open(image_path(name), "rb").read())
        actual = render(args.app, name)
        try:
            actual.expect_same_as(golden, what=name)
            print(f"ok {name}")
        except PixelMismatch as error:
            os.makedirs(args.out, exist_ok=True)
            path = os.path.join(args.out, name + "-actual.png")
            with open(path, "wb") as file:
                file.write(actual.to_png())
            print(f"FAIL: {error}; the actual picture is {path}")
            failed = True
    if failed:
        sys.exit("a look changed: if it is intended, `update` and have a person confirm it")
    print("all passed")


def command_update(args):
    entries = load_entries()
    os.makedirs(GOLDEN_DIR, exist_ok=True)
    for name, shows in GOLDENS.items():
        picture = render(args.app, name)
        with open(image_path(name), "wb") as file:
            file.write(picture.to_png())
        entries[name] = {
            "shows": shows,
            "commit": git("rev-parse", "HEAD"),
            "dirty": bool(git("status", "--porcelain", "--", *GUARDED)),
            "binary_sha256": sha256(args.app),
            "captured": datetime.datetime.now().astimezone().isoformat(timespec="seconds"),
            "os": f"{platform.system()} {platform.release()}",
            "image_sha256": sha256(image_path(name)),
            "confirmed_by": None,
            "confirmed": None,
        }
        print(f"wrote {os.path.relpath(image_path(name), ROOT)}: look at it in a test build, "
              f"then `confirm --by NAME`")
    save_entries(entries)


def command_confirm(args):
    entries = load_entries()
    if not entries:
        sys.exit("no references")
    for name, entry in entries.items():
        if sha256(image_path(name)) != entry["image_sha256"]:
            sys.exit(f"{name}: the file is not the recorded picture; run `update` again")
        entry["confirmed_by"] = args.by
        entry["confirmed"] = datetime.date.today().isoformat()
    save_entries(entries)
    print(f"confirmed {', '.join(entries)} as {args.by}")


def command_check(args):
    entries, late = load_entries(), False
    for name in GOLDENS:
        if name not in entries:
            print(f"{name}: no entry")
            late = True
    for name, entry in sorted(entries.items()):
        try:
            behind = int(git("rev-list", "--count", f"{entry['commit']}..HEAD", "--", *GUARDED))
        except Exception:
            behind = None
        note = "unknown commit" if behind is None else f"{behind} commits behind"
        if entry.get("dirty"):
            note += ", taken from uncommitted sources"
        if name not in GOLDENS:
            note += ", no such reference is defined any more"
            late = True
        path = image_path(name)
        if not os.path.isfile(path) or sha256(path) != entry.get("image_sha256"):
            note += ", the file is not the recorded picture"
            late = True
        if not entry.get("confirmed_by"):
            note += ", NOT CONFIRMED by a person yet"
        print(f"{name}: {entry['commit'][:10]} {entry['captured']}, {note}")
        late |= behind is None or behind > args.max_behind or bool(entry.get("dirty"))
    if late:
        sys.exit(f"take the references again (update, then a person confirms); limit is "
                 f"{args.max_behind} commits")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    commands = parser.add_subparsers(dest="command", required=True)
    for name, handler, help_text in (
            ("compare", command_compare, "render and compare with the references"),
            ("update", command_update, "write new references (unconfirmed)")):
        command = commands.add_parser(name, help=help_text)
        command.add_argument("--app", required=True)
        command.set_defaults(handler=handler)
        if name == "compare":
            command.add_argument("--out", default=os.path.join(ROOT, "target", "e2e", "golden"))
    command = commands.add_parser("confirm", help="record that a person confirmed the look")
    command.add_argument("--by", required=True)
    command.set_defaults(handler=command_confirm)
    command = commands.add_parser("check", help="are the references fresh (no display needed)")
    command.add_argument("--max-behind", type=int, default=MAX_BEHIND)
    command.set_defaults(handler=command_check)
    args = parser.parse_args()
    args.handler(args)


if __name__ == "__main__":
    main()
