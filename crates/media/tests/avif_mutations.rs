// SPDX-License-Identifier: MPL-2.0
//! Feeds damaged AVIF files to the preview decoder. `rav1d` is called through
//! its C ABI, where a panic aborts the process, so this test passing is the
//! evidence that damaged payloads end in an error and not in an abort.
//! `AVIF_MUTATIONS=<n>` raises the number of mutations per file and kind and
//! `AVIF_SEED=<n>` changes the sequence.

use cayenchat_media::{CancelFlag, Limits, decode};

const FIXTURES: [&[u8]; 4] = [
    include_bytes!("../src/testdata/opaque8.avif"),
    include_bytes!("../src/testdata/opaque10.avif"),
    include_bytes!("../src/testdata/alpha.avif"),
    include_bytes!("../src/testdata/c444.avif"),
];

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

/// Where the `mdat` box (the coded data) starts in `bytes`.
fn payload_start(bytes: &[u8]) -> usize {
    bytes
        .windows(4)
        .position(|window| window == b"mdat")
        .map_or(0, |at| at + 4)
}

fn check(bytes: &[u8], limits: &Limits) {
    if let Ok(thumbnail) = decode::thumbnail(bytes, limits, &CancelFlag::default()) {
        assert!(thumbnail.width <= limits.thumbnail_width);
        assert!(thumbnail.height <= limits.thumbnail_height);
        assert_eq!(
            thumbnail.bgra.len(),
            thumbnail.width as usize * thumbnail.height as usize * 4
        );
    }
}

