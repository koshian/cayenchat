//! Sending an image from an IRC draft.
//!
//! Paste and drop both hand an [`Attachment`] to [`AttachmentFlow::offer`].
//! IRC cannot carry files, so a confirmed image goes to the user's external
//! hosting account and the returned link is inserted into the draft that was
//! selected when the image arrived. The flow never sends a message; the user
//! reviews the draft and sends it. Uploading itself happens outside this
//! module, which only tracks the steps.

use cayenchat_model::attachment::Attachment;

use crate::Selection;

/// What the application knows about the configured image uploader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploaderReadiness {
    /// No provider is selected.
    NotConfigured,
    /// A provider is selected but no account credential is saved.
    NeedsAccount {
        provider: String,
    },
    Ready {
        provider: String,
    },
}

/// What the user must be shown after offering an attachment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Offer {
    /// Ask before the image leaves the computer.
    Confirm {
        provider: String,
        name: String,
        size: String,
    },
    /// Guide the user to image upload settings.
    Configure,
    /// Guide the user to reconnect the provider account.
    Reconnect { provider: String },
    /// Another image is awaiting confirmation or uploading.
    Busy,
}

/// An upload the caller must start.
#[derive(Clone, Debug)]
pub struct UploadJob {
    pub id: u64,
    pub attachment: Attachment,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadFailure {
    Authentication,
    Rejected(String),
    Network(String),
}

/// The result of an upload the caller finished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Completion {
    /// Insert `url` into the draft of `target`.
    InsertLink { target: Selection, url: String },
    /// Report the failure; the draft stays as it is.
    Failed {
        provider: String,
        failure: UploadFailure,
    },
    /// The upload was cancelled or superseded; do nothing.
    Ignored,
}

#[derive(Clone, Debug)]
pub enum Phase {
    Idle,
    Confirming {
        attachment: Attachment,
        target: Selection,
        provider: String,
    },
    Uploading {
        id: u64,
        target: Selection,
        provider: String,
    },
}

#[derive(Debug)]
pub struct AttachmentFlow {
    phase: Phase,
    next_id: u64,
}

impl Default for AttachmentFlow {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            next_id: 1,
        }
    }
}

impl AttachmentFlow {
    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// The provider being uploaded to, while an upload runs.
    pub fn uploading(&self) -> Option<&str> {
        match &self.phase {
            Phase::Uploading { provider, .. } => Some(provider),
            _ => None,
        }
    }

    /// Starts the flow for an image destined for the draft of `target`.
    pub fn offer(
        &mut self,
        attachment: Attachment,
        target: Selection,
        readiness: UploaderReadiness,
    ) -> Offer {
        if !matches!(self.phase, Phase::Idle) {
            return Offer::Busy;
        }
        match readiness {
            UploaderReadiness::NotConfigured => Offer::Configure,
            UploaderReadiness::NeedsAccount { provider } => Offer::Reconnect { provider },
            UploaderReadiness::Ready { provider } => {
                let offer = Offer::Confirm {
                    provider: provider.clone(),
                    name: attachment.name.clone(),
                    size: attachment.size_text(),
                };
                self.phase = Phase::Confirming {
                    attachment,
                    target,
                    provider,
                };
                offer
            }
        }
    }

    /// The user declined; nothing is uploaded.
    pub fn decline(&mut self) {
        if matches!(self.phase, Phase::Confirming { .. }) {
            self.phase = Phase::Idle;
        }
    }

    /// The user agreed. Returns the upload to start, once per offer.
    pub fn confirm(&mut self) -> Option<UploadJob> {
        if !matches!(self.phase, Phase::Confirming { .. }) {
            return None;
        }
        let Phase::Confirming {
            attachment,
            target,
            provider,
        } = std::mem::replace(&mut self.phase, Phase::Idle)
        else {
            return None;
        };
        let id = self.next_id;
        self.next_id += 1;
        self.phase = Phase::Uploading {
            id,
            target,
            provider,
        };
        Some(UploadJob { id, attachment })
    }

    /// Stops waiting for the running upload; its result will be ignored.
    /// The request itself may still reach the provider.
    pub fn cancel_upload(&mut self) -> bool {
        let cancelled = matches!(self.phase, Phase::Uploading { .. });
        if cancelled {
            self.phase = Phase::Idle;
        }
        cancelled
    }

