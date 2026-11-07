#!/usr/bin/env python3
"""Checks the pure parts of pixel_check.py, without a display:

    python3 -m unittest discover -s scripts/e2e -p 'test_*.py'

Mostly that a wrong expectation fails, so the GUI checks built on it cannot
pass for nothing.
"""
import os, sys, unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pixel_check import PixelMismatch, Pixels, Rect, hex_color, parse_color  # noqa: E402

A, B = (0x33, 0x66, 0xCC), (0xCC, 0x66, 0x33)


class PixelsTest(unittest.TestCase):
    def picture(self):
        return Pixels.from_rgb([[A, A, B], [A, A, B]])

    def test_parse_color(self):
        self.assertEqual(parse_color("#3366cc"), A)
        self.assertEqual(parse_color(0x3366CC), A)
        self.assertEqual(parse_color(A), A)
        self.assertEqual(hex_color(A), "#3366cc")
        with self.assertRaises(ValueError):
            parse_color("#fff")

    def test_cut_keeps_only_the_rectangle(self):
        cut = self.picture().cut(Rect(1, 0, 2, 2))
        self.assertEqual(cut.colors(), {A: 2, B: 2})
        self.assertEqual(cut.rgb(1, 1), B)

    def test_cut_outside_the_picture_fails(self):
        with self.assertRaises(PixelMismatch):
            self.picture().cut(Rect(2, 0, 2, 1))

    def test_uniform(self):
        self.picture().cut(Rect(0, 0, 2, 2)).expect_uniform("#3366cc")
        with self.assertRaises(PixelMismatch):
            self.picture().expect_uniform("#3366cc")
        with self.assertRaises(PixelMismatch):
            self.picture().cut(Rect(0, 0, 2, 2)).expect_uniform("#3366cd")
        self.picture().cut(Rect(0, 0, 2, 2)).expect_uniform("#3366cd", tolerance=1)

    def test_present_and_absent(self):
        picture = self.picture()
        picture.expect_present(B)
        picture.cut(Rect(0, 0, 2, 2)).expect_absent(B)
        with self.assertRaises(PixelMismatch):
            picture.expect_absent(B)
        with self.assertRaises(PixelMismatch):
            picture.cut(Rect(0, 0, 2, 2)).expect_present(B)


if __name__ == "__main__":
    unittest.main()
