#!/usr/bin/env python3
"""Regenerate platform icons from the project artwork.

Requires Pillow and `rsvg-convert` (librsvg: Debian/Ubuntu `librsvg2-bin`,
Homebrew `librsvg`).
"""

import io
import subprocess
from pathlib import Path

from PIL import Image


root = Path(__file__).resolve().parents[1]
rendered = subprocess.run(
    ["rsvg-convert", "--width=1024", "--height=1024", root / "assets/icons/cayenchat.svg"],
    check=True,
    capture_output=True,
).stdout
source = Image.open(io.BytesIO(rendered)).convert("RGBA")
source.save(root / "crates/ui/resources/macos/CayenChat.icns", format="ICNS")
source.save(
    root / "crates/ui/resources/windows/cayenchat.ico",
    format="ICO",
    sizes=[(size, size) for size in (16, 24, 32, 48, 64, 128, 256)],
)
