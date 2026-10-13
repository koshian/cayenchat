// SPDX-License-Identifier: GPL-3.0-only
use super::newest_lines;
use cayenchat_model::{Conversation, ConversationId, Message, NetworkId, TimeOfDay};

/// The previous implementation: every tail collected, then sorted.
fn collect_and_sort(
    conversations: &[Conversation],
    excluded: Option<ConversationId>,
    limit: usize,
) -> Vec<(u64, ConversationId, usize)> {
    let mut rows: Vec<_> = conversations
        .iter()
        .filter(|c| Some(c.id) != excluded)
        .flat_map(|c| {
            c.messages
                .iter()
                .enumerate()
                .rev()
                .filter(|(_, m)| {
                    !m.activity && m.provenance != cayenchat_model::Provenance::Requested
                })
                .take(limit)
                .map(move |(index, m)| (m.sequence, c.id, index))
        })
        .collect();
    rows.sort_unstable_by_key(|(sequence, _, _)| *sequence);
    rows.drain(..rows.len().saturating_sub(limit));
    rows
}

#[test]
fn merged_tails_match_collecting_and_sorting() {
    // Interleaved arrivals across three conversations on two networks,
    // with activity lines and an empty conversation.
    let mut conversations: Vec<Conversation> = (0..4)
        .map(|id| Conversation {
            id: ConversationId(id + 1),
            network: NetworkId(id / 2 + 1),
            kind: cayenchat_model::ConversationKind::Channel,
            name: format!("#c{id}"),
            topic: String::new(),
            messages: Vec::new(),
            members: Vec::new(),
        })
        .collect();
    for sequence in 1..=500u64 {
        let target = [0, 1, 1, 2, 0, 2, 1][sequence as usize % 7];
        conversations[target].messages.push(Message {
            time: TimeOfDay::new(0, 0),
            sequence,
            timestamp: None,
            native_id: None,
            account: None,
            delivery_failed: false,
            sender: "bob".into(),
            text: String::new(),
            highlights: Default::default(),
            links: Default::default(),
            activity: sequence % 5 == 0,
            alert: false,
            notice: false,
            provenance: if sequence % 11 == 0 {
                cayenchat_model::Provenance::Requested
            } else {
                cayenchat_model::Provenance::Live
            },
        });
    }
    for excluded in [None, Some(ConversationId(2)), Some(ConversationId(4))] {
        for limit in [0, 1, 7, 100, 1_000] {
            assert_eq!(
                newest_lines(&conversations, excluded, limit),
                collect_and_sort(&conversations, excluded, limit),
                "excluded {excluded:?}, limit {limit}"
            );
        }
    }
}
