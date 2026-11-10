// SPDX-License-Identifier: GPL-3.0-only
//! Which members of the selected channel are chosen in the member list.
//!
//! Members are kept by nickname (without the `@`/`+` role prefix, compared the
//! way IRC compares nicknames), so the selection survives roster updates that
//! reorder or re-prefix the list. It belongs to one channel: asking about
//! another channel starts empty.

use std::borrow::Cow;

use cayenchat_irc_core::text::{nickname_key, same_nickname};
use cayenchat_model::ConversationId;
use cayenchat_model::names::CaseMapping;

/// What a click on a row asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    /// Select only this member.
    Only,
    /// Add or remove this member (Cmd/Ctrl-click).
    Toggle,
    /// Select everything from the last clicked member to this one
    /// (Shift-click).
    Range,
}

#[derive(Debug, Default)]
pub struct MemberSelection {
    conversation: Option<ConversationId>,
    /// Nickname keys of the chosen members.
    chosen: Vec<String>,
    /// Where a range starts: the key of the member clicked last.
    anchor: Option<String>,
}

/// The nickname of a roster entry, without its role prefix.
pub fn nickname(entry: &str) -> &str {
    entry.trim_start_matches(['~', '&', '@', '%', '+'])
}

/// The rows of the member list in display order.
pub trait Roster {
    fn entries(&self) -> impl Iterator<Item = &String>;
}

impl Roster for [String] {
    fn entries(&self) -> impl Iterator<Item = &String> {
        self.iter()
    }
}

impl Roster for Vec<String> {
    fn entries(&self) -> impl Iterator<Item = &String> {
        self.iter()
    }
}

/// What the member list shows: the roster with our own row moved to the top.
/// The roster is borrowed as it is, so drawing never copies it; a row's
/// index here is its position on screen.
pub struct Participants<'a> {
    rows: Cow<'a, [String]>,
    /// Index in `rows` of the row shown first, when it is not row 0.
    first: Option<usize>,
}

impl<'a> Participants<'a> {
    /// `rows` as they are.
    pub fn plain(rows: Cow<'a, [String]>) -> Self {
        Self { rows, first: None }
    }

    /// `members` with our own entry (`own`, compared as IRC nicknames, role
    /// prefix ignored) first and the others in their order. Unchanged when we
    /// are not listed.
    pub fn own_first(mapping: CaseMapping, members: &'a [String], own: Option<&str>) -> Self {
        let first = own.and_then(|own| {
            members
                .iter()
                .position(|member| same_nickname(mapping, nickname(member), own))
        });
        Self {
            rows: Cow::Borrowed(members),
            first: first.filter(|&at| at != 0),
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn get(&self, index: usize) -> Option<&String> {
        let at = match self.first {
            Some(first) if index == 0 => first,
            Some(first) if index <= first => index - 1,
            _ => index,
        };
        self.rows.get(at)
    }
}

impl Roster for Participants<'_> {
    fn entries(&self) -> impl Iterator<Item = &String> {
        let first = self.first.and_then(|at| self.rows.get(at));
        let skipped = self.first;
        first.into_iter().chain(
            self.rows
                .iter()
                .enumerate()
                .filter(move |(at, _)| Some(*at) != skipped)
                .map(|(_, row)| row),
        )
    }
}

impl MemberSelection {
    pub fn click(
        &mut self,
        mapping: CaseMapping,
        conversation: ConversationId,
        members: &(impl Roster + ?Sized),
        index: usize,
        click: Click,
    ) {
        let Some(entry) = members.entries().nth(index) else {
            return;
        };
        if self.conversation != Some(conversation) {
            *self = Self::default();
            self.conversation = Some(conversation);
        }
        let key = nickname_key(mapping, nickname(entry));
        match click {
            Click::Only => {
                self.chosen = vec![key.clone()];
                self.anchor = Some(key);
            }
            Click::Toggle => {
                if let Some(position) = self.chosen.iter().position(|chosen| *chosen == key) {
                    self.chosen.remove(position);
                } else {
                    self.chosen.push(key.clone());
                }
                self.anchor = Some(key);
            }
            Click::Range => {
                let from = self
                    .anchor
                    .as_ref()
                    .and_then(|anchor| {
                        members
                            .entries()
                            .position(|member| nickname_key(mapping, nickname(member)) == *anchor)
                    })
                    .unwrap_or(index);
                let (low, high) = (from.min(index), from.max(index));
                self.chosen = members
                    .entries()
                    .skip(low)
                    .take(high - low + 1)
                    .map(|member| nickname_key(mapping, nickname(member)))
                    .collect();
                // The anchor stays, so the range can be widened or narrowed.
            }
        }
    }

