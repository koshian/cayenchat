//! Nicknames and channel names compared the way IRC servers compare them.
//! The case mapping a server uses is advertised as `CASEMAPPING` in
//! RPL_ISUPPORT; RFC 1459 applies until it says otherwise. Nicknames are
//! always compared with RFC 1459 ([`fold`], [`same`]); channel names follow
//! the network's [`CaseMapping`] ([`CaseMapping::fold`], [`CaseMapping::same`])
//! so two channels the server keeps apart never share a conversation.

/// How a server folds names for comparison.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CaseMapping {
    /// ASCII letters only (`CASEMAPPING=ascii`). Also the safe choice for a
    /// mapping we do not know (for example `rfc7613`): it merges the fewest
    /// names.
    Ascii,
    /// ASCII letters, and `[]\` as the uppercase forms of `{}|`.
    Rfc1459Strict,
    /// `Rfc1459Strict` plus `~` as the uppercase form of `^`. The default
    /// when a server advertises nothing.
    #[default]
    Rfc1459,
}

impl CaseMapping {
    /// The mapping named by a `CASEMAPPING` value (case-insensitive).
    pub fn parse(value: &str) -> Self {
        if value.eq_ignore_ascii_case("rfc1459") {
            Self::Rfc1459
        } else if value.eq_ignore_ascii_case("rfc1459-strict") {
            Self::Rfc1459Strict
        } else {
            Self::Ascii
        }
    }

    /// `ch` in its folded (lowercase) form. The UTF-8 length never changes.
    pub fn fold_char(self, ch: char) -> char {
        match (self, ch) {
            (Self::Ascii, ch) => ch.to_ascii_lowercase(),
            (_, '[') => '{',
            (_, ']') => '}',
            (_, '\\') => '|',
            (Self::Rfc1459, '~') => '^',
            (_, ch) => ch.to_ascii_lowercase(),
        }
    }

    /// `name` folded for use as a key. Byte offsets stay valid in the
    /// original.
    pub fn fold(self, name: &str) -> String {
        name.chars().map(|ch| self.fold_char(ch)).collect()
    }

    /// Whether two names are the same under this mapping, without
    /// allocating.
    pub fn same(self, left: &str, right: &str) -> bool {
        left.len() == right.len()
            && left
                .chars()
                .map(|ch| self.fold_char(ch))
                .eq(right.chars().map(|ch| self.fold_char(ch)))
    }
}

/// `ch` in its RFC 1459 folded form.
pub fn fold_char(ch: char) -> char {
    CaseMapping::Rfc1459.fold_char(ch)
}

/// `name` folded with RFC 1459, for use as a key.
pub fn fold(name: &str) -> String {
    CaseMapping::Rfc1459.fold(name)
}

/// Whether two names are the same under RFC 1459.
pub fn same(left: &str, right: &str) -> bool {
    CaseMapping::Rfc1459.same(left, right)
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

    #[test]
    fn each_mapping_merges_only_its_own_pairs() {
        use CaseMapping::*;
        assert!(Ascii.same("#Foo", "#fOO"));
        assert!(!Ascii.same("#foo[1]", "#foo{1}"));
        assert!(!Ascii.same("#foo~", "#foo^"));
        assert!(Rfc1459Strict.same("#foo[1]\\", "#foo{1}|"));
        assert!(!Rfc1459Strict.same("#foo~", "#foo^"));
        assert!(Rfc1459.same("#foo~", "#foo^"));
        assert_eq!(Ascii.fold("#A[~"), "#a[~");
        assert_eq!(Rfc1459Strict.fold("#A[~"), "#a{~");
        assert_eq!(CaseMapping::default(), Rfc1459);
    }

    #[test]
    fn casemapping_values_parse_and_unknown_ones_merge_least() {
        assert_eq!(CaseMapping::parse("rfc1459"), CaseMapping::Rfc1459);
        assert_eq!(
            CaseMapping::parse("RFC1459-Strict"),
            CaseMapping::Rfc1459Strict
        );
        assert_eq!(CaseMapping::parse("ascii"), CaseMapping::Ascii);
        assert_eq!(CaseMapping::parse("rfc7613"), CaseMapping::Ascii);
    }
}
