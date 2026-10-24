// SPDX-License-Identifier: GPL-3.0-only
//! How a channel line of Reiwa mode leads: with the full header, or as the
//! continuation of the line above it.
//!
//! The lead is never stored on a message. It is derived from the line shown
//! just above, so older history inserted anywhere, a trimmed head or a
//! confirmed echo changes it without any bookkeeping.

use cayenchat_model::{Message, names::CaseMapping};

/// Messages further apart than this never join, whatever their minute: the
/// same "HH:MM" on another day is not a continuation.
const SAME_DAY_MILLIS: u64 = 24 * 60 * 60 * 1000;

/// What stands left of / above a Reiwa mode line. The discriminants are the
/// shape the virtualized list compares to find rows whose height changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Lead {
    /// Avatar, nickname and time above the text (two lines).
    Header = 0,
    /// Same speaker in the same minute: the text alone.
    Text = 1,
    /// Same speaker, another minute: the time in the avatar's column.
    Time = 2,
}

/// A plain message of a speaker, as opposed to activity, a NOTICE, an action
/// or a failed send, which end a group and keep their own look.
fn plain(message: &Message) -> bool {
    !(message.activity || message.notice || message.action || message.delivery_failed)
}

/// The lead of `message`, which is shown right below `above` (`None` for the
/// first line). Both are lines of one conversation of one network, whose
/// case mapping compares the nicknames.
pub fn lead(above: Option<&Message>, message: &Message, mapping: CaseMapping) -> Lead {
    let Some(above) = above else {
        return Lead::Header;
    };
    if !plain(above) || !plain(message) || !mapping.same(&above.sender, &message.sender) {
        return Lead::Header;
    }
    if let (Some(a), Some(b)) = (above.timestamp, message.timestamp)
        && a.as_millis().abs_diff(b.as_millis()) >= SAME_DAY_MILLIS
    {
        return Lead::Header;
    }
    if above.time == message.time {
        Lead::Text
    } else {
        Lead::Time
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cayenchat_model::{TimeOfDay, Timestamp};

    fn said(sender: &str, hour: u8, minute: u8) -> Message {
        Message {
            time: TimeOfDay::new(hour, minute),
            sequence: 0,
            timestamp: None,
            native_id: None,
            account: None,
            sender: sender.into(),
            text: String::new(),
            highlights: Default::default(),
            links: Default::default(),
            activity: false,
            alert: false,
            notice: false,
            action: false,
            provenance: Default::default(),
            delivery_failed: false,
        }
    }

    const MAP: CaseMapping = CaseMapping::Rfc1459;

    #[test]
    fn a_speaker_continues_in_the_same_or_another_minute() {
        let first = said("Alice", 1, 10);
        assert_eq!(lead(None, &first, MAP), Lead::Header);
        assert_eq!(lead(Some(&first), &said("alice", 1, 10), MAP), Lead::Text);
        assert_eq!(lead(Some(&first), &said("Alice", 1, 11), MAP), Lead::Time);
        assert_eq!(lead(Some(&first), &said("Bob", 1, 10), MAP), Lead::Header);
    }

    #[test]
    fn nicknames_follow_the_case_mapping() {
        let first = said("a[b", 1, 10);
        assert_eq!(lead(Some(&first), &said("A{B", 1, 10), MAP), Lead::Text);
        assert_eq!(
            lead(Some(&first), &said("A{B", 1, 10), CaseMapping::Ascii),
            Lead::Header
        );
    }

    #[test]
    fn other_kinds_of_line_break_the_group() {
        let first = said("Alice", 1, 10);
        for change in [
            (|m: &mut Message| m.activity = true) as fn(&mut Message),
            |m| m.notice = true,
            |m| m.action = true,
            |m| m.delivery_failed = true,
        ] {
            let mut other = said("Alice", 1, 10);
            change(&mut other);
            assert_eq!(lead(Some(&first), &other, MAP), Lead::Header);
            assert_eq!(lead(Some(&other), &first, MAP), Lead::Header);
        }
    }

    #[test]
    fn the_same_clock_time_on_another_day_does_not_join() {
        let at = |millis| Message {
            timestamp: Some(Timestamp::from_millis(millis)),
            ..said("Alice", 1, 10)
        };
        let day = SAME_DAY_MILLIS;
        assert_eq!(lead(Some(&at(1_000)), &at(1_000 + 5_000), MAP), Lead::Text);
        assert_eq!(lead(Some(&at(1_000)), &at(1_000 + day), MAP), Lead::Header);
        // Without a trusted time on one side no date is guessed.
        let bare = said("Alice", 1, 10);
        assert_eq!(lead(Some(&at(1_000)), &bare, MAP), Lead::Text);
    }
}
