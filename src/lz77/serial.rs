// src/lz77/serial.rs — Serialize/deserialize LZ77 tokens to/from bytes
//
// Format per token:
//   Literal:  [0x00][byte]                        = 2 bytes
//   Match:    [0x01][offset_hi][offset_lo+len_hi][len_lo]  = 4 bytes
//             offset: 22 bits (covers up to 4MB window)
//             length - MIN_MATCH: 10 bits (covers up to 1027)
//             packed into 3 bytes after the 0x01 flag

use super::types::*;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Lz77SerialError {
    #[error("LZ77: truncated token at position {pos}")]
    TruncatedToken { pos: usize },
    #[error("LZ77: invalid token type 0x{byte:02X} at position {pos}")]
    InvalidTokenType { byte: u8, pos: usize },
}

/// Serialize a token stream to bytes.
pub fn serialize(tokens: &[Token]) -> Vec<u8> {
    let mut out = Vec::with_capacity(tokens.len() * 3);

    for token in tokens {
        match token {
            Token::Literal(b) => {
                out.push(0x00);
                out.push(*b);
            }
            Token::Match { offset, length } => {
                // Pack: offset (22 bits) | length_enc (10 bits) into 32 bits
                // length_enc = length - MIN_MATCH
                let length_enc = (*length as u32).saturating_sub(MIN_MATCH as u32);
                let packed: u32 = (*offset << 10) | (length_enc & 0x3FF);

                out.push(0x01);
                // 4 bytes big-endian for the full 32-bit packed value
                out.push((packed >> 24) as u8);
                out.push((packed >> 16) as u8);
                out.push((packed >> 8) as u8);
                out.push(packed as u8);
            }
        }
    }

    out
}

/// Deserialize bytes back to a token stream.
pub fn deserialize(data: &[u8]) -> Result<Vec<Token>, Lz77SerialError> {
    let mut tokens = Vec::new();
    let mut pos = 0;

    while pos < data.len() {
        match data[pos] {
            0x00 => {
                // Literal: flag + 1 byte value
                if pos + 1 >= data.len() {
                    return Err(Lz77SerialError::TruncatedToken { pos });
                }
                tokens.push(Token::Literal(data[pos + 1]));
                pos += 2;
            }
            0x01 => {
                // Match: flag + 4 bytes packed (offset:22 | length_enc:10)
                if pos + 5 > data.len() {
                    return Err(Lz77SerialError::TruncatedToken { pos });
                }
                let packed: u32 = (data[pos + 1] as u32) << 24
                    | (data[pos + 2] as u32) << 16
                    | (data[pos + 3] as u32) << 8
                    | data[pos + 4] as u32;

                let offset = packed >> 10;
                let length_enc = packed & 0x3FF;
                let length = (length_enc + MIN_MATCH as u32) as u16;

                tokens.push(Token::Match { offset, length });
                pos += 5;
            }
            other => {
                return Err(Lz77SerialError::InvalidTokenType {
                    byte: other,
                    pos,
                });
            }
        }
    }

    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_roundtrip() {
        let tokens = vec![
            Token::Literal(0x42),
            Token::Match {
                offset: 100,
                length: 10,
            },
            Token::Literal(0xFF),
            Token::Match {
                offset: 4_000_000,
                length: 258,
            },
        ];
        let serialized = serialize(&tokens);
        let deserialized = deserialize(&serialized).expect("deserialize ok");
        assert_eq!(tokens, deserialized);
    }

    #[test]
    fn test_serialize_min_max_values() {
        let tokens = vec![
            Token::Match {
                offset: 1,
                length: MIN_MATCH as u16,
            },
            Token::Match {
                // Max storable offset: 22 bits = 4,194,303 (MAX_WINDOW - 1)
                offset: (MAX_WINDOW - 1) as u32,
                length: MAX_MATCH as u16,
            },
        ];
        let serialized = serialize(&tokens);
        let deserialized = deserialize(&serialized).expect("deserialize ok");
        assert_eq!(tokens, deserialized);
    }
}
