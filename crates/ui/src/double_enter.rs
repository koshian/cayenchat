// SPDX-License-Identifier: GPL-3.0-only

//! The "Enter twice" way of sending a chat draft (`SendKey::DoubleEnter`):
//! the first Enter only waits, a second one soon after sends. The state
//! belongs to the chat draft alone and starts over on anything else.

use cayenchat_app::Selection;
use std::{
    ops::Range,
    time::{Duration, Instant},
};

/// How long the first Enter waits for the second.
pub(crate) const WINDOW: Duration = Duration::from_millis(700);

#[derive(Debug, Default)]
pub(crate) struct DoubleEnter {
    pending: Option<Pending>,
    /// Enter is still down since the last press, so another press is the
    /// keyboard's auto-repeat. Not every platform marks repeats, so this
    /// follows the key-up instead.
    held: bool,
}

#[derive(Debug)]
struct Pending {
    selection: Selection,
    text: String,
    cursor: Range<usize>,
    at: Instant,
}

impl DoubleEnter {
    /// Forgets a waiting first Enter: another key, focus or channel change,
    /// a different send key, an empty or composing draft.
    pub(crate) fn reset(&mut self) {
        self.pending = None;
    }

    /// Enter was released.
    pub(crate) fn key_up(&mut self) {
        self.held = false;
    }

    /// The first Enter belongs to `selection`; it is dropped once the chat
    /// shows another conversation or the draft lost the focus.
    pub(crate) fn keep_only_for(&mut self, selection: Selection, focused: bool) {
        if !focused {
            self.held = false;
        }
        if !focused
            || self
                .pending
                .as_ref()
                .is_some_and(|p| p.selection != selection)
        {
            self.pending = None;
        }
    }

    /// A plain Enter in the draft of `selection` holding `text` with the
    /// caret at `cursor`. True when it completes a double press and the
    /// draft is to be sent; false when it only waits (or is a repeat).
    pub(crate) fn press(
        &mut self,
        selection: Selection,
        text: &str,
        cursor: Range<usize>,
        now: Instant,
    ) -> bool {
        if std::mem::replace(&mut self.held, true) {
            return false;
        }
        if let Some(first) = self.pending.take()
            && first.selection == selection
            && first.text == text
            && first.cursor == cursor
            && now.saturating_duration_since(first.at) <= WINDOW
        {
            return true;
        }
        self.pending = Some(Pending {
            selection,
            text: text.to_owned(),
            cursor,
            at: now,
        });
        false
    }

    #[cfg(test)]
    pub(crate) fn is_waiting(&self) -> bool {
        self.pending.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cayenchat_model::ConversationId as ChannelId;

    fn channel(id: u32) -> Selection {
        Selection::Channel(ChannelId(id))
    }

    #[test]
    fn the_second_press_sends_once_and_the_first_does_not() {
        let mut keys = DoubleEnter::default();
        let start = Instant::now();
        assert!(!keys.press(channel(1), "hi", 2..2, start));
        keys.key_up();
        assert!(keys.press(channel(1), "hi", 2..2, start + Duration::from_millis(300)));
        keys.key_up();
        // The next pair starts from scratch.
        assert!(!keys.press(channel(1), "hi", 2..2, start + Duration::from_millis(400)));
    }

    #[test]
    fn a_late_second_press_is_a_new_first_press() {
        let mut keys = DoubleEnter::default();
        let start = Instant::now();
        assert!(!keys.press(channel(1), "hi", 2..2, start));
        keys.key_up();
        let late = start + WINDOW + Duration::from_millis(1);
        assert!(!keys.press(channel(1), "hi", 2..2, late));
        keys.key_up();
        assert!(keys.press(channel(1), "hi", 2..2, late + Duration::from_millis(100)));
    }

    #[test]
    fn auto_repeat_is_not_a_second_press() {
        let mut keys = DoubleEnter::default();
        let start = Instant::now();
        assert!(!keys.press(channel(1), "hi", 2..2, start));
        // No key-up in between: the key is held down.
        for step in 1..5 {
            let at = start + Duration::from_millis(100 * step);
            assert!(!keys.press(channel(1), "hi", 2..2, at));
        }
        assert!(keys.is_waiting(), "the repeat does not cancel the wait");
    }

    #[test]
    fn editing_the_draft_or_moving_the_caret_starts_over() {
        let mut keys = DoubleEnter::default();
        let start = Instant::now();
        let soon = start + Duration::from_millis(100);
        assert!(!keys.press(channel(1), "hi", 2..2, start));
        keys.key_up();
        assert!(!keys.press(channel(1), "hi!", 3..3, soon));
        keys.key_up();
        assert!(!keys.press(channel(1), "hi!", 0..0, soon));
    }

    #[test]
    fn other_keys_channels_and_focus_reset() {
        let mut keys = DoubleEnter::default();
        let start = Instant::now();
        let soon = start + Duration::from_millis(100);

        assert!(!keys.press(channel(1), "hi", 2..2, start));
        keys.key_up();
        keys.reset();
        assert!(!keys.press(channel(1), "hi", 2..2, soon));
        keys.key_up();

        keys.keep_only_for(channel(2), true);
        assert!(!keys.is_waiting(), "another channel is shown");
        assert!(!keys.press(channel(2), "hi", 2..2, soon));
        keys.key_up();
        keys.keep_only_for(channel(2), false);
        assert!(!keys.is_waiting(), "the draft lost the focus");

        assert!(!keys.press(channel(2), "hi", 2..2, soon));
        keys.key_up();
        keys.keep_only_for(channel(2), true);
        assert!(keys.is_waiting(), "the same draft keeps waiting");
        // The same text in another channel is not the first press either.
        assert!(!keys.press(channel(3), "hi", 2..2, soon));
    }
}
