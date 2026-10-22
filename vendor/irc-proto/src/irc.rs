//! Implementation of IRC codec for Tokio.
use bytes::BytesMut;
use tokio_util::codec::{Decoder, Encoder};

use crate::error;
use crate::line::LineCodec;
use crate::message::Message;

/// An IRC codec built around an inner codec.
pub struct IrcCodec {
    inner: LineCodec,
}

impl IrcCodec {
    /// Creates a new instance of IrcCodec wrapping a LineCodec with the specific encoding.
    pub fn new(label: &str) -> error::Result<IrcCodec> {
        LineCodec::new(label).map(|codec| IrcCodec { inner: codec })
    }

    /// Like [`IrcCodec::new`], with other encodings for the text of messages
    /// to some targets (see [`crate::charsets`]).
    #[cfg(feature = "encoding")]
    pub fn with_charsets(
        label: &str,
        charsets: Option<std::sync::Arc<dyn crate::charsets::BodyCharsets>>,
    ) -> error::Result<IrcCodec> {
        LineCodec::new(label).map(|codec| IrcCodec {
            inner: codec.with_charsets(charsets),
        })
    }

    /// Sanitizes the input string by cutting up to (and including) the first occurence of a line
    /// terminiating phrase (`\r\n`, `\r`, or `\n`). This is used in sending messages through the
    /// codec to prevent the injection of additional commands.
    pub fn sanitize(mut data: String) -> String {
        // n.b. ordering matters here to prefer "\r\n" over "\r"
        if let Some((pos, len)) = ["\r\n", "\r", "\n"]
            .iter()
            .flat_map(|needle| data.find(needle).map(|pos| (pos, needle.len())))
            .min_by_key(|&(pos, _)| pos)
        {
            data.truncate(pos + len);
        }
        data
    }
}

impl Decoder for IrcCodec {
    type Item = Message;
    type Error = error::ProtocolError;

    fn decode(&mut self, src: &mut BytesMut) -> error::Result<Option<Message>> {
        self.inner
            .decode(src)
            .and_then(|res| res.map_or(Ok(None), |msg| msg.parse::<Message>().map(Some)))
    }
}

impl Encoder<Message> for IrcCodec {
    type Error = error::ProtocolError;

    fn encode(&mut self, msg: Message, dst: &mut BytesMut) -> error::Result<()> {
        // Not cut at a line break, as upstream did, which sent the first part
        // as a command of its own: `LineCodec` drops the whole line (#554).
        self.inner.encode(msg.to_string(), dst)
    }
}
