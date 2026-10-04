//! Standard keyboard movement for a focused list (channel tree, member list).
//!
//! These keys act only on the list that has focus, so they never reach the
//! draft, where `J`/`K` are text.

/// A movement a focused list understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListKey {
    Previous,
    Next,
    First,
    Last,
    PageUp,
    PageDown,
    /// Channel tree only: to the parent server / the first channel.
    Parent,
    Child,
    Activate,
}

impl ListKey {
    /// The movement for an unmodified key as GPUI names it.
    pub fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "up" | "k" => Self::Previous,
            "down" | "j" => Self::Next,
            "home" => Self::First,
            "end" => Self::Last,
            "pageup" => Self::PageUp,
            "pagedown" => Self::PageDown,
            "left" => Self::Parent,
            "right" => Self::Child,
            "enter" => Self::Activate,
            _ => return None,
        })
    }
}

/// The row to move to from `current` in a list of `len` rows, `page` rows per
/// page. Movement stops at the ends; with nothing current, forward keys start
/// at the first row and backward keys at the last.
pub fn target(current: Option<usize>, len: usize, key: ListKey, page: usize) -> Option<usize> {
    let last = len.checked_sub(1)?;
    let page = page.max(1);
    let to = match (key, current) {
        (ListKey::First, _) => 0,
        (ListKey::Last, _) => last,
        (ListKey::Next | ListKey::PageDown, None) => 0,
        (ListKey::Previous | ListKey::PageUp, None) => last,
        (ListKey::Next, Some(at)) => (at + 1).min(last),
        (ListKey::Previous, Some(at)) => at.saturating_sub(1),
        (ListKey::PageDown, Some(at)) => (at + page).min(last),
        (ListKey::PageUp, Some(at)) => at.saturating_sub(page),
        _ => return None,
    };
    Some(to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_movements() {
        assert_eq!(ListKey::from_key("j"), Some(ListKey::Next));
        assert_eq!(ListKey::from_key("k"), Some(ListKey::Previous));
        assert_eq!(ListKey::from_key("down"), Some(ListKey::Next));
        assert_eq!(ListKey::from_key("n"), None);
        assert_eq!(ListKey::from_key("p"), None);
    }

    #[test]
    fn movement_stops_at_the_ends() {
        assert_eq!(target(Some(0), 5, ListKey::Previous, 3), Some(0));
        assert_eq!(target(Some(4), 5, ListKey::Next, 3), Some(4));
        assert_eq!(target(Some(1), 5, ListKey::PageDown, 3), Some(4));
        assert_eq!(target(Some(1), 5, ListKey::PageUp, 3), Some(0));
        assert_eq!(target(Some(2), 5, ListKey::First, 3), Some(0));
        assert_eq!(target(Some(2), 5, ListKey::Last, 3), Some(4));
    }

    #[test]
    fn without_a_current_row_movement_starts_at_an_end() {
        assert_eq!(target(None, 5, ListKey::Next, 3), Some(0));
        assert_eq!(target(None, 5, ListKey::Previous, 3), Some(4));
        assert_eq!(target(None, 0, ListKey::Next, 3), None);
    }
}
