//! Sent-draft history for the chat input: in memory only and bounded.

/// Most recent sent drafts kept.
const LIMIT: usize = 20;

#[derive(Default)]
pub struct InputHistory {
    /// Oldest first.
    entries: Vec<String>,
    browsing: Option<Browsing>,
}

struct Browsing {
    /// Which conversation's input is being browsed; moving to another one
    /// ends it.
    scope: u64,
    index: usize,
    /// The text this history last put into the input; anything else in the
    /// input means the user edited it or moved elsewhere.
    shown: String,
    /// The unsent text that was in the input before browsing started.
    draft: String,
}

/// Whether `text` carries a credential and must not be kept where an Up key
/// (or a shared screen) could show it again: commands to the services that
/// take passwords (`cayenchat_irc_core::is_credential_service`), `/oper`,
/// `/pass`, `/auth`, and `/raw` forms of those, or anything typed in a
/// private conversation with such a service (`private_peer`). Mirrors what
/// the core hides from the transcript, but wider: any message to those
/// services counts.
pub fn is_sensitive(text: &str, private_peer: Option<&str>) -> bool {
    use cayenchat_irc_core::is_credential_service;
    let Some(command) = text.strip_prefix('/') else {
        return private_peer.is_some_and(is_credential_service);
    };
    let mut words = command.split_whitespace();
    let verb = words.next().unwrap_or("").to_ascii_uppercase();
    match verb.as_str() {
        "OPER" | "PASS" | "AUTH" | "AUTHENTICATE" | "NS" | "NICKSERV" | "CS" | "CHANSERV"
        | "AUTHSERV" => true,
        "MSG" | "PRIVMSG" | "NOTICE" | "SQUERY" => {
            // `/msg :text` and `/msg text` leave out the target: the
            // conversation's peer.
            let rest = command
                .split_once(char::is_whitespace)
                .map_or("", |(_, r)| r.trim_start());
            let omitted = rest.starts_with(':') || rest.split_whitespace().count() == 1;
            if omitted {
                private_peer.is_some_and(is_credential_service)
            } else {
                words.next().is_some_and(is_credential_service)
            }
        }
        // `/me` and `/msg :text` in a service conversation go to the service.
        _ if private_peer.is_some_and(is_credential_service) => true,
        "RAW" | "QUOTE" => command
            .split_once(char::is_whitespace)
            .is_some_and(|(_, raw)| is_sensitive(&format!("/{}", raw.trim_start()), None)),
        _ => false,
    }
}

impl InputHistory {
    /// Remembers a sent draft and ends any browsing. Repeating the latest
    /// entry does not add another, and drafts with credentials are not kept.
    /// `private_peer` is the peer of the private conversation it was sent in.
    pub fn record(&mut self, text: &str, private_peer: Option<&str>) {
        self.browsing = None;
        if text.trim().is_empty()
            || is_sensitive(text, private_peer)
            || self.entries.last().is_some_and(|last| last == text)
        {
            return;
        }
        self.entries.push(text.to_owned());
        if self.entries.len() > LIMIT {
            self.entries.remove(0);
        }
    }

    /// The next older entry, given the conversation (`scope`) and the input's
    /// current text. `None` leaves the input alone (no history, or already at
    /// the oldest).
    pub fn previous(&mut self, scope: u64, current: &str) -> Option<String> {
        self.drop_if_stale(scope, current);
        let index = match &self.browsing {
            Some(browsing) => browsing.index.checked_sub(1)?,
            None => self.entries.len().checked_sub(1)?,
        };
        let draft = self
            .browsing
            .take()
            .map_or_else(|| current.to_owned(), |browsing| browsing.draft);
        Some(self.show(scope, index, draft))
    }

    /// The next newer entry, or the unsent text once past the newest. `None`
    /// when not browsing.
    pub fn next(&mut self, scope: u64, current: &str) -> Option<String> {
        self.drop_if_stale(scope, current);
        let browsing = self.browsing.take()?;
        if browsing.index + 1 < self.entries.len() {
            Some(self.show(scope, browsing.index + 1, browsing.draft))
        } else {
            Some(browsing.draft)
        }
    }

    fn show(&mut self, scope: u64, index: usize, draft: String) -> String {
        let shown = self.entries[index].clone();
        self.browsing = Some(Browsing {
            scope,
            index,
            shown: shown.clone(),
            draft,
        });
        shown
    }

