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

#[test]
fn png_metadata_and_gif_intermediate_allocations_obey_decode_budgets() {
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
}
