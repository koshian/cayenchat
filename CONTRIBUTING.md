# Contributing

[日本語](CONTRIBUTING.ja.md)

Read [AGENTS.md](AGENTS.md) and the specifications under [`spec/`](spec/)
before changing code; they describe how the project is built, tested and
reviewed.

## Licensing of contributions

This repository has two licenses (see [README](README.md#license) and
decision D042 in [`spec/decisions.md`](spec/decisions.md)):

- the core crates `crates/model`, `irc-core`, `storage`, `media`, `upload`
  and `app` are under the Mozilla Public License 2.0;
- the GPUI application `crates/ui` and everything else are under GPL
  version 3 only.

By submitting a contribution, you agree that:

1. a contribution to a core crate is licensed under MPL-2.0;
2. a contribution to any other part is licensed under GPL-3.0-only **and
   also under MPL-2.0**, so that it can later move into a core crate (for
   example when application logic moves from `ui` into `app`) without
   asking you again;
3. you have the right to license it this way: it is your own work, or you
   state its origin and license in the pull request.

Do not copy code from other projects into a core crate. Code adapted from
elsewhere is only accepted in a part whose license allows it, with its
origin and license recorded in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
