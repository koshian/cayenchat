# Local rav1d 1.1.0 changes

This directory is a copy of the published rav1d 1.1.0 crate (the Rust port of
dav1d, BSD-2-Clause, `COPYING`). The workspace uses it through
`[patch.crates-io]` because AVIF previews (D018, `crates/media/src/avif.rs`)
decode untrusted files with it.

- `src/decode.rs`, `rav1d_submit_frame::on_error`: `f.frame_hdr.as_ref().unwrap()`
  becomes `is_some_and(..)`. Damaged AV1 data can reach this error path with the
  frame header already taken; the `unwrap` panicked inside `dav1d_send_data`,
  which is `extern "C"` and cannot unwind, so the whole process aborted. Found by
  `crates/media/tests/avif_mutations.rs`.
- `src/obu.rs`, `parse_frame_size`: with `frame_size_override` a frame larger
  than the sequence header's `max_width`/`max_height` is refused (EINVAL), as the
  AV1 specification requires of a conforming stream. Upstream only checks the
  area against `frame_size_limit`, so a small header could announce a much
  larger frame than the memory estimate in `avif.rs` was made for.
- `Cargo.toml`: `crate-type` is `rlib` only (no `staticlib`), and `[lints.rust] warnings = "allow"` as in the other copies.
- Not copied: `src/x86` and `src/arm` (the assembly, used only with the `asm`
  feature, which the workspace leaves off so that no assembler is needed),
  tests, CI files and the upstream `Cargo.lock`.
