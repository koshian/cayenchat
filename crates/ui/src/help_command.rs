// SPDX-License-Identifier: GPL-3.0-only
//! The local `/help` command: a list of the main commands in the server
//! log. It is never sent to the server.

/// What a `/help` line asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// The command list.
    List,
    /// Arguments were given; there is no topic help.
    Usage,
}

/// The request in `line` if it is a `/help` command (any case); `None` for
/// every other line, `/helper` included.
pub fn parse(line: &str) -> Option<Request> {
    let rest = line.trim().strip_prefix('/')?;
    let (word, rest) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    word.eq_ignore_ascii_case("help").then(|| {
        if rest.trim().is_empty() {
            Request::List
        } else {
            Request::Usage
        }
    })
}
