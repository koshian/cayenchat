//! Nicknames and channel names compared the way IRC servers compare them:
//! RFC 1459 case mapping, in which ASCII letters ignore case and `[]\~` are
//! the uppercase forms of `{}|^`. Other characters compare exactly. Every
//! feature uses these, so "the same channel" or "the same user" never
//! depends on which part of the application asks.

/// `ch` in its folded (lowercase) form. The UTF-8 length never changes.
pub fn fold_char(ch: char) -> char {
    match ch {
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        '~' => '^',
        ch => ch.to_ascii_lowercase(),
    }
}

/// `name` folded for use as a key. Byte offsets stay valid in the original.
pub fn fold(name: &str) -> String {
    name.chars().map(fold_char).collect()
}

/// Whether two names are the same under the case mapping, without
/// allocating.
pub fn same(left: &str, right: &str) -> bool {
    left.len() == right.len() && left.chars().map(fold_char).eq(right.chars().map(fold_char))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc1459_pairs_and_ascii_letters_fold_and_nothing_else() {
        assert!(same("#Foo[1]\\~", "#foo{1}|^"));
        assert_eq!(fold("#Foo[1]\\~"), "#foo{1}|^");
        // Only ASCII letters change case; other scripts compare exactly.
        assert!(!same("#Ä", "#ä"));
        assert!(same("#日本語", "#日本語"));
        assert!(!same("#a", "#ab"));
        assert_eq!(fold("#Ä日本").len(), "#Ä日本".len());
    }
}
