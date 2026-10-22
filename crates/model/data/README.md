# BudouX Japanese model

`budoux-ja-v0.9.3.json` is `budoux/models/ja.json` from Google's
[BudouX v0.9.3](https://github.com/google/budoux/releases/tag/v0.9.3)
(the 2026 Japanese model), Copyright 2021 Google LLC, Apache License 2.0
(`licenses/BUDOUX-APACHE-2.0.txt` at the repository root, which every
package ships). It is embedded unchanged and read by
`src/phrase.rs`, which reimplements the scoring of the official
`budoux/parser.py` (no code is copied).

To update the model:

1. Download `budoux/models/ja.json` from the new release tag.
2. Replace the JSON here (rename it to the new version) and update the
   `include_str!` path in `src/phrase.rs`.
3. Regenerate the expected splits in the `matches_official_parser` test with
   the release's `parser.py` (`Parser(model).parse(sentence)`) and run
   `cargo test -p cayenchat-model`.
