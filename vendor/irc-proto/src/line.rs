//! Implementation of line-delimiting codec for Tokio.

use std::io;

use bytes::BytesMut;
#[cfg(feature = "encoding")]
use encoding::label::encoding_from_whatwg_label;
#[cfg(feature = "encoding")]
use encoding::{DecoderTrap, EncoderTrap, EncodingRef};
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
    encoding: EncodingRef,
    next_index: usize,
}

impl LineCodec {
    /// Creates a new instance of LineCodec from the specified encoding.
    pub fn new(label: &str) -> error::Result<LineCodec> {
        Ok(LineCodec {
            #[cfg(feature = "encoding")]
            encoding: match encoding_from_whatwg_label(label) {
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
                // Decode the line using the codec's encoding.
                match self.encoding.decode(line.as_ref(), DecoderTrap::Replace) {
                    Ok(data) => Ok(Some(data)),
                    Err(data) => Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        &format!("Failed to decode {} as {}.", data, self.encoding.name())[..],
                    )
                    .into()),
                }
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
            // Encode the message using the codec's encoding.
            let data: error::Result<Vec<u8>> = self
                .encoding
                .encode(&msg, EncoderTrap::Replace)
                .map_err(|data| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        &format!("Failed to encode {} as {}.", data, self.encoding.name())[..],
                    )
                    .into()
                });
            // Write the encoded message to the output buffer.
            dst.extend(&data?);
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
}
