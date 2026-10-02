//! Privilege changes for several members at once, split into `MODE` lines
//! the server accepts.

use std::sync::atomic::{AtomicUsize, Ordering};

use irc::proto::{Command, Message, Response};

/// Modes per `MODE` line when the server announces no `MODES` limit: the
/// number RFC 2812 and IRCnet allow.
pub const DEFAULT_MODES_PER_LINE: usize = 3;
/// Upper bound even when a server announces more or no limit; keeps lines
/// well within 512 bytes together with the nicknames.
const MAX_MODES_PER_LINE: usize = 12;
/// Bytes a line's parameters may use, leaving room for `MODE <channel>`, the
/// flags and the line ending inside the 512-byte limit.
const MAX_PARAMETER_BYTES: usize = 350;

/// A change of one privilege for every listed member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberMode {
    Op,
    Deop,
    Voice,
    Devoice,
}

impl MemberMode {
    fn sign(self) -> char {
        match self {
            Self::Op | Self::Voice => '+',
            Self::Deop | Self::Devoice => '-',
        }
    }

    fn flag(self) -> char {
        match self {
            Self::Op | Self::Deop => 'o',
            Self::Voice | Self::Devoice => 'v',
        }
    }
}

/// The `MODES=<n>` value of an `RPL_ISUPPORT` line. `None` for other lines
/// and for `MODES` without a usable number (a bare `MODES` means the server
/// sets no fixed limit); either way the default per-line count applies.
pub(crate) fn announced_limit(message: &Message) -> Option<usize> {
    let Command::Response(Response::RPL_ISUPPORT, args) = &message.command else {
        return None;
    };
    args.iter()
        .skip(1)
        .flat_map(|arg| arg.split_whitespace())
        .find_map(|token| {
            let (name, value) = token.split_once('=')?;
            name.eq_ignore_ascii_case("MODES")
                .then(|| value.parse().ok().filter(|limit| *limit > 0))
                .flatten()
        })
}

/// Remembers the announced limit; 0 means none was announced.
pub(crate) fn observe(message: &Message, limit: &AtomicUsize) {
    if let Some(announced) = announced_limit(message) {
        limit.store(announced, Ordering::Relaxed);
    }
}

pub(crate) fn effective_limit(announced: usize) -> usize {
    match announced {
        0 => DEFAULT_MODES_PER_LINE,
        n => n.min(MAX_MODES_PER_LINE),
    }
}

/// The parameters of `MODE <channel> +ooo a b c` commands (`[channel, "+ooo",
/// "a", "b", "c"]`), as many commands as needed so none has more than
/// `per_line` modes or an over-long parameter list. Nicknames repeat at most
/// once; their order is kept.
pub(crate) fn mode_lines(
    channel: &str,
    mode: MemberMode,
    nicknames: &[String],
    per_line: usize,
) -> Vec<Vec<String>> {
    let per_line = per_line.max(1);
    let mut unique: Vec<&str> = Vec::new();
    for nickname in nicknames {
        if !unique
            .iter()
            .any(|seen| crate::text::same_nickname(seen, nickname))
        {
            unique.push(nickname);
        }
    }
    let mut lines = Vec::new();
    let mut group: Vec<&str> = Vec::new();
    let mut bytes = 0;
    let flush = |group: &mut Vec<&str>, lines: &mut Vec<Vec<String>>| {
        if !group.is_empty() {
            let flags = mode.flag().to_string().repeat(group.len());
            let mut args = vec![channel.to_owned(), format!("{}{flags}", mode.sign())];
            args.extend(group.drain(..).map(str::to_owned));
            lines.push(args);
        }
    };
    for nickname in unique {
        if group.len() == per_line || bytes + nickname.len() + 1 > MAX_PARAMETER_BYTES {
            flush(&mut group, &mut lines);
            bytes = 0;
        }
        bytes += nickname.len() + 1;
        group.push(nickname);
    }
    flush(&mut group, &mut lines);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nicks(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    /// The commands as they go on the wire.
    fn wire(lines: Vec<Vec<String>>) -> Vec<String> {
        lines
            .into_iter()
            .map(|args| format!("MODE {}", args.join(" ")))
            .collect()
    }

    #[test]
    fn groups_nicknames_by_the_per_line_limit() {
        assert_eq!(
            wire(mode_lines(
                "#c",
                MemberMode::Op,
                &nicks(&["a", "b", "c", "d", "e"]),
                3
            )),
            ["MODE #c +ooo a b c", "MODE #c +oo d e"]
        );
        assert_eq!(
            wire(mode_lines("#c", MemberMode::Devoice, &nicks(&["a"]), 3)),
            ["MODE #c -v a"]
        );
        assert_eq!(
            wire(mode_lines("#c", MemberMode::Voice, &nicks(&["a", "b"]), 1)),
            ["MODE #c +v a", "MODE #c +v b"]
        );
        assert!(mode_lines("#c", MemberMode::Op, &[], 3).is_empty());
    }

    #[test]
    fn repeated_nicknames_are_sent_once() {
        assert_eq!(
            wire(mode_lines(
                "#c",
                MemberMode::Deop,
                &nicks(&["Al", "bo", "AL"]),
                4
            )),
            ["MODE #c -oo Al bo"]
        );
    }

    #[test]
    fn long_nicknames_start_a_new_line_before_the_limit() {
        let names: Vec<String> = ["a", "b", "c", "d"]
            .iter()
            .map(|letter| letter.repeat(100))
            .collect();
        let lines = mode_lines("#c", MemberMode::Op, &names, 12);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(wire(lines).iter().all(|line| line.len() < 450));
    }

    #[test]
    fn announced_limits() {
        let limit = |line: &str| announced_limit(&line.parse().unwrap());
        assert_eq!(
            limit(":srv 005 me CHANTYPES=# MODES=4 :are supported"),
            Some(4)
        );
        assert_eq!(limit(":srv 005 me modes=6 :are supported"), Some(6));
        assert_eq!(limit(":srv 005 me MODES :are supported"), None);
        assert_eq!(limit(":srv 005 me MODES=0 :are supported"), None);
        assert_eq!(limit(":srv 001 me :MODES=4"), None);
        assert_eq!(effective_limit(0), DEFAULT_MODES_PER_LINE);
        assert_eq!(effective_limit(4), 4);
        assert_eq!(effective_limit(100), MAX_MODES_PER_LINE);
    }
}
