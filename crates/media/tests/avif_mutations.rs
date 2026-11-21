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
