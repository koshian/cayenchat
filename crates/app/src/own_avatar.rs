//! Our own avatar on one server, as far as the server has confirmed it.
//!
//! The URL the user edits is a draft kept in settings; nothing here reads or
//! changes it. This state tracks what the server reported on the current
//! connection, the one request that may be waiting for the server, and the
//! last outcome to show. Success is only ever recorded from the server's
//! answer, never because a request was queued. Requests carry increasing
//! identifiers for the whole session, so an answer to a request from an
//! earlier connection, or one given up already, cannot be mistaken for the
//! current one.
//!
//! Protocol-free: the IRC adapter reports availability, answers and
//! failures; a Matrix client could drive the same state.

/// What the server holds for us on the current connection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Confirmed {
    /// Not connected, or not reported yet.
    #[default]
    Unknown,
    NotSet,
    Set(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Publish,
    Remove,
}

/// Why a request did not succeed, as the protocol adapter reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// Not registered, or the capability is not negotiated.
    Unavailable,
    /// Another request is still waiting for the server.
    Busy,
    Rejected {
        code: String,
        description: String,
    },
    RateLimited {
        retry_after: Option<u64>,
    },
    NoReply,
    CapabilityLost,
    /// The connection ended before the server answered.
    ConnectionLost,
    /// The request could not even be queued.
    NotSent(String),
}

/// The last thing to tell the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Waiting(Action),
    Published(String),
    Removed,
    Failed(Action, Failure),
}

/// Why a request cannot be started now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocked {
    NotReady,
    Busy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pending {
    request: u64,
    action: Action,
}

#[derive(Debug, Default)]
pub struct OwnAvatar {
    ready: bool,
    confirmed: Confirmed,
    pending: Option<Pending>,
    outcome: Option<Outcome>,
    next_request: u64,
}

impl OwnAvatar {
    pub fn confirmed(&self) -> &Confirmed {
        &self.confirmed
    }

    pub fn outcome(&self) -> Option<&Outcome> {
        self.outcome.as_ref()
    }

    /// Whether the connection can take a request.
    pub fn ready(&self) -> bool {
        self.ready
    }

    pub fn waiting(&self) -> bool {
        self.pending.is_some()
    }

    /// Whether Publish or Remove may be offered now.
    pub fn can_request(&self) -> bool {
        self.ready && self.pending.is_none()
    }

    /// Starts a request; the caller sends it with the returned identifier.
    pub fn begin(&mut self, action: Action) -> Result<u64, Blocked> {
        if !self.ready {
            return Err(Blocked::NotReady);
        }
        if self.pending.is_some() {
            return Err(Blocked::Busy);
        }
        self.next_request += 1;
        let request = self.next_request;
        self.pending = Some(Pending { request, action });
        self.outcome = Some(Outcome::Waiting(action));
        Ok(request)
    }

    /// The protocol can take requests (IRC: registered, capability
    /// negotiated, subscribed).
    pub fn set_ready(&mut self) {
        self.ready = true;
    }

    /// The server reported our avatar. `request` is the request this
    /// answers, if any; an answer to anything but the pending request only
    /// updates what the server holds. Reports while not ready belong to a
    /// connection that has ended and are ignored.
    pub fn reported(&mut self, url: Option<String>, request: Option<u64>) {
        if !self.ready {
            return;
        }
        self.confirmed = match &url {
            Some(url) => Confirmed::Set(url.clone()),
            None => Confirmed::NotSet,
        };
        let Some(pending) = self
            .pending
            .filter(|pending| Some(pending.request) == request)
        else {
            return;
        };
        self.pending = None;
        self.outcome = Some(match url {
            Some(url) => Outcome::Published(url),
            // The server kept no avatar: that is what a removal wants, and a
            // publication that ends there did not succeed.
            None if pending.action == Action::Remove => Outcome::Removed,
            None => Outcome::Failed(
                Action::Publish,
                Failure::Rejected {
                    code: "KEY_NOT_SET".into(),
                    description: String::new(),
                },
            ),
        });
    }

    /// A request failed; stale identifiers are ignored.
    pub fn failed(&mut self, request: u64, failure: Failure) {
        if let Some(pending) = self.pending.filter(|pending| pending.request == request) {
            self.pending = None;
            self.outcome = Some(Outcome::Failed(pending.action, failure));
        }
    }

    /// The capability went away: nothing more can be sent on this
    /// connection and what the server holds is unknown again.
    pub fn capability_lost(&mut self) {
        self.end(Failure::CapabilityLost);
    }

