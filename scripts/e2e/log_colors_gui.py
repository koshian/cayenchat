#!/usr/bin/env python3
"""Pixel check of the channel log's colors, the first user of `pixel_check.py`
(issue #959):

    scripts/e2e/log_colors_gui.py --app target/debug/cayenchat

The settings give the log a background and an alternate-row color. Both can
be computed from them, so the expected pixels are exact, not judged by eye:

- the part of the log pane right of the message text (no glyphs there) holds
  exactly the background and the alternate color when `alternate_rows` is on,
  with the alternate color on every other row;
- with `alternate_rows` off the same part is the background alone, so the
  two modes draw differently;
- the empty top of the pane, above the oldest line, is the background.

Only the log pane's rectangle of a 1100x750 window (toolbar hidden, in-window
menu bar 28 px high) is cut out of the screen. `--expect-background` changes
the expected color so that the check can be seen to fail (it must exit
non-zero), which shows it is not passing for nothing.
"""
import argparse, sys, os

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pixel_check import FixtureSession, PixelMismatch, Rect, hex_color, parse_color  # noqa: E402

BACKGROUND = "#3366cc"
ALTERNATE = "#cc6633"
LINES = [f"line {n}" for n in range(1, 7)]
# The log pane of the 1100x750 window, and the part of it where there is
# only row background: right of the longest line, below the oldest one.
LOG_PANE = Rect(0, 28, 855, 342)
TEXT_FREE = Rect(300, 0, 500, 342)  # inside the pane
EMPTY_TOP = Rect(0, 0, 855, 150)  # inside the pane


def settings(alternate_rows):
    return {"appearance": {"channel_log_background": BACKGROUND, "channel_log_alternate": ALTERNATE,
                           "alternate_rows": alternate_rows}}


def row_colors(pane, x):
    """The color at column `x` of the pane for each of its rows, top down,
    without repeats of a row."""
    column = [pane.rgb(x, y) for y in range(pane.height)]
    return [color for index, color in enumerate(column) if index == 0 or color != column[index - 1]]


def check(app, expect_background):
    background, alternate = parse_color(expect_background), parse_color(ALTERNATE)
    with FixtureSession(app, settings(True), lines=LINES) as session:
        log = session.pane(LOG_PANE, "alternate-on")
        log.cut(EMPTY_TOP).expect_uniform(background, what="empty top of the log")
        free = log.cut(TEXT_FREE)
        colors = set(free.colors())
        if colors != {background, alternate}:
            raise PixelMismatch("rows: want only {} and {}, got {}".format(
                hex_color(background), hex_color(alternate), free.summary(8)))
        # Down the column: background, then the alternate color on lines 1, 3
        # and 5 with the background between them and after.
        runs = row_colors(free, 250)
        want = [background, alternate, background, alternate, background, alternate, background]
        if runs != want:
            raise PixelMismatch(f"alternate rows down the column: want {len(want)} runs "
                                f"{[hex_color(c) for c in want]}, got {[hex_color(c) for c in runs]}")
        print("ok alternate rows on")
    with FixtureSession(app, settings(False), lines=LINES) as session:
        free = session.pane(LOG_PANE, "alternate-off").cut(TEXT_FREE)
        free.expect_uniform(background, what="the log without alternate rows")
        free.expect_absent(alternate, what="the log without alternate rows")
        print("ok alternate rows off")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--app", required=True)
    parser.add_argument("--expect-background", default=BACKGROUND,
                        help="expect another color, to see the check fail")
    args = parser.parse_args()
    try:
        check(args.app, args.expect_background)
    except PixelMismatch as error:
        sys.exit(f"FAIL: {error}")
    print("all passed")


if __name__ == "__main__":
    main()
