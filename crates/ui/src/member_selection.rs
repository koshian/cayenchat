//! Which members of the selected channel are chosen in the member list.
//!
//! Members are kept by nickname (without the `@`/`+` role prefix, compared the
//! way IRC compares nicknames), so the selection survives roster updates that
//! reorder or re-prefix the list. It belongs to one channel: asking about
//! another channel starts empty.

use cayenchat_irc_core::text::nickname_key;
use cayenchat_model::ConversationId;

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

impl MemberSelection {
    pub fn click(
        &mut self,
        conversation: ConversationId,
        members: &[String],
        index: usize,
        click: Click,
    ) {
        let Some(entry) = members.get(index) else {
            return;
        };
        if self.conversation != Some(conversation) {
            *self = Self::default();
            self.conversation = Some(conversation);
        }
        let key = nickname_key(nickname(entry));
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
                            .iter()
                            .position(|member| nickname_key(nickname(member)) == *anchor)
                    })
                    .unwrap_or(index);
                let (low, high) = (from.min(index), from.max(index));
                self.chosen = members[low..=high]
                    .iter()
                    .map(|member| nickname_key(nickname(member)))
                    .collect();
                // The anchor stays, so the range can be widened or narrowed.
            }
        }
    }

    /// Whether the roster entry `entry` of `conversation` is chosen.
    pub fn contains(&self, conversation: ConversationId, entry: &str) -> bool {
        self.conversation == Some(conversation)
            && self.chosen.contains(&nickname_key(nickname(entry)))
    }

    /// The chosen members of `conversation` that are still in `members`, as
    /// plain nicknames in list order.
    pub fn nicknames(&self, conversation: ConversationId, members: &[String]) -> Vec<String> {
        if self.conversation != Some(conversation) {
            return Vec::new();
        }
        members
            .iter()
            .map(|member| nickname(member))
            .filter(|name| self.chosen.contains(&nickname_key(name)))
            .map(str::to_owned)
            .collect()
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        selection.click(A, &members, 2, Click::Only);
        selection.click(A, &members, 3, Click::Only);
        assert_eq!(selection.nicknames(A, &members), ["Bob"]);
        assert!(selection.contains(A, "bob"), "nicknames compare like IRC");
        assert!(!selection.contains(A, "alice"));
    }

    #[test]
    fn toggling_adds_and_removes_one_member_at_a_time() {
        let (mut selection, members) = (MemberSelection::default(), roster());
        selection.click(A, &members, 0, Click::Only);
        selection.click(A, &members, 4, Click::Toggle);
        selection.click(A, &members, 2, Click::Toggle);
        assert_eq!(selection.nicknames(A, &members), ["op", "alice", "carol"]);
        selection.click(A, &members, 4, Click::Toggle);
        assert_eq!(selection.nicknames(A, &members), ["op", "alice"]);
    }

    #[test]
    fn a_range_runs_from_the_last_click_in_either_direction() {
        let (mut selection, members) = (MemberSelection::default(), roster());
        selection.click(A, &members, 1, Click::Only);
        selection.click(A, &members, 3, Click::Range);
        assert_eq!(selection.nicknames(A, &members), ["voiced", "alice", "Bob"]);
        // Same anchor: the range follows the new end, even upwards.
        selection.click(A, &members, 0, Click::Range);
        assert_eq!(selection.nicknames(A, &members), ["op", "voiced"]);
        // Without a previous click the range is the clicked member alone.
        let mut fresh = MemberSelection::default();
        fresh.click(A, &members, 3, Click::Range);
        assert_eq!(fresh.nicknames(A, &members), ["Bob"]);
    }

    #[test]
    fn the_selection_follows_nicknames_through_roster_changes() {
        let (mut selection, members) = (MemberSelection::default(), roster());
        selection.click(A, &members, 2, Click::Only);
        selection.click(A, &members, 3, Click::Toggle);
        // alice got op and moved; Bob left; a newcomer appeared.
        let later = ["@alice", "@op", "dave"].map(str::to_owned).to_vec();
        assert_eq!(selection.nicknames(A, &later), ["alice"]);
        assert!(selection.contains(A, "@alice"));
    }

    #[test]
    fn another_channel_starts_empty() {
        let (mut selection, members) = (MemberSelection::default(), roster());
        selection.click(A, &members, 2, Click::Only);
        assert!(selection.nicknames(B, &members).is_empty());
        assert!(!selection.contains(B, "alice"));
        selection.click(B, &members, 3, Click::Toggle);
        assert_eq!(selection.nicknames(B, &members), ["Bob"]);
        assert!(
            selection.nicknames(A, &members).is_empty(),
            "A was replaced"
        );
        selection.clear();
        assert!(selection.nicknames(B, &members).is_empty());
    }

    #[test]
    fn clicking_past_the_end_changes_nothing() {
        let (mut selection, members) = (MemberSelection::default(), roster());
        selection.click(A, &members, 2, Click::Only);
        selection.click(A, &members, 99, Click::Toggle);
        assert_eq!(selection.nicknames(A, &members), ["alice"]);
    }
}
