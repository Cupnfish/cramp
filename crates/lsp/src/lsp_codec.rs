use crate::error::RaError;
use bytes::{BufMut, BytesMut};
use log::{error, trace, warn};
use std::io;
use std::str;
use tokio_util::codec::{Decoder, Encoder};

const CONTENT_LENGTH_HEADER: &str = "Content-Length: ";
const SEPARATOR: &[u8] = b"\r\n\r\n";

#[derive(Debug, Clone, Default)]
enum DecodeState {
    #[default]
    Header,
    Body(usize), // Content-Length
}

/// Codec for Language Server Protocol messages.
/// Handles the `Content-Length: <N>\r\n\r\n<BODY>` framing.
#[derive(Debug, Default)]
pub struct LspCodec {
    state: DecodeState,
}

// --- Encoder ---
impl Encoder<String> for LspCodec {
    type Error = RaError;

    fn encode(&mut self, item: String, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let content_length = item.as_bytes().len();
        let header = format!("{}{}{}", CONTENT_LENGTH_HEADER, content_length, "\r\n\r\n");

        dst.reserve(header.len() + content_length);
        dst.put(header.as_bytes());
        dst.put(item.as_bytes());
        trace!("Encoded: {}{}", header, item);
        Ok(())
    }
}

// --- Decoder ---
impl Decoder for LspCodec {
    type Item = String;
    type Error = RaError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        loop {
            match self.state {
                DecodeState::Header => {
                    if let Some(separator_pos) = find_subarray(src, SEPARATOR) {
                        let header_len = separator_pos;
                        let full_header_len = separator_pos + SEPARATOR.len();
                        let header_bytes = src.split_to(full_header_len);

                        let content_length = parse_content_length(&header_bytes[..header_len])
                            .map_err(|e| {
                                error!("Failed to parse header: {:?}", e);
                                RaError::Lsp(format!("Invalid LSP header: {}", e))
                            })?;

                        self.state = DecodeState::Body(content_length);
                        continue;
                    } else {
                        return Ok(None);
                    }
                }
                DecodeState::Body(content_length) => {
                    if src.len() >= content_length {
                        let body_bytes = src.split_to(content_length);

                        let body_str = str::from_utf8(&body_bytes)
                            .map_err(|e| {
                                warn!("Invalid UTF-8 in LSP message body: {}", e);
                                RaError::Lsp(format!("Invalid UTF-8 in body: {}", e))
                            })?
                            .to_string();

                        self.state = DecodeState::Header;
                        trace!("Decoded body ({} bytes): {}", content_length, body_str);
                        return Ok(Some(body_str));
                    } else {
                        return Ok(None);
                    }
                }
            }
        }
    }
}

/// Helper to find the start index of a subarray
fn find_subarray(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Helper to parse the Content-Length header value
fn parse_content_length(header_bytes: &[u8]) -> io::Result<usize> {
    let header_str = str::from_utf8(header_bytes).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Header not UTF-8: {}", e),
        )
    })?;

    for line in header_str.lines() {
        if line.starts_with(CONTENT_LENGTH_HEADER) {
            let len_str = line[CONTENT_LENGTH_HEADER.len()..].trim();
            return len_str.parse::<usize>().map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Invalid Content-Length value '{}': {}", len_str, e),
                )
            });
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "Content-Length header not found",
    ))
}
