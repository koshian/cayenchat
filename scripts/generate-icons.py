#!/usr/bin/env python3
"""Regenerate platform icons from the project artwork (requires Pillow)."""

from pathlib import Path

from PIL import Image


root = Path(__file__).resolve().parents[1]
source = Image.open(root / "icon.png").convert("RGBA")
source.save(root / "crates/ui/resources/macos/CayenChat.icns", format="ICNS")
source.save(
    root / "crates/ui/resources/windows/cayenchat.ico",
    format="ICO",
    sizes=[(size, size) for size in (16, 24, 32, 48, 64, 128, 256)],
)