    /// The row of the member chosen last, where keyboard movement starts.
    pub fn anchor_index(
        &self,
        mapping: CaseMapping,
        conversation: ConversationId,
        members: &(impl Roster + ?Sized),
    ) -> Option<usize> {
        if self.conversation != Some(conversation) {
            return None;
        }
        let anchor = self.anchor.as_ref()?;
        members
            .entries()
            .position(|member| nickname_key(mapping, nickname(member)) == *anchor)
    }

    /// Forgets the choice, e.g. when the member list loses focus.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Whether the roster entry `entry` of `conversation` is chosen.
    pub fn contains(
        &self,
        mapping: CaseMapping,
        conversation: ConversationId,
        entry: &str,
    ) -> bool {
        self.conversation == Some(conversation)
            && self
                .chosen
                .contains(&nickname_key(mapping, nickname(entry)))
    }

    /// Forgets chosen members who are no longer in `members`. Called with
    /// every new roster: nicknames are reused, so a member who left (or
    /// changed nickname) must not stay chosen for whoever takes the nickname
    /// next, who would then receive whatever is done to the choice.
    pub fn retain_present(
        &mut self,
        mapping: CaseMapping,
        conversation: ConversationId,
        members: &[String],
    ) {
        if self.conversation != Some(conversation)
            || (self.chosen.is_empty() && self.anchor.is_none())
        {
            return;
        }
        let present: Vec<String> = members
            .iter()
            .map(|member| nickname_key(mapping, nickname(member)))
            .collect();
        self.chosen.retain(|key| present.contains(key));
        if self
            .anchor
            .as_ref()
            .is_some_and(|anchor| !present.contains(anchor))
        {
            self.anchor = None;
        }
    }

    /// How many members of `conversation` are chosen. Chosen members who left
    /// are dropped with every new roster (`retain_present`), so this needs no
    /// roster.
    pub fn count(&self, conversation: ConversationId) -> usize {
        if self.conversation == Some(conversation) {
            self.chosen.len()
        } else {
            0
        }
    }

