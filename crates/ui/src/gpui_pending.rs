// SPDX-License-Identifier: GPL-3.0-only

//! Stand-ins for CayenChat's patches to GPUI 0.2.2 that the unpatched
//! `gpui-pre` 0.3.12 does not have (#1081, PR 1 of the GPUI Kit migration).
//!
//! Each item switches a feature off or keeps it minimal, so that the code
//! using it keeps compiling. The later PRs re-apply the patches to the split
//! GPUI crates and remove the item here with it; see the list in
//! `spec/gpui-kit-migration-2026-11-18.md`.

use std::{ops::Range, path::PathBuf, sync::Arc};

use gpui::{AnyView, App, ExternalPaths, InteractiveText, Pixels, StyledText, Window};

/// Phrase wrapping (`StyledText::with_wrap_joins`, #537, #550) needs the text
/// patch. Off, the log wraps like stock GPUI and no joins are computed.
pub(crate) const PHRASE_WRAP: bool = false;

/// A temporary directory holding files received from a macOS file promise,
/// removed with the last handle (the macOS file drop patch's type).
pub(crate) struct PromisedFiles(PathBuf);

impl PromisedFiles {
    /// Takes ownership of `directory`, which is removed with the last handle.
    #[cfg(test)]
    pub(crate) fn new(directory: PathBuf) -> Self {
        Self(directory)
    }
}

impl Drop for PromisedFiles {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The handle `ExternalPaths::keep_alive` gave: the macOS drop of promised
/// files is not patched in yet, so there is none and the files of such a drop
/// do not arrive.
pub(crate) fn keep_alive(_paths: &ExternalPaths) -> Option<Arc<PromisedFiles>> {
    None
}

/// The window methods of the background blur patch (#930, #1018, D091).
/// Without it no platform offers a blur, and a transparent window is always
/// taken to show what lies behind it.
pub(crate) trait WindowPending {
    fn is_compositing(&self) -> bool;
    fn supports_background_blur(&self) -> bool;
    fn supports_background_blur_radius(&self) -> bool;
    fn set_background_blur_radius(&mut self, radius: u32);
}

impl WindowPending for Window {
    fn is_compositing(&self) -> bool {
        true
    }

    fn supports_background_blur(&self) -> bool {
        false
    }

    fn supports_background_blur_radius(&self) -> bool {
        false
    }

    fn set_background_blur_radius(&mut self, _radius: u32) {}
}

/// The link tooltip of the text patch: the tooltip a pointer can move onto
/// (to click the copy button of a decoded character reference). Stock GPUI's
/// tooltip is not hoverable, so it vanishes before the button can be used.
pub(crate) trait InteractiveTextPending: Sized {
    fn hoverable_tooltip(
        self,
        anchor: impl Fn(usize) -> Option<Range<usize>> + 'static,
        builder: impl Fn(Range<usize>, &mut Window, &mut App) -> AnyView + 'static,
    ) -> Self;

    fn tooltip_bridge(self, left: Pixels, top: Pixels) -> Self;
}

impl InteractiveTextPending for InteractiveText {
    fn hoverable_tooltip(
        self,
        anchor: impl Fn(usize) -> Option<Range<usize>> + 'static,
        builder: impl Fn(Range<usize>, &mut Window, &mut App) -> AnyView + 'static,
    ) -> Self {
        self.tooltip(move |index, window, cx| anchor(index).map(|range| builder(range, window, cx)))
    }

    fn tooltip_bridge(self, _left: Pixels, _top: Pixels) -> Self {
        self
    }
}

/// Phrase wrapping of the text patch (#537, #550): GPUI's line wrapper does
/// not break the text at these offsets. Without the patch nothing is joined.
pub(crate) trait StyledTextPending: Sized {
    fn with_wrap_joins(self, joins: Arc<[u32]>) -> Self;
}

impl StyledTextPending for StyledText {
    fn with_wrap_joins(self, _joins: Arc<[u32]>) -> Self {
        self
    }
}
