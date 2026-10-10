# Local native-theme 0.5.7 changes

This directory is a copy of the published native-theme 0.5.7 crate
(<https://github.com/tiborgats/native-theme>), licensed under
`MIT OR Apache-2.0 OR 0BSD`. The published crate ships no license file, so the
upstream repository's `LICENSE-0BSD`, `LICENSE-MIT` and `LICENSE-APACHE` are
added here; CayenChat uses the crate under 0BSD. The workspace uses the copy
through `[patch.crates-io]`.

## Windows fonts and metrics in logical pixels (#290)

Backported from native-theme 0.6.1, which cannot be used yet:
`native-theme-gpui` 0.6.1 needs GPUI 0.3.8 (`gpui-pre`) and `gpui-component`
0.7.1, and the workspace is on GPUI 0.2.2 (#265).

0.5.7 read the Windows fonts (`SystemParametersInfoW`, in a DPI-aware
process) and metrics (`GetSystemMetricsForDpi`) at the system DPI and reported
that DPI as `font_dpi`, so the resolved sizes were physical pixels. GPUI treats
them as logical pixels and applies the display scale again: at 150 % the
settings window's text was drawn at 1.5 times its size. The system DPI is also
fixed at sign-in, so after changing the scale without signing out the text
stayed enlarged even at 100 %.

In `src/windows.rs`:

- `LOGICAL_DPI` (96, `USER_DEFAULT_SCREEN_DPI`) replaces `read_dpi()`
  (`GetDpiForSystem`) where the reader starts, so the metrics, icon sizes and
  `font_dpi` are all at 96.
- `read_all_system_fonts` uses `SystemParametersInfoForDpi(SPI_GETNONCLIENTMETRICS,
  …, 96)` instead of `SystemParametersInfoW`.

In `src/detect.rs`, `detect_system_font_dpi` returns `LOGICAL_DPI` on Windows.

Nothing changes on macOS or Linux.
