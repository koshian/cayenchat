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


def black_square(path, side=40):
    """Writes a black PNG of `side` pixels."""
    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))
    rows = b"".join(b"\0" + b"\0\0\0" * side for _ in range(side))
    with open(path, "wb") as file:
        file.write(b"\x89PNG\r\n\x1a\n"
                   + chunk(b"IHDR", struct.pack(">IIBBBBB", side, side, 8, 2, 0, 0, 0))
                   + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


# Inside the picture (rows, no text), and in the frame left of the log beside
# the picture.
IN_PICTURE = Rect(300, 100, 200, 100)
FRAME_LEFT = Rect(0, 100, 6, 100)


def check_wallpaper(app, pane, wallpaper):
    """The wallpaper's mix and the pane's opacity are independent (#989). The
    pane has the opacity `pane` as a whole: where the "contain" picture leaves
    the log bare, and in the frame, every pixel is a row color at `pane` once
    over the window base, whatever the mix. On the (black) picture the mix of
    the color's share `wallpaper` and the picture is made first and `pane`
    applies to it: the color at pane*wallpaper over the base."""
    background = parse_color(BACKGROUND)
    alternate = parse_color(ALTERNATE)
    on_base = (over_base(background, pane), over_base(alternate, pane))
    # Black picture: p*(black*(1-b) + c*b) + (1-p)*base.
    on_picture = tuple(over_base(tuple(round(c * wallpaper / 100) for c in color), pane)
                       for color in (background, alternate))
    wanted = {"beside the picture": (BESIDE_PICTURE, on_base),
              "in the frame": (FRAME_LEFT, on_base),
              "on the picture": (IN_PICTURE, on_picture)}
    with tempfile.TemporaryDirectory(prefix="opacity-picture-") as scratch:
        picture = os.path.join(scratch, "square.png")
        black_square(picture)
        settings = {"appearance": {"channel_log_background": BACKGROUND,
                                   "channel_log_alternate": ALTERNATE,
                                   "alternate_rows": True,
                                   "channel_log_opacity": pane,
                                   "channel_looks": [
                                       {"server": "local", "channel": "#demo",
                                        "wallpaper": picture, "wallpaper_fit": "contain",
                                        "background_opacity": wallpaper,
                                        # The tone defaults would lighten the
                                        # black picture in the light theme (#1047).
                                        "wallpaper_brightness_light": 0,
                                        "wallpaper_saturation_light": 100,
                                        "wallpaper_brightness_dark": 0,
                                        "wallpaper_saturation_dark": 100}]}}
        with FixtureSession(app, settings, lines=LINES) as session:
            log = session.pane(LOG_PANE, f"wallpaper-{pane}-{wallpaper}")
            for where, (rect, allowed) in wanted.items():
                cut = log.cut(rect)
                for color in cut.colors():
                    if not any(near(color, want) for want in allowed):
                        raise PixelMismatch(
                            f"{where} at pane {pane} %, wallpaper {wallpaper} %: "
                            f"{hex_color(color)} is none of "
                            f"{[hex_color(want) for want in allowed]}: {cut.summary(8)}")
            print(f"ok wallpaper pane {pane} %, wallpaper {wallpaper} %")


# The combined log's rows (right of the text, on the black picture), clear of
# the channel list and the time column.
COMBINED_ROWS = Rect(500, 416, 200, 320)
COMBINED_BACKGROUND = "#3366cc"
COMBINED_ALTERNATE = "#cc6633"


def check_combined(app, pane, wallpaper):
    """The combined log's alternate rows hold the same mix and the same pane
    opacity as the others (#989): a row is the color at pane*wallpaper over the
    black picture, over the window base, with no extra layer of its own."""
    wanted = [over_base(tuple(round(c * wallpaper / 100) for c in parse_color(color)), pane)
              for color in (COMBINED_BACKGROUND, COMBINED_ALTERNATE)]
    with tempfile.TemporaryDirectory(prefix="opacity-combined-") as scratch:
        picture = os.path.join(scratch, "black.png")
        black_square(picture)
        settings = {"appearance": {"alternate_rows": True,
                                   "combined_log_background": COMBINED_BACKGROUND,
                                   "combined_log_alternate": COMBINED_ALTERNATE,
                                   "combined_log_opacity": pane,
                                   "wallpapers": {"combined_log": {"path": picture,
                                                                   "opacity": wallpaper,
                                                                   "brightness": 0,
                                                                   "saturation": 100}}}}
        with FixtureSession(app, settings, lines=LINES, channel="#demo,#other") as session:
            cut = session.screen(f"combined-{pane}-{wallpaper}").cut(COMBINED_ROWS)
            seen = [False, False]
            for color in cut.colors():
                for index, want in enumerate(wanted):
                    seen[index] |= near(color, want)
                if not any(near(color, want) for want in wanted):
                    raise PixelMismatch(
                        f"combined log at pane {pane} %, mix {wallpaper} %: {hex_color(color)} "
                        f"is none of {[hex_color(want) for want in wanted]}: {cut.summary(8)}")
            if not all(seen):
                raise PixelMismatch(f"combined log at pane {pane} %, mix {wallpaper} %: "
                                    f"rows seen {seen}, want both {[hex_color(w) for w in wanted]}")
            print(f"ok combined pane {pane} %, mix {wallpaper} %")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--app", required=True)
    args = parser.parse_args()
    try:
        check(args.app, OPACITY)
        check(args.app, 100)
        for pane, wallpaper in ((100, 80), (40, 80), (40, 100), (40, 0), (40, 50)):
            check_wallpaper(args.app, pane, wallpaper)
        for pane, wallpaper in ((40, 80), (40, 20), (100, 80)):
            check_combined(args.app, pane, wallpaper)
    except PixelMismatch as error:
        sys.exit(f"FAIL: {error}")
    print("all passed")


if __name__ == "__main__":
    main()
