//! Implementation of line-delimiting codec for Tokio.

use std::io;

use bytes::BytesMut;
#[cfg(feature = "encoding")]
use encoding_rs::Encoding;
use tokio_util::codec::{Decoder, Encoder};

use crate::error;

/// The longest line accepted, in bytes including the line ending: the 8,191
/// bytes of tags IRCv3 allows plus the 512-byte message, with room to spare.
/// A longer line is a protocol error, so a peer that never sends a newline
/// cannot make the buffer grow without bound.
pub const MAX_LINE_BYTES: usize = 16 * 1024;

/// A line-based codec parameterized by an encoding.
pub struct LineCodec {
    #[cfg(feature = "encoding")]
    encoding: &'static Encoding,
    /// Other encodings for the text of messages to some targets.
    #[cfg(feature = "encoding")]
    charsets: Option<std::sync::Arc<dyn crate::charsets::BodyCharsets>>,
    next_index: usize,
}

impl LineCodec {
    /// Creates a new instance of LineCodec from the specified encoding (a
    /// WHATWG label). Labels whose encoder writes another encoding (UTF-16,
    /// "replacement") are refused.
    pub fn new(label: &str) -> error::Result<LineCodec> {
        Ok(LineCodec {
            #[cfg(feature = "encoding")]
            encoding: match Encoding::for_label(label.as_bytes())
                .filter(|encoding| encoding.output_encoding() == *encoding)
            {
                Some(x) => x,
                None => {
                    return Err(error::ProtocolError::Io(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        &format!("Attempted to use unknown codec {}.", label)[..],
                    )));
                }
            },
            #[cfg(feature = "encoding")]
            charsets: None,
            next_index: 0,
        })
    }

    /// Converts the text of messages to the targets `charsets` knows with
    /// their own encodings, in both directions.
    #[cfg(feature = "encoding")]
    pub fn with_charsets(
        mut self,
        charsets: Option<std::sync::Arc<dyn crate::charsets::BodyCharsets>>,
    ) -> LineCodec {
        self.charsets = charsets;
        self
    }
}

impl Decoder for LineCodec {
    type Item = String;
    type Error = error::ProtocolError;

    fn decode(&mut self, src: &mut BytesMut) -> error::Result<Option<String>> {
        let newline = src[self.next_index..].iter().position(|b| *b == b'\n');
        if newline.map_or(src.len(), |offset| self.next_index + offset + 1) > MAX_LINE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Received a line longer than {MAX_LINE_BYTES} bytes."),
            )
            .into());
        }
        if let Some(offset) = newline {
            // Remove the next frame from the buffer.
            let line = src.split_to(self.next_index + offset + 1);

            // Set the search start index back to 0 since we found a newline.
            self.next_index = 0;

            #[cfg(feature = "encoding")]
            {
                // Decode the line using the codec's encoding. Malformed bytes
                // become U+FFFD, as with the replacing decoder before.
                Ok(Some(crate::charsets::decode_line(
                    line.as_ref(),
                    self.encoding,
                    self.charsets.as_deref(),
                )))
            }

            #[cfg(not(feature = "encoding"))]
            {
                match String::from_utf8(line.to_vec()) {
                    Ok(data) => Ok(Some(data)),
                    Err(data) => Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        &format!("Failed to decode {} as UTF-8.", data)[..],
                    )
                    .into()),
                }
            }
        } else {
            // Set the search start index to the current length since we know that none of the
            // characters we've already looked at are newlines.
            self.next_index = src.len();
            Ok(None)
        }
    }
}

/// Whether `line`, already formatted, is one line: apart from the closing
/// `\r\n` it holds no CR, LF or NUL. One pass, no allocation.
///
/// This is the last stop before the socket (CRLF injection, #554): every line
/// of the `irc` crate passes `encode`, including its own PONG, CTCP replies
/// and JOINs. A line that fails is dropped whole, unwritten and unlogged (it
/// may hold secrets); the lines after it go on.
fn is_single_line(line: &str) -> bool {
    let body = line.strip_suffix("\r\n").unwrap_or(line);
    !body.bytes().any(|byte| matches!(byte, b'\r' | b'\n' | 0))
}

impl Encoder<String> for LineCodec {
    type Error = error::ProtocolError;

