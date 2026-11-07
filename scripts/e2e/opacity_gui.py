#!/usr/bin/env python3
"""Pixel check of the channel log with a see-through background (issue #930):

    scripts/e2e/opacity_gui.py --app target/debug/cayenchat

The log pane is set to 40 % opacity. Xvfb has no compositor, so the window's
transparent pixels show black and each pixel is the pane color times 0.4,
which can be computed:

- the empty top of the short log (above the oldest line) has the background
  at 40 %, not the desktop (black) and not a doubled opacity;
- the part right of the text holds exactly the background and the alternate
  color, both at 40 %, so no filler or row covers another and every row has
  the same opacity;
- at 100 % the same part is the plain colors.
"""
import argparse, os, sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pixel_check import FixtureSession, PixelMismatch, hex_color, parse_color  # noqa: E402
from log_colors_gui import (  # noqa: E402
    ALTERNATE, BACKGROUND, EMPTY_TOP, LINES, LOG_PANE, TEXT_FREE, row_colors)

OPACITY = 40
TOLERANCE = 2


def over_black(color, percent):
    return tuple(round(channel * percent / 100) for channel in color)


def near(a, b):
    return all(abs(x - y) <= TOLERANCE for x, y in zip(a, b))


def check(app, percent):
    background = over_black(parse_color(BACKGROUND), percent)
    alternate = over_black(parse_color(ALTERNATE), percent)
    settings = {"appearance": {"channel_log_background": BACKGROUND,
                               "channel_log_alternate": ALTERNATE,
                               "alternate_rows": True,
                               "channel_log_opacity": percent}}
    with FixtureSession(app, settings, lines=LINES) as session:
        log = session.pane(LOG_PANE, f"opacity-{percent}")
        log.cut(EMPTY_TOP).expect_uniform(background, tolerance=TOLERANCE,
                                          what=f"empty top of the log at {percent} %")
        free = log.cut(TEXT_FREE)
        for color in free.colors():
            if not (near(color, background) or near(color, alternate)):
                raise PixelMismatch(f"rows at {percent} %: {hex_color(color)} is neither "
                                    f"{hex_color(background)} nor {hex_color(alternate)}: "
                                    f"{free.summary(8)}")
        runs = row_colors(free, 250)
        shades = [near(c, alternate) for c in runs]
        want = [False, True, False, True, False, True, False]
        if shades != want:
            raise PixelMismatch(f"alternate rows down the column at {percent} %: want {want}, "
                                f"got {shades} ({[hex_color(c) for c in runs]})")
        print(f"ok {percent} %")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--app", required=True)
    args = parser.parse_args()
    try:
        check(args.app, OPACITY)
        check(args.app, 100)
    except PixelMismatch as error:
        sys.exit(f"FAIL: {error}")
    print("all passed")


if __name__ == "__main__":
    main()
