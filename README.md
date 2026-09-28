# CayenChat

[Japanese README](README.ja.md)

A compact native IRC desktop client written in Rust and GPUI, for macOS, Windows and Linux.

- Multiple servers at once, with per-server nickname, auto-join channels, TLS and SASL
- UTF-8, ISO-2022-JP, Shift_JIS and EUC-JP encodings
- Passwords kept in the OS credential store
- Image sharing through your own image hosting account (ImgBB)
- Japanese and English UI

Chat history is not saved.

## Build and run

Requires Rust 1.88 or later.

```sh
cargo run --locked -p cayenchat-ui
```

On macOS, you can also build a development `.app` bundle:

```sh
sh scripts/bundle-macos.sh
open target/CayenChat.app
```

See [development requirements](spec/development.md) for platform prerequisites and checks.

## Usage

Add a server in the settings window that opens at startup (`Cmd+,` / `Ctrl+,`), then choose **Connect**. See [keyboard shortcuts and IRC commands](SHORTCUTS.md).

## Documentation

The specifications under [`spec/`](spec/) are authoritative; start with [architecture](spec/architecture.md) and [feature parity](spec/feature-parity.md). To run the tests and view their coverage, see [How to test](HOW_TO_TEST.md).

## License

GPL version 3 only ([LICENSE](LICENSE)). `crates/ui/src/input.rs`, adapted from GPUI, remains Apache-2.0; third-party dependencies keep their own licenses ([notices](THIRD_PARTY_NOTICES.md)).