    /// The connection ended, or a new one starts. The draft (elsewhere) is
    /// kept; nothing is republished.
    pub fn connection_ended(&mut self) {
        self.end(Failure::ConnectionLost);
    }

    fn end(&mut self, failure: Failure) {
        self.ready = false;
        self.confirmed = Confirmed::Unknown;
        if let Some(pending) = self.pending.take() {
            self.outcome = Some(Outcome::Failed(pending.action, failure));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://example.com/me.png";

    fn ready() -> OwnAvatar {
        let mut own = OwnAvatar::default();
        own.set_ready();
        own
    }

    #[test]
    fn requests_need_a_ready_connection_and_go_one_at_a_time() {
        let mut own = OwnAvatar::default();
        assert!(!own.can_request());
        assert_eq!(own.begin(Action::Publish), Err(Blocked::NotReady));
        own.set_ready();
        let first = own.begin(Action::Publish).unwrap();
        assert!(own.waiting() && !own.can_request());
        assert_eq!(own.begin(Action::Remove), Err(Blocked::Busy));
        assert_eq!(own.outcome(), Some(&Outcome::Waiting(Action::Publish)));
        // Queued is not published: only the server's answer is.
        assert_eq!(own.confirmed(), &Confirmed::Unknown);
        own.reported(Some(URL.into()), Some(first));
        assert_eq!(own.outcome(), Some(&Outcome::Published(URL.into())));
        assert_eq!(own.confirmed(), &Confirmed::Set(URL.into()));
        assert!(own.can_request());
    }

    #[test]
    fn removal_and_rejections_follow_the_server() {
        let mut own = ready();
        let remove = own.begin(Action::Remove).unwrap();
        own.reported(None, Some(remove));
        assert_eq!(own.outcome(), Some(&Outcome::Removed));
        assert_eq!(own.confirmed(), &Confirmed::NotSet);

        let publish = own.begin(Action::Publish).unwrap();
        let rejected = Failure::Rejected {
            code: "INVALID_VALUE".into(),
            description: "Value is too long".into(),
        };
        own.failed(publish, rejected.clone());
        assert_eq!(
            own.outcome(),
            Some(&Outcome::Failed(Action::Publish, rejected))
        );
        assert_eq!(own.confirmed(), &Confirmed::NotSet, "unchanged");

        let publish = own.begin(Action::Publish).unwrap();
        own.reported(None, Some(publish));
        assert!(matches!(
            own.outcome(),
            Some(Outcome::Failed(Action::Publish, _))
        ));
    }

    #[test]
    fn stale_answers_and_changes_elsewhere_do_not_confirm_a_request() {
        let mut own = ready();
        let old = own.begin(Action::Publish).unwrap();
        own.failed(old, Failure::NoReply);
        let current = own.begin(Action::Publish).unwrap();
        // The late answer to the earlier request, and a change made by
        // another client, update what the server holds only.
        own.reported(Some(URL.into()), Some(old));
        own.failed(old, Failure::NoReply);
        own.reported(Some("https://example.com/other.png".into()), None);
        assert_eq!(own.outcome(), Some(&Outcome::Waiting(Action::Publish)));
        assert_eq!(
            own.confirmed(),
            &Confirmed::Set("https://example.com/other.png".into())
        );
        own.reported(Some(URL.into()), Some(current));
        assert_eq!(own.outcome(), Some(&Outcome::Published(URL.into())));
    }

    #[test]
    fn disconnects_and_capability_loss_fail_the_request_and_forget_the_state() {
        let mut own = ready();
        let request = own.begin(Action::Publish).unwrap();
        own.connection_ended();
        assert_eq!(
            own.outcome(),
            Some(&Outcome::Failed(Action::Publish, Failure::ConnectionLost))
        );
        assert!(!own.ready() && own.confirmed() == &Confirmed::Unknown);
        // An answer from the old connection changes nothing now, and the
        // next connection starts with a fresh identifier.
        own.reported(Some(URL.into()), Some(request));
        assert!(matches!(own.outcome(), Some(Outcome::Failed(..))));
        assert_eq!(own.confirmed(), &Confirmed::Unknown);
        own.set_ready();
        let next = own.begin(Action::Remove).unwrap();
        assert!(next > request);
        own.capability_lost();
        assert_eq!(
            own.outcome(),
            Some(&Outcome::Failed(Action::Remove, Failure::CapabilityLost))
        );
        assert_eq!(own.begin(Action::Remove), Err(Blocked::NotReady));
    }
}