    fn drop_if_stale(&mut self, scope: u64, current: &str) {
        if self
            .browsing
            .as_ref()
            .is_some_and(|browsing| browsing.scope != scope || browsing.shown != current)
        {
            self.browsing = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HERE: u64 = 1;

    fn history(entries: &[&str]) -> InputHistory {
        let mut history = InputHistory::default();
        for entry in entries {
            history.record(entry, None);
        }
        history
    }

    #[test]
    fn walks_back_and_forth_and_restores_the_unsent_draft() {
        let mut history = history(&["one", "/join #two", "three"]);
        assert_eq!(history.previous(HERE, "draft").as_deref(), Some("three"));
        assert_eq!(
            history.previous(HERE, "three").as_deref(),
            Some("/join #two")
        );
        assert_eq!(history.previous(HERE, "/join #two").as_deref(), Some("one"));
        assert_eq!(history.previous(HERE, "one"), None, "stops at the oldest");
        assert_eq!(history.next(HERE, "one").as_deref(), Some("/join #two"));
        assert_eq!(history.next(HERE, "/join #two").as_deref(), Some("three"));
        assert_eq!(history.next(HERE, "three").as_deref(), Some("draft"));
        assert_eq!(history.next(HERE, "draft"), None, "no longer browsing");
    }

    #[test]
    fn is_bounded_and_skips_blanks_and_immediate_repeats() {
        let mut history = InputHistory::default();
        history.record("   ", None);
        assert_eq!(history.previous(HERE, ""), None);
        for n in 0..LIMIT + 5 {
            history.record(&n.to_string(), None);
            history.record(&n.to_string(), None);
        }
        assert_eq!(history.entries.len(), LIMIT);
        assert_eq!(history.entries[0], "5");
    }

    #[test]
    fn editing_the_recalled_text_starts_over() {
        let mut history = history(&["one", "two"]);
        assert_eq!(history.previous(HERE, "").as_deref(), Some("two"));
        // Edited, so the draft is now "two!" and browsing restarts from the newest.
        assert_eq!(history.previous(HERE, "two!").as_deref(), Some("two"));
        assert_eq!(history.next(HERE, "two").as_deref(), Some("two!"));
    }

    #[test]
    fn sending_ends_browsing() {
        let mut history = history(&["one", "two"]);
        assert_eq!(history.previous(HERE, "").as_deref(), Some("two"));
        history.record("two", None);
        assert_eq!(history.next(HERE, "two"), None);
    }

    #[test]
    fn another_conversation_ends_browsing_even_if_its_text_matches() {
        let mut history = history(&["one", "two"]);
        assert_eq!(history.previous(HERE, "mine").as_deref(), Some("two"));
        // The other conversation's draft happens to equal what was shown: the
        // unsent text of the first one must not be restored there.
        assert_eq!(history.next(2, "two"), None);
        assert_eq!(history.previous(2, "two").as_deref(), Some("two"));
        assert_eq!(history.next(2, "two").as_deref(), Some("two"));
    }

    #[test]
    fn credentials_are_never_kept() {
        let secret = [
            "/msg NickServ identify hunter2",
            "/MSG nickserv@services.example register pw a@b.c",
            "/notice ChanServ SET #c founder me",
            "/ns identify pw",
            "/nickserv ghost me pw",
            "/cs op #c",
            "/oper admin pw",
            "/pass secret",
            "/raw PASS secret",
            "/quote OPER admin pw",
            "/raw PRIVMSG NickServ :IDENTIFY pw",
            "/raw   ns identify pw",
            "/msg Q@CServe.quakenet.org AUTH alice pw",
            "/msg X@channels.undernet.org login alice pw",
            "/msg AuthServ auth alice pw",
            "/authserv auth alice pw",
            "/quote AUTH alice pw",
        ];
        let mut history = InputHistory::default();
        for line in secret {
            assert!(is_sensitive(line, None), "{line}");
            history.record(line, None);
        }
        // Typed in a private conversation with a service.
        for (line, peer) in [
            ("AUTH alice pw", "Q"),
            ("identify pw", "NickServ"),
            ("/me x", "X"),
            ("/msg :AUTH alice pw", "Q"),
            ("/notice :AUTH alice pw", "Q"),
        ] {
            assert!(is_sensitive(line, Some(peer)), "{line}");
            history.record(line, Some(peer));
        }
        assert_eq!(history.previous(HERE, ""), None, "nothing was kept");
        let harmless = [
            "hello NickServ",
            "NickServ identify is how you log in",
            "/msg alice identify yourself",
            "/join #nickserv",
            "/me waves",
            "/raw WHO #c",
            "/nick alice",
        ];
        for line in harmless {
            assert!(!is_sensitive(line, None), "{line}");
            assert!(!is_sensitive(line, Some("alice")), "{line}");
            history.record(line, None);
        }
        assert_eq!(history.previous(HERE, "").as_deref(), Some("/nick alice"));
    }
}
