// SPDX-License-Identifier: MPL-2.0
//! Measures allocations made by the public media decode entry points.

use std::alloc::{GlobalAlloc, Layout, System};
use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use cayenchat_media::{CancelFlag, Limits, avatar_edit, decode};

struct TrackingAllocator;

static TRACK: AtomicBool = AtomicBool::new(false);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static LARGEST: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            record_allocation(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            record_allocation(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(pointer, layout, size) };
        if !result.is_null() {
            LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
            record_allocation(size);
        }
        result
    }
}

fn record_allocation(size: usize) {
    let live = LIVE.fetch_add(size, Ordering::SeqCst) + size;
    PEAK.fetch_max(live, Ordering::SeqCst);
    if TRACK.load(Ordering::SeqCst) {
        LARGEST.fetch_max(size, Ordering::SeqCst);
    }
}

/// The counters are process-wide, so each test holds this for its whole run.
static MEASURING: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn within_budget<T>(budget: usize, decode: impl FnOnce() -> T) -> T {
    let baseline = LIVE.load(Ordering::SeqCst);
    PEAK.store(baseline, Ordering::SeqCst);
    LARGEST.store(0, Ordering::SeqCst);
    TRACK.store(true, Ordering::SeqCst);
    let result = decode();
    TRACK.store(false, Ordering::SeqCst);

    let largest = LARGEST.load(Ordering::SeqCst);
    let peak = PEAK.load(Ordering::SeqCst).saturating_sub(baseline);
    assert!(largest <= budget, "single allocation {largest} > {budget}");
    assert!(peak <= budget, "simultaneous allocations {peak} > {budget}");
    result
}

fn png_with_icc(profile_bytes: usize) -> Vec<u8> {
    let mut info = png::Info::with_size(1, 1);
    info.color_type = png::ColorType::Rgb;
    info.bit_depth = png::BitDepth::Eight;
    info.icc_profile = Some(Cow::Owned(vec![0; profile_bytes]));
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::with_info(&mut bytes, info).unwrap();
    encoder.set_compression(png::Compression::High);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&[255, 0, 0]).unwrap();
    drop(writer);
    bytes
}

fn png_with_text(width: u32, height: u32, text_bytes: usize) -> Vec<u8> {
    let mut info = png::Info::with_size(width, height);
    info.color_type = png::ColorType::Rgba;
    info.bit_depth = png::BitDepth::Eight;
    info.uncompressed_latin1_text
        .push(png::text_metadata::TEXtChunk::new(
            "Comment",
            "x".repeat(text_bytes),
        ));
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::with_info(&mut bytes, info).unwrap();
    encoder.set_compression(png::Compression::High);
    let mut writer = encoder.write_header().unwrap();
    writer
        .write_image_data(&vec![0; width as usize * height as usize * 4])
        .unwrap();
    drop(writer);
    bytes
}

