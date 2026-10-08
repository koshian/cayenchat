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
            next_index: 0,
        })
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
                let (data, _) = self.encoding.decode_without_bom_handling(line.as_ref());
                Ok(Some(data.into_owned()))
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

impl Encoder<String> for LineCodec {
    type Error = error::ProtocolError;

    fn encode(&mut self, msg: String, dst: &mut BytesMut) -> error::Result<()> {
        #[cfg(feature = "encoding")]
        {
            // Encode the message using the codec's encoding. Characters the
            // encoding cannot hold become numeric character references;
            // callers that care check encodability before sending.
            let (data, _, _) = self.encoding.encode(&msg);
            dst.extend_from_slice(&data);
        }

        #[cfg(not(feature = "encoding"))]
        {
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