    pub fn finish(&mut self, id: u64, result: Result<String, UploadFailure>) -> Completion {
        let Phase::Uploading {
            id: current,
            target,
            provider,
        } = &self.phase
        else {
            return Completion::Ignored;
        };
        if *current != id {
            return Completion::Ignored;
        }
        let completion = match result {
            Ok(url) => Completion::InsertLink {
                target: *target,
                url,
            },
            Err(failure) => Completion::Failed {
                provider: provider.clone(),
                failure,
            },
        };
        self.phase = Phase::Idle;
        completion
    }
}

/// Text to insert at a cursor between `before` and `after` so the link is
/// separated from neighbouring words by single spaces.
pub fn link_insertion(before: Option<char>, after: Option<char>, url: &str) -> String {
    let mut text = String::new();
    if before.is_some_and(|ch| !ch.is_whitespace()) {
        text.push(' ');
    }
    text.push_str(url);
    if after.is_none_or(|ch| !ch.is_whitespace()) {
        text.push(' ');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use cayenchat_model::{
        ConversationId,
        attachment::{Attachment, AttachmentSource},
    };

    fn image() -> Attachment {
        Attachment::image(
            Some("shot.png"),
            b"\x89PNG\r\n\x1a\nxx".to_vec(),
            AttachmentSource::Clipboard,
        )
        .unwrap()
    }

    fn ready() -> UploaderReadiness {
        UploaderReadiness::Ready {
            provider: "Gyazo".into(),
        }
    }

    const TARGET: Selection = Selection::Channel(ConversationId(7));

    #[test]
    fn missing_configuration_or_account_guides_the_user() {
        let mut flow = AttachmentFlow::default();
        assert_eq!(
            flow.offer(image(), TARGET, UploaderReadiness::NotConfigured),
            Offer::Configure
        );
        assert_eq!(
            flow.offer(
                image(),
                TARGET,
                UploaderReadiness::NeedsAccount {
                    provider: "Gyazo".into()
                }
            ),
            Offer::Reconnect {
                provider: "Gyazo".into()
            }
        );
        assert!(matches!(flow.phase(), Phase::Idle));
        assert!(flow.confirm().is_none(), "nothing to upload");
    }

    #[test]
    fn declining_uploads_nothing() {
        let mut flow = AttachmentFlow::default();
        assert!(matches!(
            flow.offer(image(), TARGET, ready()),
            Offer::Confirm { ref name, .. } if name == "shot.png"
        ));
        flow.decline();
        assert!(flow.confirm().is_none());
        assert!(matches!(flow.phase(), Phase::Idle));
    }

    #[test]
    fn confirmed_upload_inserts_link_into_the_original_draft_once() {
        let mut flow = AttachmentFlow::default();
        flow.offer(image(), TARGET, ready());
        let job = flow.confirm().unwrap();
        assert_eq!(job.attachment.name, "shot.png");
        assert!(flow.confirm().is_none(), "one upload per offer");
        assert_eq!(flow.uploading(), Some("Gyazo"));
        // A second paste during the upload does not start another one.
        assert_eq!(flow.offer(image(), TARGET, ready()), Offer::Busy);
        assert_eq!(
            flow.finish(job.id, Ok("https://i.example/x.png".into())),
            Completion::InsertLink {
                target: TARGET,
                url: "https://i.example/x.png".into()
            }
        );
        assert_eq!(
            flow.finish(job.id, Ok("https://i.example/x.png".into())),
            Completion::Ignored
        );
    }

    #[test]
    fn failures_and_cancellation_leave_the_draft_alone() {
        let mut flow = AttachmentFlow::default();
        flow.offer(image(), TARGET, ready());
        let job = flow.confirm().unwrap();
        assert_eq!(
            flow.finish(job.id, Err(UploadFailure::Authentication)),
            Completion::Failed {
                provider: "Gyazo".into(),
                failure: UploadFailure::Authentication
            }
        );
        flow.offer(image(), TARGET, ready());
        let job = flow.confirm().unwrap();
        assert!(flow.cancel_upload());
        assert_eq!(
            flow.finish(job.id, Ok("https://late".into())),
            Completion::Ignored
        );
        assert!(!flow.cancel_upload());
    }

    #[test]
    fn links_are_spaced_from_surrounding_text() {
        assert_eq!(link_insertion(None, None, "u"), "u ");
        assert_eq!(link_insertion(Some('a'), None, "u"), " u ");
        assert_eq!(link_insertion(Some(' '), Some(' '), "u"), "u");
        assert_eq!(link_insertion(Some('a'), Some('b'), "u"), " u ");
    }
}