    /// The chosen members of `conversation` that are still in `members`, as
    /// plain nicknames in list order.
    pub fn nicknames(
        &self,
        mapping: CaseMapping,
        conversation: ConversationId,
        members: &(impl Roster + ?Sized),
    ) -> Vec<String> {
        if self.conversation != Some(conversation) {
            return Vec::new();
        }
        members
            .entries()
            .map(|member| nickname(member))
            .filter(|name| self.chosen.contains(&nickname_key(mapping, name)))
            .map(str::to_owned)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: CaseMapping = CaseMapping::Rfc1459;
    const A: ConversationId = ConversationId(1);
    const B: ConversationId = ConversationId(2);

    fn roster() -> Vec<String> {
        ["@op", "+voiced", "alice", "Bob", "carol"]
            .map(str::to_owned)
            .to_vec()
    }

    #[test]
    fn a_plain_click_selects_only_that_member() {
        let (mut selection, members) = (MemberSelection::default(), roster());
        selection.click(M, A, &members, 2, Click::Only);
        selection.click(M, A, &members, 3, Click::Only);
        assert_eq!(selection.nicknames(M, A, &members), ["Bob"]);
        assert!(
            selection.contains(M, A, "bob"),
            "nicknames compare like IRC"
        );
        assert!(!selection.contains(M, A, "alice"));
    }

    #[test]
    fn toggling_adds_and_removes_one_member_at_a_time() {
        let (mut selection, members) = (MemberSelection::default(), roster());
        selection.click(M, A, &members, 0, Click::Only);
        selection.click(M, A, &members, 4, Click::Toggle);
        selection.click(M, A, &members, 2, Click::Toggle);
        assert_eq!(
            selection.nicknames(M, A, &members),
            ["op", "alice", "carol"]
        );
        selection.click(M, A, &members, 4, Click::Toggle);
        assert_eq!(selection.nicknames(M, A, &members), ["op", "alice"]);
    }

    #[test]
    fn a_range_runs_from_the_last_click_in_either_direction() {
        let (mut selection, members) = (MemberSelection::default(), roster());
        selection.click(M, A, &members, 1, Click::Only);
        selection.click(M, A, &members, 3, Click::Range);
        assert_eq!(
            selection.nicknames(M, A, &members),
            ["voiced", "alice", "Bob"]
        );
        // Same anchor: the range follows the new end, even upwards.
        selection.click(M, A, &members, 0, Click::Range);
        assert_eq!(selection.nicknames(M, A, &members), ["op", "voiced"]);
        // Without a previous click the range is the clicked member alone.
        let mut fresh = MemberSelection::default();
        fresh.click(M, A, &members, 3, Click::Range);
        assert_eq!(fresh.nicknames(M, A, &members), ["Bob"]);
    }

    #[test]
    fn the_selection_follows_nicknames_through_roster_changes() {
        let (mut selection, members) = (MemberSelection::default(), roster());
        selection.click(M, A, &members, 2, Click::Only);
        selection.click(M, A, &members, 3, Click::Toggle);
        // alice got op and moved; Bob left; a newcomer appeared.
        let later = ["@alice", "@op", "dave"].map(str::to_owned).to_vec();
        assert_eq!(selection.nicknames(M, A, &later), ["alice"]);
        assert!(selection.contains(M, A, "@alice"));
    }

    #[test]
    fn count_is_the_chosen_members_of_that_channel_only() {
        let (mut selection, members) = (MemberSelection::default(), roster());
        assert_eq!(selection.count(A), 0);
        selection.click(M, A, &members, 2, Click::Only);
        assert_eq!(selection.count(A), 1);
        selection.click(M, A, &members, 3, Click::Toggle);
        assert_eq!(selection.count(A), 2);
        assert_eq!(selection.count(B), 0);
        // Members who left are dropped, so the count needs no roster.
        selection.retain_present(M, A, &["@alice".to_owned()]);
        assert_eq!(selection.count(A), 1);
    }

    #[test]
    fn another_channel_starts_empty() {
        let (mut selection, members) = (MemberSelection::default(), roster());
        selection.click(M, A, &members, 2, Click::Only);
        assert!(selection.nicknames(M, B, &members).is_empty());
        assert!(!selection.contains(M, B, "alice"));
        selection.click(M, B, &members, 3, Click::Toggle);
        assert_eq!(selection.nicknames(M, B, &members), ["Bob"]);
        assert!(
            selection.nicknames(M, A, &members).is_empty(),
            "A was replaced"
        );
    }

    #[test]
    fn a_member_who_leaves_is_forgotten_even_if_the_nickname_comes_back() {
        let (mut selection, members) = (MemberSelection::default(), roster());
        selection.click(M, A, &members, 2, Click::Only); // alice
        selection.click(M, A, &members, 3, Click::Toggle); // Bob
        // alice quits; the roster no longer has her.
        let without_alice: Vec<String> = members
            .iter()
            .filter(|member| nickname(member) != "alice")
            .cloned()
            .collect();
        selection.retain_present(M, A, &without_alice);
        // Someone else takes the nickname: not chosen.
        selection.retain_present(M, A, &members);
        assert!(!selection.contains(M, A, "alice"));
        assert_eq!(selection.nicknames(M, A, &members), ["Bob"], "Bob stays");
        // A nickname change is a leave as well as a join: the old name is
        // forgotten and the new one is not chosen.
        let renamed = ["@op", "+voiced", "alice", "Robert", "carol"]
            .map(str::to_owned)
            .to_vec();
        selection.retain_present(M, A, &renamed);
        assert!(selection.nicknames(M, A, &renamed).is_empty());
        assert!(!selection.contains(M, A, "Robert"));
        // Other conversations' rosters do not touch this choice.
        selection.click(M, A, &members, 0, Click::Only);
        selection.retain_present(M, B, &[]);
        assert_eq!(selection.nicknames(M, A, &members), ["op"]);
    }

    #[test]
    fn a_range_cannot_start_from_a_member_who_left() {
        let (mut selection, members) = (MemberSelection::default(), roster());
        selection.click(M, A, &members, 1, Click::Only); // voiced is the anchor
        let without: Vec<String> = members[2..].to_vec();
        selection.retain_present(M, A, &without);
        // With the anchor gone, a range is the clicked member alone.
        selection.click(M, A, &without, 1, Click::Range);
        assert_eq!(selection.nicknames(M, A, &without), ["Bob"]);
    }

    #[test]
    fn clicking_past_the_end_changes_nothing() {
        let (mut selection, members) = (MemberSelection::default(), roster());
        selection.click(M, A, &members, 2, Click::Only);
        selection.click(M, A, &members, 99, Click::Toggle);
        assert_eq!(selection.nicknames(M, A, &members), ["alice"]);
    }
}