#[test]
fn damaged_avif_files_never_abort_or_exceed_limits() {
    let rounds: usize = std::env::var("AVIF_MUTATIONS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(200);
    let limits = Limits::default();
    let seed: u64 = std::env::var("AVIF_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    let mut random = Random(seed.max(1));
    for original in FIXTURES {
        let payload = payload_start(original);
        for _ in 0..rounds {
            // Flip bytes anywhere.
            let mut bytes = original.to_vec();
            for _ in 0..=random.below(4) {
                let at = random.below(bytes.len());
                bytes[at] ^= 1 << random.below(8);
            }
            check(&bytes, &limits);

            // Overwrite bytes in the coded data, where the decoder works.
            let mut bytes = original.to_vec();
            for _ in 0..=random.below(8) {
                let at = payload + random.below(bytes.len() - payload);
                bytes[at] = random.next() as u8;
            }
            check(&bytes, &limits);

            // Cut it short.
            check(&original[..random.below(original.len())], &limits);

            // Drop a run of bytes from the coded data.
            let mut bytes = original.to_vec();
            let at = payload + random.below(bytes.len() - payload);
            let end = (at + 1 + random.below(16)).min(bytes.len());
            bytes.drain(at..end);
            check(&bytes, &limits);
        }
    }
}

const LARGE_FIXTURES: [&[u8]; 2] = [
    include_bytes!("../src/testdata/plasma4096.avif"),
    include_bytes!("../src/testdata/plasma4096_444_10.avif"),
];

fn env_number(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// One OBU found in the coded data: where its header starts, the length of
/// header plus size field, and the length of its payload.
struct Obu {
    start: usize,
    header_len: usize,
    payload_len: usize,
    kind: u8,
}

/// Reads OBUs (AV1 open bitstream units) one after another from the start of
/// `mdat` for as long as they parse; the primary and the alpha item follow each
/// other there. A scanner that stops early only gives fewer places to mutate.
fn scan_obus(bytes: &[u8]) -> Vec<Obu> {
    let mut found = Vec::new();
    let mut at = payload_start(bytes);
    while at < bytes.len() {
        let header = bytes[at];
        let has_size = header & 2 != 0;
        let extension = usize::from(header & 4 != 0);
        let mut cursor = at + 1 + extension;
        if !has_size {
            break;
        }
        let (mut size, mut shift) = (0usize, 0);
        loop {
            let Some(&byte) = bytes.get(cursor) else {
                return found;
            };
            cursor += 1;
            size |= usize::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 || shift > 28 {
                break;
            }
        }
        if cursor + size > bytes.len() {
            break;
        }
        found.push(Obu {
            start: at,
            header_len: cursor - at,
            payload_len: size,
            kind: (header >> 3) & 15,
        });
        at = cursor + size;
    }
    found
}

/// Damage that follows the AV1 structure: header bits of the sequence and frame
/// headers (sizes, profile, tiling, quantizer ...), OBU types and size fields,
/// and OBUs that are dropped, repeated or swapped, which plain byte noise
/// rarely produces without breaking the container first.
fn structured(original: &[u8], obus: &[Obu], random: &mut Random) -> Vec<u8> {
    let mut bytes = original.to_vec();
    let obu = &obus[random.below(obus.len())];
    match random.below(7) {
        // Bits in the first bytes of a header OBU (types 1 sequence, 3 frame
        // header, 6 frame), where the dimensions and coding tools are chosen.
        0..=2 => {
            let headers: Vec<_> = obus
                .iter()
                .filter(|obu| matches!(obu.kind, 1 | 3 | 6) && obu.payload_len > 0)
                .collect();
            let obu = headers
                .get(random.below(headers.len().max(1)))
                .copied()
                .unwrap_or(obu);
            for _ in 0..=random.below(3) {
                let span = obu.payload_len.clamp(1, 24);
                let at = obu.start
                    + obu.header_len
                    + random.below(span).min(obu.payload_len.saturating_sub(1));
                bytes[at] ^= 1 << random.below(8);
            }
        }
        // Another OBU type (reserved ones included).
        3 => bytes[obu.start] = (bytes[obu.start] & !0x78) | ((random.below(16) as u8) << 3),
        // A wrong size field inside the same bytes (all bit patterns of its
        // first byte, so it can grow, shrink or run on).
        4 if obu.header_len > 1 => {
            let at = obu.start + obu.header_len - 1;
            bytes[at] = random.next() as u8;
        }
        // Drop the OBU.
        5 => {
            bytes.drain(obu.start..obu.start + obu.header_len + obu.payload_len);
        }
        // Repeat the OBU right after itself.
        _ => {
            let end = obu.start + obu.header_len + obu.payload_len;
            let copy = original[obu.start..end].to_vec();
            bytes.splice(end..end, copy);
        }
    }
    bytes
}

/// Where the case being decoded is kept before it runs (`AVIF_LAST_CASE=<path>`),
/// because an abort leaves no test output: the file then holds the culprit.
fn remember(bytes: &[u8]) {
    if let Some(path) = std::env::var_os("AVIF_LAST_CASE") {
        std::fs::write(path, bytes).expect("write AVIF_LAST_CASE");
    }
}

fn run_structured(fixtures: &[&[u8]], rounds: usize, limits: &Limits) {
    let mut random = Random(env_number("AVIF_SEED", 0x9E37_79B9_7F4A_7C15).max(1));
    for original in fixtures {
        let obus = scan_obus(original);
        assert!(
            obus.iter().any(|obu| obu.kind == 1),
            "no sequence header found"
        );
        for _ in 0..rounds {
            let bytes = structured(original, &obus, &mut random);
            remember(&bytes);
            check(&bytes, limits);
        }
    }
}

/// The OBU scanner must keep finding the units the structured mutations aim at.
#[test]
fn the_small_fixtures_expose_their_av1_structure() {
    for original in FIXTURES.iter().chain(&LARGE_FIXTURES) {
        let obus = scan_obus(original);
        assert!(obus.iter().any(|obu| obu.kind == 1), "sequence header");
        assert!(
            obus.iter().any(|obu| obu.kind == 6 || obu.kind == 3),
            "frame"
        );
    }
}

#[test]
fn av1_structure_damage_never_aborts_or_exceeds_limits() {
    let rounds = env_number("AVIF_STRUCTURED", 400) as usize;
    run_structured(&FIXTURES, rounds, &Limits::default());
}

/// The 4096 × 4096 fixtures, where a decode takes a third of a second and more
/// in a debug build, so it is left out of the normal run:
/// `cargo test --release -p cayenchat-media --test avif_mutations -- --ignored`.
/// `AVIF_LARGE_MUTATIONS=<n>` sets the rounds per file and kind (default 40).
#[test]
#[ignore = "slow; run in release mode, see the doc comment"]
fn large_avif_damage_never_aborts_or_exceeds_limits() {
    let rounds = env_number("AVIF_LARGE_MUTATIONS", 40) as usize;
    let limits = Limits::default();
    run_structured(&LARGE_FIXTURES, rounds, &limits);
    let mut random = Random(env_number("AVIF_SEED", 0x9E37_79B9_7F4A_7C15).max(1));
    for original in LARGE_FIXTURES {
        let payload = payload_start(original);
        for _ in 0..rounds {
            // Coded data damaged, so the decoder runs into the tile data.
            let mut bytes = original.to_vec();
            for _ in 0..=random.below(8) {
                let at = payload + random.below(bytes.len() - payload);
                bytes[at] = random.next() as u8;
            }
            remember(&bytes);
            check(&bytes, &limits);
            // Cut short in the middle of the tile data.
            let cut = &original[..payload + random.below(original.len() - payload)];
            remember(cut);
            check(cut, &limits);
        }
    }
}