    fn encode(&mut self, msg: String, dst: &mut BytesMut) -> error::Result<()> {
        #[cfg(feature = "encoding")]
        {
            // Encode the message using the codec's encoding. Characters the
            // encoding cannot hold become numeric character references;
            // callers that care check encodability before sending.
            let pinned = match self
                .charsets
                .as_deref()
                .and_then(|charsets| charsets.take_pin(&msg))
            {
                Some(crate::charsets::Pin::Skip) => return Ok(()),
                Some(crate::charsets::Pin::Encoding(encoding)) => {
                    Some(crate::charsets::Fixed(encoding))
                }
                None => None,
            };
            if !is_single_line(&msg) {
                if let Some(charsets) = self.charsets.as_deref() {
                    charsets.refused_line();
                }
                return Ok(());
            }
            let charsets = match &pinned {
                Some(fixed) => Some(fixed as &dyn crate::charsets::BodyCharsets),
                None => self.charsets.as_deref(),
            };
            let (data, _) = crate::charsets::encode_line(&msg, self.encoding, charsets);
            dst.extend_from_slice(&data);
        }

        #[cfg(not(feature = "encoding"))]
        {
            if !is_single_line(&msg) {
                return Ok(());
            }
            dst.extend(msg.into_bytes());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{LineCodec, MAX_LINE_BYTES};
    use bytes::BytesMut;
    use tokio_util::codec::Decoder;

    #[test]
    fn a_line_up_to_the_limit_is_decoded() {
        let mut codec = LineCodec::new("utf-8").unwrap();
        let line = format!("{}\r\n", "a".repeat(MAX_LINE_BYTES - 2));
        let mut src = BytesMut::from(line.as_bytes());
        assert_eq!(codec.decode(&mut src).unwrap(), Some(line));
    }

    #[test]
    fn a_longer_line_is_an_error_with_or_without_its_newline() {
        let mut codec = LineCodec::new("utf-8").unwrap();
        let mut src = BytesMut::from("a".repeat(MAX_LINE_BYTES).as_bytes());
        assert_eq!(codec.decode(&mut src).unwrap(), None);
        src.extend_from_slice(b"a");
        assert!(codec.decode(&mut src).is_err());

        let mut codec = LineCodec::new("utf-8").unwrap();
        let line = format!("{}\r\n", "a".repeat(MAX_LINE_BYTES - 1));
        assert!(codec.decode(&mut BytesMut::from(line.as_bytes())).is_err());
    }

    #[test]
    fn the_limit_applies_to_each_line_not_to_the_buffer() {
        let mut codec = LineCodec::new("utf-8").unwrap();
        let line = format!("{}\r\n", "a".repeat(MAX_LINE_BYTES - 2));
        let mut src = BytesMut::from(format!("{line}{line}").as_bytes());
        assert_eq!(codec.decode(&mut src).unwrap(), Some(line.clone()));
        assert_eq!(codec.decode(&mut src).unwrap(), Some(line));
    }

    #[test]
    fn a_line_ending_split_across_reads_counts_toward_the_limit() {
        let mut codec = LineCodec::new("utf-8").unwrap();
        let mut src = BytesMut::from("a".repeat(MAX_LINE_BYTES - 2).as_bytes());
        src.extend_from_slice(b"\r");
        assert_eq!(codec.decode(&mut src).unwrap(), None);
        src.extend_from_slice(b"\n");
        assert_eq!(codec.decode(&mut src).unwrap().unwrap().len(), MAX_LINE_BYTES);

        let mut codec = LineCodec::new("utf-8").unwrap();
        let mut src = BytesMut::from("a".repeat(MAX_LINE_BYTES - 1).as_bytes());
        src.extend_from_slice(b"\r");
        assert_eq!(codec.decode(&mut src).unwrap(), None);
        src.extend_from_slice(b"\n");
        assert!(codec.decode(&mut src).is_err());
    }

    #[test]
    fn a_long_line_after_a_normal_one_is_an_error() {
        let mut codec = LineCodec::new("utf-8").unwrap();
        let mut src = BytesMut::from("PING x\r\n".as_bytes());
        src.extend_from_slice("a".repeat(MAX_LINE_BYTES + 1).as_bytes());
        assert_eq!(codec.decode(&mut src).unwrap(), Some("PING x\r\n".into()));
        assert!(codec.decode(&mut src).is_err());
    }

    #[cfg(feature = "encoding")]
    #[test]
    fn a_pinned_line_is_written_with_its_pinned_encoding() {
        use crate::charsets::BodyCharsets;
        use std::sync::{Arc, Mutex};
        use tokio_util::codec::Encoder;

        struct Pinned(Mutex<Option<crate::charsets::Pin>>);
        impl BodyCharsets for Pinned {
            fn is_empty(&self) -> bool {
                false
            }
            fn decoder(&self, _: &str) -> Option<&'static encoding_rs::Encoding> {
                None
            }
            // What the setting has become since the line was queued.
            fn encoder(&self, _: &str) -> Option<&'static encoding_rs::Encoding> {
                Some(encoding_rs::UTF_8)
            }
            fn take_pin(&self, _: &str) -> Option<crate::charsets::Pin> {
                self.0.lock().unwrap().take()
            }
        }

        use crate::charsets::Pin;
        let pins = Arc::new(Pinned(Mutex::new(Some(Pin::Encoding(Some(
            encoding_rs::SHIFT_JIS,
        ))))));
        let mut codec = LineCodec::new("utf-8").unwrap().with_charsets(Some(pins));
        let mut dst = BytesMut::new();
        codec.encode("PRIVMSG #a :あ\r\n".into(), &mut dst).unwrap();
        assert_eq!(&dst[..], b"PRIVMSG #a :\x82\xa0\r\n");
        // The pin is used up: the next line follows the current setting.
        let mut dst = BytesMut::new();
        codec.encode("PRIVMSG #a :あ\r\n".into(), &mut dst).unwrap();
        assert_eq!(&dst[..], "PRIVMSG #a :あ\r\n".as_bytes());
        // A line the connection no longer allows is not written at all.
        let pins = Arc::new(Pinned(Mutex::new(Some(Pin::Skip))));
        let mut codec = LineCodec::new("utf-8").unwrap().with_charsets(Some(pins));
        let mut dst = BytesMut::new();
        codec.encode("PRIVMSG #a :あ\r\n".into(), &mut dst).unwrap();
        assert!(dst.is_empty());
    }

