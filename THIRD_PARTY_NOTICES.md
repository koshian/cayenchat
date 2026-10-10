# Third-party notices

The application uses a local copy of [GPUI 0.2.2](https://docs.rs/crate/gpui/0.2.2)
in `vendor/gpui`, Copyright Zed Industries, licensed under Apache License 2.0.
The Windows keyboard-message handling in this copy is patched as described in
`vendor/gpui/PATCHES.md`. In addition,
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

Settings appearance uses [native-theme and native-theme-gpui 0.5.7](https://github.com/tiborgats/native-theme)
(MIT OR Apache-2.0 OR 0BSD) and [gpui-component 0.5.1](https://github.com/longbridge/gpui-component)
(Apache-2.0). They are Cargo dependencies. native-theme 0.5.7 is used as a
patched copy in `vendor/native-theme` (Windows DPI fix backported from 0.6.1;
see its `PATCHES.md`), used under the 0BSD option of its license; the upstream
license texts are kept in that directory. Include their license notices and
those of their transitive dependencies when preparing a distributable binary.

UI icons in `assets/icons/ui/` are individual SVG files copied from
[Lucide](https://github.com/lucide-icons/lucide), licensed under the ISC License;
icons derived from Feather (including `link` and `external-link`) are
additionally under the MIT License. The upstream LICENSE, with both texts, is
retained verbatim in `licenses/LUCIDE-ISC.txt`; the icons in use are listed in
`assets/icons/ui/README.md`.
