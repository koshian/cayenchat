// SPDX-License-Identifier: GPL-3.0-only
//! The local `/charset` command: a channel's own character encodings.
//! The syntax follows WeeChat's: `/charset <encoding>` sets both
//! directions, `decode` and `encode` one of them, `reset` removes the
//! override. It is never sent to the server.

use cayenchat_storage::{ChannelCharset, TextEncoding};

/// What a `/charset` line asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// Show the encodings in use and where they come from.
    Show,
    /// Show the syntax and the supported names.
    Help,
    /// Remove the override of both directions.
    Reset,
    /// Override the given sides; a side that is `None` stays as it is.
    Set {
        decode: Option<TextEncoding>,
        encode: Option<TextEncoding>,
    },
}

/// Why a `/charset` line cannot be run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// An encoding name that is none of the supported ones.
    UnknownEncoding(String),
    /// A word that is not part of the syntax, or a missing or surplus one.
    Usage,
}

/// The request in `line` if it is a `/charset` command (any case, with
/// anything after the word); `None` for every other line.
pub fn parse(line: &str) -> Option<Result<Request, Problem>> {
    let rest = line.trim().strip_prefix('/')?;
    let (word, rest) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    word.eq_ignore_ascii_case("charset")
        .then(|| parse_arguments(&rest.split_whitespace().collect::<Vec<_>>()))
}

fn encoding(name: &str) -> Result<TextEncoding, Problem> {
    TextEncoding::parse(name).ok_or_else(|| Problem::UnknownEncoding(name.to_owned()))
}

fn parse_arguments(words: &[&str]) -> Result<Request, Problem> {
    match words {
        [] => Ok(Request::Show),
        [word] if word.eq_ignore_ascii_case("help") => Ok(Request::Help),
        [word] if word.eq_ignore_ascii_case("reset") => Ok(Request::Reset),
        [word, name] if word.eq_ignore_ascii_case("decode") => Ok(Request::Set {
            decode: Some(encoding(name)?),
            encode: None,
        }),
        [word, name] if word.eq_ignore_ascii_case("encode") => Ok(Request::Set {
            decode: None,
            encode: Some(encoding(name)?),
        }),
        [word] if word.eq_ignore_ascii_case("decode") || word.eq_ignore_ascii_case("encode") => {
            Err(Problem::Usage)
        }
        [name] => {
            let both = Some(encoding(name)?);
            Ok(Request::Set {
                decode: both,
                encode: both,
            })
        }
        _ => Err(Problem::Usage),
    }
}

/// The names of every supported encoding, as `/charset` and the server
/// settings list them.
pub fn supported_names() -> String {
    TextEncoding::ALL
        .into_iter()
        .map(TextEncoding::label)
        .collect::<Vec<_>>()
        .join(", ")
}

/// What `request` leaves of `current`: the sides it names replaced, the
/// other kept. `None` for both sides means no override at all.
pub fn apply(
    request: Request,
    current: Option<&ChannelCharset>,
) -> (Option<TextEncoding>, Option<TextEncoding>) {
    let (decode, encode) = current.map_or((None, None), |entry| (entry.decode, entry.encode));
    match request {
        Request::Reset => (None, None),
        Request::Set {
            decode: new_decode,
            encode: new_encode,
        } => (new_decode.or(decode), new_encode.or(encode)),
        Request::Show | Request::Help => (decode, encode),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(line: &str) -> Result<Request, Problem> {
        parse(line).expect("a /charset line")
    }

    #[test]
    fn only_charset_lines_are_commands() {
        assert!(parse("hello").is_none());
        assert!(parse("/me charset").is_none());
        assert!(parse("/charsets").is_none());
        assert!(parse("//charset").is_none());
        assert!(parse("/charset").is_some());
        assert!(parse("  /CHARSET  ").is_some());
    }

    #[test]
    fn the_weechat_forms_are_understood() {
        assert_eq!(run("/charset"), Ok(Request::Show));
        assert_eq!(run("/charset help"), Ok(Request::Help));
        assert_eq!(run("/charset RESET"), Ok(Request::Reset));
        let jis = Some(TextEncoding::Iso2022Jp);
        assert_eq!(
            run("/charset ISO-2022-JP"),
            Ok(Request::Set {
                decode: jis,
                encode: jis
            })
        );
        assert_eq!(
            run("/charset decode iso-2022-jp"),
            Ok(Request::Set {
                decode: jis,
                encode: None
            })
        );
        assert_eq!(
            run("/charset  Encode   sjis"),
            Ok(Request::Set {
                decode: None,
                encode: Some(TextEncoding::ShiftJis)
            })
        );
    }

    #[test]
    fn bad_input_is_named_not_guessed() {
        assert_eq!(
            run("/charset klingon"),
            Err(Problem::UnknownEncoding("klingon".into()))
        );
        assert_eq!(
            run("/charset decode ISO-8859-1"),
            Err(Problem::UnknownEncoding("ISO-8859-1".into()))
        );
        assert_eq!(run("/charset decode"), Err(Problem::Usage));
        assert_eq!(run("/charset encode a b"), Err(Problem::Usage));
        assert_eq!(run("/charset reset now"), Err(Problem::Usage));
        assert_eq!(run("/charset a b c"), Err(Problem::Usage));
    }

    #[test]
    fn one_side_leaves_the_other_as_it_was() {
        let entry = ChannelCharset {
            channel: "#a".into(),
            decode: Some(TextEncoding::Big5),
            encode: None,
            char_refs: None,
        };
        let set = |request| apply(request, Some(&entry));
        assert_eq!(
            set(Request::Set {
                decode: None,
                encode: Some(TextEncoding::Gbk)
            }),
            (Some(TextEncoding::Big5), Some(TextEncoding::Gbk))
        );
        assert_eq!(
            apply(
                Request::Set {
                    decode: Some(TextEncoding::Gbk),
                    encode: None
                },
                None
            ),
            (Some(TextEncoding::Gbk), None)
        );
        assert_eq!(set(Request::Reset), (None, None));
    }

    #[test]
    fn every_supported_name_is_listed_and_accepted() {
        let names = supported_names();
        for encoding in TextEncoding::ALL {
            assert!(names.contains(encoding.label()));
            assert_eq!(
                run(&format!("/charset {}", encoding.label())),
                Ok(Request::Set {
                    decode: Some(encoding),
                    encode: Some(encoding)
                })
            );
        }
    }
}
