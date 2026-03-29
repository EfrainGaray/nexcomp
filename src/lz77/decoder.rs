// src/lz77/decoder.rs — LZ77 token decoder

use super::types::*;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Lz77DecodeError {
    #[error("LZ77: match offset={offset} exceeds available={available}")]
    InvalidMatch { offset: usize, available: usize },
}

/// Decode a stream of LZ77 tokens back to the original data.
///
/// For each token:
///   Literal(b) → push b
///   Match{offset, length} → copy `length` bytes from position (output.len() - offset)
///
/// Note: match can overlap with destination (offset < length), which implements
/// implicit run-length encoding (e.g., offset=1 repeats the last byte).
pub fn decode(tokens: &[Token]) -> Result<Vec<u8>, Lz77DecodeError> {
    let mut output = Vec::with_capacity(tokens.len() * 2);

    for token in tokens {
        match token {
            Token::Literal(b) => {
                output.push(*b);
            }
            Token::Match { offset, length } => {
                let offset = *offset as usize;
                let length = *length as usize;

                if offset > output.len() || offset == 0 {
                    return Err(Lz77DecodeError::InvalidMatch {
                        offset,
                        available: output.len(),
                    });
                }

                // Copy byte-by-byte to handle overlapping matches correctly.
                // This is the standard LZ77 decode: when offset < length,
                // the copy wraps around and repeats bytes (RLE behavior).
                let start = output.len() - offset;
                for i in 0..length {
                    let b = output[start + i];
                    output.push(b);
                }
            }
        }
    }

    Ok(output)
}
