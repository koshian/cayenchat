//! The outgoing message queue, bounded in messages and bytes.
//!
//! Upstream used an unbounded channel, so a server that sends `PING` faster
//! than the socket drains made the automatic `PONG` replies pile up without
//! limit. Every send, automatic replies included, goes through this queue; a
//! send that would exceed a limit fails with `Error::OutgoingQueueFull`
//! instead of dropping the message silently.
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    task::{Context, Poll},
};

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::{
    error,
    proto::{Command, Message},
};

/// Most messages waiting to be written.
pub const MAX_QUEUED_MESSAGES: usize = 1024;
/// Most bytes (serialized length) waiting to be written.
pub const MAX_QUEUED_BYTES: usize = 1024 * 1024;

#[derive(Debug, Default)]
struct Usage {
    messages: AtomicUsize,
    bytes: AtomicUsize,
}

/// Sending half of the outgoing queue.
#[derive(Debug, Clone)]
pub struct OutgoingSender {
    tx: UnboundedSender<(Message, usize)>,
    usage: Arc<Usage>,
}

/// Receiving half of the outgoing queue.
#[derive(Debug)]
pub struct OutgoingReceiver {
    rx: UnboundedReceiver<(Message, usize)>,
    usage: Arc<Usage>,
}

pub fn channel() -> (OutgoingSender, OutgoingReceiver) {
    let (tx, rx) = mpsc::unbounded_channel();
    let usage = Arc::new(Usage::default());
    (
        OutgoingSender {
            tx,
            usage: usage.clone(),
        },
        OutgoingReceiver { rx, usage },
    )
}

impl OutgoingSender {
    /// Queues `msg`, or fails when the queue is closed or full. `QUIT` is
    /// always admitted: the application sends it once when it ends the
    /// connection and it must be written behind any backlog.
    pub fn send(&self, msg: Message) -> error::Result<()> {
        let weight = msg.to_string().len();
        let messages = self.usage.messages.fetch_add(1, Ordering::AcqRel) + 1;
        let bytes = self.usage.bytes.fetch_add(weight, Ordering::AcqRel) + weight;
        let over = messages > MAX_QUEUED_MESSAGES || bytes > MAX_QUEUED_BYTES;
        if over && !matches!(msg.command, Command::QUIT(_)) {
            self.usage.release(weight);
            return Err(error::Error::OutgoingQueueFull);
        }
        self.tx.send((msg, weight)).map_err(|_| {
            self.usage.release(weight);
            error::Error::AsyncChannelClosed
        })
    }
}

impl OutgoingReceiver {
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<Message>> {
        self.rx.poll_recv(cx).map(|item| {
            item.map(|(msg, weight)| {
                self.usage.release(weight);
                msg
            })
        })
    }
}

impl Usage {
    fn release(&self, weight: usize) {
        self.messages.fetch_sub(1, Ordering::AcqRel);
        self.bytes.fetch_sub(weight, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use futures_util::task::noop_waker;

    fn pong(data: &str) -> Message {
        Command::PONG(data.to_owned(), None).into()
    }

    #[test]
    fn rejects_past_message_limit_and_recovers() {
        let (tx, mut rx) = channel();
        for _ in 0..MAX_QUEUED_MESSAGES {
            tx.send(pong("x")).unwrap();
        }
        assert!(matches!(
            tx.send(pong("x")),
            Err(error::Error::OutgoingQueueFull)
        ));
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(rx.poll_recv(&mut cx), Poll::Ready(Some(_))));
        tx.send(pong("x")).unwrap();
        assert_eq!(
            tx.usage.messages.load(Ordering::Acquire),
            MAX_QUEUED_MESSAGES
        );
    }

    #[test]
    fn quit_is_admitted_behind_a_full_queue() {
        let (tx, _rx) = channel();
        for _ in 0..MAX_QUEUED_MESSAGES {
            tx.send(pong("x")).unwrap();
        }
        tx.send(Command::QUIT(None).into()).unwrap();
    }

    #[test]
    fn rejects_past_byte_limit() {
        let (tx, _rx) = channel();
        let big = "a".repeat(MAX_QUEUED_BYTES / 4);
        for _ in 0..3 {
            tx.send(pong(&big)).unwrap();
        }
        assert!(matches!(
            tx.send(pong(&big)).and(tx.send(pong(&big))),
            Err(error::Error::OutgoingQueueFull)
        ));
        assert!(tx.usage.bytes.load(Ordering::Acquire) <= MAX_QUEUED_BYTES);
    }

    #[test]
    fn closed_queue_does_not_leak_usage() {
        let (tx, rx) = channel();
        drop(rx);
        assert!(matches!(
            tx.send(pong("x")),
            Err(error::Error::AsyncChannelClosed)
        ));
        assert_eq!(tx.usage.messages.load(Ordering::Acquire), 0);
        assert_eq!(tx.usage.bytes.load(Ordering::Acquire), 0);
    }
}
