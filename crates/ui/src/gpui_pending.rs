// SPDX-License-Identifier: GPL-3.0-only

//! Stand-ins for CayenChat's patches to GPUI 0.2.2 that the unpatched
//! `gpui-pre` 0.3.12 does not have (#1081, PR 1 of the GPUI Kit migration).
//!
//! Each item switches a feature off or keeps it minimal, so that the code
//! using it keeps compiling. The later PRs re-apply the patches to the split
//! GPUI crates and remove the item here with it; see the list in
//! `spec/gpui-kit-migration-2026-11-18.md`.

use std::ops::Range;

use gpui::{AnyView, App, InteractiveText, Pixels, Window};

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
