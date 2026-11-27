# Third-party notices

The application uses [GPUI](https://github.com/zed-industries/zed) through the
`gpui-pre` 0.3.12 and `gpui-pre-platform` 0.3.12 crates of GPUI Kit (Cargo
dependencies, Copyright Zed Industries, licensed under Apache License 2.0).
CayenChat's patches to GPUI 0.3.12 are in `vendor/gpui-pre*` (see each
`PATCHES.md`); the older GPUI 0.2.2 copy was removed (#1144) and remains in
Git history. In addition,
`crates/ui/src/input.rs` adapts its
[text input example](https://docs.rs/crate/gpui/0.2.2/source/examples/input.rs)
and remains under Apache License 2.0. The license is retained in
`licenses/GPUI-APACHE-2.0.txt`.
Modifications cover styling, focus, scoped shortcuts, line-break normalization,
and IME selection offsets. No LimeChat implementation code is included.

The IRC connection adapter depends on [`irc` 1.1.0](https://github.com/aatxe/irc),
licensed under MPL-2.0. No source code from that crate is copied into this project;
its types remain inside `crates/irc-core`. The crate's license is retained in
`licenses/IRC-MPL-2.0.md`. Include the dependency's required license and source
availability materials when preparing a distributable binary.

Settings appearance uses [native-theme and native-theme-gpui 0.7.1](https://github.com/tiborgats/native-theme)
(MIT OR Apache-2.0 OR 0BSD) and [gpui-component 0.9.1](https://github.com/longbridge/gpui-component)
(Apache-2.0). They are Cargo dependencies, used unpatched (native-theme 0.7.1
reads Windows fonts at 96 DPI itself, so the patched copy of 0.5.7 in
`vendor/native-theme` is gone), under the 0BSD option of the native-theme
license. Include their license notices and those of their transitive
dependencies when preparing a distributable binary.

AVIF previews decode with [rav1d 1.1.0](https://github.com/memorysafety/rav1d), the
Rust port of [dav1d](https://code.videolan.org/videolan/dav1d), licensed under the
BSD 2-Clause License (Copyright the dav1d and rav1d authors), and read the container
with [mp4parse](https://github.com/mozilla/mp4parse-rust) (MPL-2.0). Both are Cargo
dependencies and no C code is built. rav1d is used as a patched copy in `vendor/rav1d`
(see its `PATCHES.md`); its license text is kept there as `COPYING`.

UI icons in `assets/icons/ui/` are individual SVG files copied from
[Lucide](https://github.com/lucide-icons/lucide), licensed under the ISC License;
icons derived from Feather (including `link` and `external-link`) are
additionally under the MIT License. The upstream LICENSE, with both texts, is
retained verbatim in `licenses/LUCIDE-ISC.txt`; the icons in use are listed in
`assets/icons/ui/README.md`.

## Dependency crates in distributed binaries

The license texts and copyright notices of every Rust crate linked into a
distributed binary, including the transitive ones above, are collected in
`THIRD_PARTY_LICENSES.html`. The package workflow generates it with
`cargo about` (configured by `about.toml` and `about.hbs`) and puts it next to
this file in every package. It is not committed to the repository.

## BudouX Japanese model

`crates/model/data/budoux-ja-v0.9.3.json` is `budoux/models/ja.json` from
[BudouX v0.9.3](https://github.com/google/budoux/releases/tag/v0.9.3),
Copyright 2021 Google LLC, licensed under Apache License 2.0
(`licenses/BUDOUX-APACHE-2.0.txt`). It is embedded unchanged;
`crates/model/src/phrase.rs` is our own implementation of its scoring.
