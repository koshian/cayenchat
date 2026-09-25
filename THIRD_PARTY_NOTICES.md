# Third-party notices

The application depends on [GPUI 0.2.2](https://docs.rs/crate/gpui/0.2.2),
Copyright Zed Industries, licensed under Apache License 2.0. In addition,
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