    #[test]
    fn a_line_with_cr_lf_or_nul_is_not_written() {
        use tokio_util::codec::Encoder;
        let mut codec = LineCodec::new("utf-8").unwrap();
        let mut dst = BytesMut::new();
        for bad in [
            "PRIVMSG #a :x\r\nQUIT\r\n",
            "PRIVMSG #a\n:x\r\n",
            "PRIVMSG #a :x\0\r\n",
            "PRIVMSG #a :x\r",
            "@k=\rv PING\r\n",
            "PRIVMSG #a :x\r\n\r\n",
        ] {
            codec.encode(bad.into(), &mut dst).unwrap();
        }
        assert!(dst.is_empty());
        // The lines after a dropped one go on.
        codec.encode("PRIVMSG #a :ok\r\n".into(), &mut dst).unwrap();
        assert_eq!(&dst[..], b"PRIVMSG #a :ok\r\n");
    }

    #[cfg(feature = "encoding")]
    #[test]
    fn japanese_legacy_encodings_round_trip_and_bad_bytes_are_replaced() {
        use tokio_util::codec::Encoder;
        for (label, wire) in [
            ("ISO-2022-JP", &b"PRIVMSG #a :\x1b$B$3$s$K$A$O\x1b(B\r\n"[..]),
            ("Shift_JIS", &b"PRIVMSG #a :\x82\xb1\x82\xf1\x82\xc9\x82\xbf\x82\xcd\r\n"[..]),
            ("EUC-JP", &b"PRIVMSG #a :\xa4\xb3\xa4\xf3\xa4\xcb\xa4\xc1\xa4\xcf\r\n"[..]),
        ] {
            let mut codec = LineCodec::new(label).unwrap();
            let line = codec.decode(&mut BytesMut::from(wire)).unwrap().unwrap();
            assert_eq!(line, "PRIVMSG #a :こんにちは\r\n", "{label}");
            let mut encoded = BytesMut::new();
            codec.encode(line, &mut encoded).unwrap();
            assert_eq!(&encoded[..], wire, "{label}");
        }
        let mut codec = LineCodec::new("utf-8").unwrap();
        let line = codec.decode(&mut BytesMut::from(&b"a\xffb\r\n"[..])).unwrap();
        assert_eq!(line.as_deref(), Some("a\u{FFFD}b\r\n"));
        // Unknown labels, and labels whose encoder writes something else.
        assert!(LineCodec::new("no-such-charset").is_err());
        assert!(LineCodec::new("utf-16le").is_err());
    }
}
