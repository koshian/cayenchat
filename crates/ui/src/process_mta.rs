// SPDX-License-Identifier: GPL-3.0-only
//! Keeps the process-wide COM multithreaded apartment (MTA) alive (#659).
//!
//! `windows-core` caches every WinRT activation factory for the whole process.
//! A factory first made on a thread that initialized COM is tied to that
//! thread's apartment; when that was the last user of the MTA and the thread
//! ends, the apartment is torn down and the cached factory dangles. The next
//! activation on any other thread then crashes (`0xc0000005`, seen on Windows
//! ARM64 in `UISettings::new`). A live `CoIncrementMTAUsage` keeps the MTA, and
//! so the cached factories, valid, and lets threads that never initialized COM
//! join it implicitly.

use std::sync::Once;

use windows_sys::Win32::System::Com::CoIncrementMTAUsage;

/// Takes the MTA reference once per process; the cookie is never released.
pub fn keep() {
    static KEEP: Once = Once::new();
    KEEP.call_once(|| {
        let mut cookie = std::ptr::null_mut();
        // SAFETY: `cookie` is a valid out pointer; the reference is
        // intentionally held until the process exits.
        let result = unsafe { CoIncrementMTAUsage(&mut cookie) };
        if result < 0 {
            eprintln!("CoIncrementMTAUsage failed: {result:#x}");
        }
    });
}
