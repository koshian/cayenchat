#!/usr/bin/env python3
"""Pixel check of the channel log with a see-through background (issue #930):

    scripts/e2e/opacity_gui.py --app target/debug/cayenchat

The log pane is set to 40 % opacity. Xvfb has no compositor, so the window
stays opaque and the pane lies over the window's base color (white in the
light theme); each pixel is the pane color times 0.4 plus the base times 0.6,
which can be computed:

- the empty top of the short log (above the oldest line) has the background
  at 40 %, not the bare base and not a doubled opacity;
- the part right of the text holds exactly the background and the alternate
  color, both at 40 %, so no filler or row covers another and every row has
  the same opacity;
- at 100 % the same part is the plain colors.
"""
import argparse, os, struct, sys, tempfile, zlib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pixel_check import (  # noqa: E402
    FixtureSession, PixelMismatch, Rect, hex_color, parse_color)
from log_colors_gui import (  # noqa: E402
    ALTERNATE, BACKGROUND, EMPTY_TOP, LINES, LOG_PANE, TEXT_FREE, row_colors)

OPACITY = 40
TOLERANCE = 2


BASE = (255, 255, 255)


def over_base(color, percent):
    return tuple(round((c * percent + b * (100 - percent)) / 100)
                 for c, b in zip(color, BASE))


def near(a, b):
    return all(abs(x - y) <= TOLERANCE for x, y in zip(a, b))


def check(app, percent):
    background = over_base(parse_color(BACKGROUND), percent)
    alternate = over_base(parse_color(ALTERNATE), percent)
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


# Right of the picture a "contain" wallpaper leaves in the log (the square
# picture is centered in the 855 px wide pane), where only the background lies.
BESIDE_PICTURE = Rect(650, 0, 200, 342)


def white_square(path, side=40):
    """Writes a white PNG of `side` pixels."""
    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))
    rows = b"".join(b"\0" + b"\xff\xff\xff" * side for _ in range(side))
    with open(path, "wb") as file:
        file.write(b"\x89PNG\r\n\x1a\n"
                   + chunk(b"IHDR", struct.pack(">IIBBBBB", side, side, 8, 2, 0, 0, 0))
                   + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def check_wallpaper(app, percent):
    """Where a "contain" wallpaper leaves the log bare, every pixel is the
    row color at the pane's opacity once (over the window base here): the
    plain rows and the part above a short log the background, the alternate
    rows the alternate color, none stacked on another layer, and none opaque."""
    background = over_base(parse_color(BACKGROUND), percent)
    alternate = over_base(parse_color(ALTERNATE), percent)
    with tempfile.TemporaryDirectory(prefix="opacity-picture-") as scratch:
        picture = os.path.join(scratch, "square.png")
        white_square(picture)
        settings = {"appearance": {"channel_log_background": BACKGROUND,
                                   "channel_log_alternate": ALTERNATE,
                                   "alternate_rows": True,
                                   "channel_log_opacity": percent,
                                   "channel_looks": [
                                       {"server": "local", "channel": "#demo",
                                        "wallpaper": picture, "wallpaper_fit": "contain",
                                        "background_opacity": percent}]}}
        with FixtureSession(app, settings, lines=LINES) as session:
            log = session.pane(LOG_PANE, f"wallpaper-{percent}")
            bare = log.cut(BESIDE_PICTURE)
            for color in bare.colors():
                if not (near(color, background) or near(color, alternate)):
                    raise PixelMismatch(
                        f"beside the picture at {percent} %: {hex_color(color)} is neither "
                        f"{hex_color(background)} nor {hex_color(alternate)}: {bare.summary(8)}")
            print(f"ok wallpaper {percent} %: {bare.summary(4)}")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--app", required=True)
    args = parser.parse_args()
    try:
        check(args.app, OPACITY)
        check(args.app, 100)
        check_wallpaper(args.app, OPACITY)
    except PixelMismatch as error:
        sys.exit(f"FAIL: {error}")
    print("all passed")


if __name__ == "__main__":
    main()
