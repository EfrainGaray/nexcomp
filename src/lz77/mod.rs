// src/lz77/mod.rs — LZ77 pre-pass for NEXCOMP v0.3
//
// 4MB sliding window with hash chain match finding and lazy matching.
// Captures long-distance repetitions that Re-Pair misses (Re-Pair only
// sees bigrams within a single block).
//
// Complementary to Re-Pair: LZ77 emits (literal, match) tokens →
// serialized to bytes → Re-Pair finds grammar structure in the token stream.

pub mod decoder;
pub mod encoder;
pub mod hash;
pub mod huffman;
pub mod optimal;
pub mod serial;
pub mod types;

pub use decoder::decode as lz77_decode;
pub use decoder::decode_with_limit as lz77_decode_with_limit;
pub use decoder::Lz77DecodeError;
pub use encoder::{EncoderStats, Lz77Encoder};
pub use serial::{deserialize, serialize, Lz77SerialError};
pub use types::*;

/// Compress: bytes → serialized LZ77 tokens.
pub fn compress(data: &[u8]) -> Vec<u8> {
    let mut enc = Lz77Encoder::new();
    let (tokens, _rep_matches) = enc.encode(data);
    serialize(&tokens)
}

/// Decompress: serialized LZ77 tokens → original bytes.
pub fn decompress(data: &[u8]) -> Result<Vec<u8>, Lz77Error> {
    let tokens = deserialize(data).map_err(Lz77Error::Serial)?;
    lz77_decode(&tokens).map_err(Lz77Error::Decode)
}

/// Combined LZ77 error type.
#[derive(Debug, thiserror::Error)]
pub enum Lz77Error {
    #[error("{0}")]
    Serial(#[from] Lz77SerialError),
    #[error("{0}")]
    Decode(#[from] Lz77DecodeError),
}

/// Measure LZ77 compression without full serialization.
/// Returns (input_len, estimated_serialized_len, match_ratio).
pub fn measure(data: &[u8]) -> (usize, usize, f64) {
    let mut enc = Lz77Encoder::new();
    let (tokens, rep_matches) = enc.encode(data);
    let stats = Lz77Encoder::stats_with_reps(&tokens, rep_matches);
    let serialized_len: usize = tokens
        .iter()
        .map(|t| if t.is_match() { 5 } else { 2 })
        .sum();
    (data.len(), serialized_len, stats.match_ratio)
}