#[test]
fn image_allocations_obey_decode_budgets() {
    let _serial = MEASURING.lock().unwrap_or_else(|e| e.into_inner());
    let png = png_with_icc(2 * 1024 * 1024);
    let preview_budget = 1024 * 1024;
    let limits = Limits {
        max_decode_bytes: preview_budget as u64,
        ..Limits::default()
    };
    let thumbnail = within_budget(preview_budget, || {
        decode::thumbnail(&png, &limits, &CancelFlag::default())
    });
    assert!(thumbnail.is_ok());

    assert!(within_budget(24 * 1024 * 1024, || {
        decode::is_small_original(&png)
    }));
    assert!(within_budget(48 * 1024 * 1024, || avatar_edit::open(&png)).is_ok());

    let gif = include_bytes!("../src/testdata/partial-frame.gif");
    let limits = Limits {
        max_source_pixels: 3000 * 3000,
        ..Limits::default()
    };
    let result = within_budget(48 * 1024 * 1024, || {
        decode::thumbnail(gif, &limits, &CancelFlag::default())
    });
    assert_eq!(result, Err(cayenchat_media::LoadError::TooLarge));

    let text_png = png_with_text(1, 1, 1536 * 1024);
    let small_budget = 1024 * 1024;
    let limits = Limits {
        max_decode_bytes: small_budget as u64,
        ..Limits::default()
    };
    let result = within_budget(small_budget, || {
        decode::thumbnail(&text_png, &limits, &CancelFlag::default())
    });
    assert_eq!(result, Err(cayenchat_media::LoadError::TooLarge));

    let raster_png = png_with_text(4096, 2800, 7 * 1024 * 1024);
    let result = within_budget(48 * 1024 * 1024, || {
        decode::thumbnail(&raster_png, &Limits::default(), &CancelFlag::default())
    });
    assert_eq!(result, Err(cayenchat_media::LoadError::TooLarge));
    let result = within_budget(48 * 1024 * 1024, || avatar_edit::open(&raster_png));
    assert!(matches!(result, Err(cayenchat_media::LoadError::TooLarge)));

    let original_png = png_with_text(2048, 3200, 64 * 1024);
    assert!(original_png.len() <= 256 * 1024);
    assert!(!within_budget(24 * 1024 * 1024, || {
        decode::is_small_original(&original_png)
    }));

    let (width, height) = (4096u32, 2800u32);
    let mut info = png::Info::with_size(width, height);
    info.color_type = png::ColorType::Rgba;
    info.bit_depth = png::BitDepth::Eight;
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::with_info(&mut bytes, info).unwrap();
    encoder.set_compression(png::Compression::High);
    let mut writer = encoder.write_header().unwrap();
    writer
        .write_image_data(&vec![255; width as usize * height as usize * 4])
        .unwrap();
    drop(writer);

    // The decoded source (at most 48 MiB) plus the 2048 × 1400 result; no
    // floating-point intermediate of the resize.
    let source = within_budget(64 * 1024 * 1024, || avatar_edit::open(&bytes)).unwrap();
    assert_eq!((source.width(), source.height()), (2048, 1400));
}

#[test]
fn decoder_construction_does_not_allocate_raster_sized_memory() {
    let _serial = MEASURING.lock().unwrap_or_else(|e| e.into_inner());
    // The header declares a 4096 × 2800 RGBA raster (about 44 MiB) while the
    // data itself compresses to a few kilobytes. Rejecting it against a
    // 16 MiB decode budget must not allocate anything near the raster size,
    // so the decoder's construction itself is cheap.
    let png = png_with_text(4096, 2800, 0);
    assert!(png.len() < 256 * 1024);
    let limits = Limits {
        max_decode_bytes: 16 * 1024 * 1024,
        ..Limits::default()
    };
    let result = within_budget(1024 * 1024, || {
        decode::thumbnail(&png, &limits, &CancelFlag::default())
    });
    assert_eq!(result, Err(cayenchat_media::LoadError::TooLarge));
}

#[test]
fn retained_exif_is_charged_to_the_decode_budget() {
    let _serial = MEASURING.lock().unwrap_or_else(|e| e.into_inner());
    // `png` retains eXIf twice for the decoder's lifetime and its internal
    // allocation limit is fixed at construction, so the 6 MiB of eXIf must be
    // charged (twice) before the 44 MiB raster, which then no longer fits.
    let (width, height) = (4096u32, 2800u32);
    let mut info = png::Info::with_size(width, height);
    info.color_type = png::ColorType::Rgba;
    info.bit_depth = png::BitDepth::Eight;
    info.exif_metadata = Some(Cow::Owned(vec![0; 6 * 1024 * 1024]));
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::with_info(&mut bytes, info).unwrap();
    encoder.set_compression(png::Compression::High);
    let mut writer = encoder.write_header().unwrap();
    writer
        .write_image_data(&vec![0; width as usize * height as usize * 4])
        .unwrap();
    drop(writer);

    let result = within_budget(48 * 1024 * 1024, || {
        decode::thumbnail(&bytes, &Limits::default(), &CancelFlag::default())
    });
    assert_eq!(result, Err(cayenchat_media::LoadError::TooLarge));
}
