//! Domain-specific codecs for NEXCOMP block compression.
//!
//! Provides block classification by data characteristics and
//! specialized codecs (RLE) for zero-heavy and low-entropy blocks.

pub mod bcj_filter;
mod bwt_cm;
pub mod bwt_codec;
pub mod delta_ans;
pub mod lzma_style;
pub mod minmask;
pub mod ppm;
pub mod rle_huffman;

use thiserror::Error;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("RLE decode: unexpected end of data")]
    UnexpectedEof,
    #[error("RLE decode: invalid varint encoding")]
    InvalidVarint,
    #[error("decompression failed: {0}")]
    DecompressFailed(String),
}

// ---------------------------------------------------------------------------
// BlockType
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BlockType {
    Zeros = 0,
    LowEntropy = 1,
    Text = 2,
    Binary = 3,
}

// ---------------------------------------------------------------------------
// Block classification
// ---------------------------------------------------------------------------

/// Classify a data block by its statistical properties.
///
/// Priority order (first match wins):
/// 1. Zeros  -- >95% of bytes are 0x00
/// 2. Text   -- >85% printable ASCII (0x20..=0x7E plus \t \n \r)
/// 3. LowEntropy -- Shannon entropy < 1.5 bits per byte
/// 4. Binary -- everything else
pub fn classify_block_type(data: &[u8]) -> BlockType {
    if data.is_empty() {
        return BlockType::Zeros;
    }

    // Build byte histogram.
    let mut hist = [0u64; 256];
    for &b in data {
        hist[b as usize] += 1;
    }

    let len = data.len() as f64;

    // Zero ratio.
    let zero_ratio = hist[0] as f64 / len;
    if zero_ratio > 0.95 {
        return BlockType::Zeros;
    }

    // Printable ratio (space..tilde + tab, newline, carriage return).
    let printable: u64 = hist[0x20..=0x7E]
        .iter()
        .copied()
        .sum::<u64>()
        + hist[b'\t' as usize]
        + hist[b'\n' as usize]
        + hist[b'\r' as usize];
    let printable_ratio = printable as f64 / len;
    if printable_ratio > 0.85 {
        return BlockType::Text;
    }

    // Shannon entropy (bits per byte).
    let entropy: f64 = hist
        .iter()
        .copied()
        .filter(|&c| c > 0)
        .map(|c| {
            let p = c as f64 / len;
            -p * p.log2()
        })
        .sum();

    if entropy < 1.5 {
        return BlockType::LowEntropy;
    }

    BlockType::Binary
}

// ---------------------------------------------------------------------------
// Varint helpers (used by RLE codec)
// ---------------------------------------------------------------------------

/// Encode a count as a 1-3 byte varint.
///
/// - count < 128       -> 1 byte:  `[count]`
/// - count < 16384     -> 2 bytes: `[0x80 | (count & 0x7F), count >> 7]`
/// - count < 2_097_152 -> 3 bytes: `[0x80 | (count & 0x7F), 0x80 | ((count >> 7) & 0x7F), count >> 14]`
fn varint_encode(count: usize, out: &mut Vec<u8>) {
    if count < 128 {
        out.push(count as u8);
    } else if count < 16384 {
        out.push(0x80 | (count & 0x7F) as u8);
        out.push((count >> 7) as u8);
    } else {
        out.push(0x80 | (count & 0x7F) as u8);
        out.push(0x80 | ((count >> 7) & 0x7F) as u8);
        out.push((count >> 14) as u8);
    }
}

/// Decode a varint from `data` starting at `pos`. Returns (value, bytes_consumed).
fn varint_decode(data: &[u8], pos: usize) -> Result<(usize, usize), CodecError> {
    if pos >= data.len() {
        return Err(CodecError::UnexpectedEof);
    }
    let b0 = data[pos] as usize;
    if b0 & 0x80 == 0 {
        return Ok((b0, 1));
    }
    if pos + 1 >= data.len() {
        return Err(CodecError::UnexpectedEof);
    }
    let b1 = data[pos + 1] as usize;
    if b1 & 0x80 == 0 {
        let val = (b0 & 0x7F) | (b1 << 7);
        return Ok((val, 2));
    }
    if pos + 2 >= data.len() {
        return Err(CodecError::UnexpectedEof);
    }
    let b2 = data[pos + 2] as usize;
    let val = (b0 & 0x7F) | ((b1 & 0x7F) << 7) | (b2 << 14);
    Ok((val, 3))
}

// ---------------------------------------------------------------------------
// RLE codec
// ---------------------------------------------------------------------------

/// RLE-encode `data`. Output format: sequence of `(byte_value, count_varint)` pairs.
pub fn rle_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    if data.is_empty() {
        return out;
    }

    let mut i = 0;
    while i < data.len() {
        let val = data[i];
        let mut run = 1usize;
        while i + run < data.len() && data[i + run] == val {
            run += 1;
        }
        out.push(val);
        varint_encode(run, &mut out);
        i += run;
    }
    out
}

/// Decode an RLE-encoded stream back to raw bytes.
pub fn rle_decode(data: &[u8]) -> Result<Vec<u8>, CodecError> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        if pos >= data.len() {
            return Err(CodecError::UnexpectedEof);
        }
        let val = data[pos];
        pos += 1;
        let (count, consumed) = varint_decode(data, pos)?;
        pos += consumed;
        if count == 0 {
            return Err(CodecError::DecompressFailed(
                "RLE run count of zero".into(),
            ));
        }
        out.resize(out.len() + count, val);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Block compress / decompress
// ---------------------------------------------------------------------------

/// Compress a data block using the codec appropriate for its type.
///
/// Returns `(block_type, compressed_data)`.
pub fn compress_block(data: &[u8]) -> (BlockType, Vec<u8>) {
    let bt = classify_block_type(data);
    let compressed = match bt {
        BlockType::Zeros | BlockType::LowEntropy => rle_encode(data),
        BlockType::Text | BlockType::Binary => data.to_vec(),
    };
    (bt, compressed)
}

/// Decompress a block given its type and compressed payload.
pub fn decompress_block(block_type: BlockType, data: &[u8]) -> Result<Vec<u8>, CodecError> {
    match block_type {
        BlockType::Zeros | BlockType::LowEntropy => rle_decode(data),
        BlockType::Text | BlockType::Binary => Ok(data.to_vec()),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rle_roundtrip_zeros() {
        let original = vec![0u8; 65536];
        let encoded = rle_encode(&original);
        assert!(
            encoded.len() < 10,
            "RLE of 64KB zeros should be < 10 bytes, got {}",
            encoded.len()
        );
        let decoded = rle_decode(&encoded).expect("decode failed");
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_rle_roundtrip_mixed() {
        let mut data = Vec::new();
        // Some runs of different bytes.
        data.extend(std::iter::repeat(0xAA).take(300));
        data.extend(std::iter::repeat(0x55).take(1));
        data.extend(std::iter::repeat(0xFF).take(20000));
        data.extend(std::iter::repeat(0x00).take(5));
        data.push(0x42);

        let encoded = rle_encode(&data);
        let decoded = rle_decode(&encoded).expect("decode failed");
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_classify_zeros() {
        let data = vec![0u8; 65536];
        assert_eq!(classify_block_type(&data), BlockType::Zeros);
    }

    #[test]
    fn test_classify_text() {
        let text = b"The quick brown fox jumps over the lazy dog. \
                     Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
                     Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.";
        assert_eq!(classify_block_type(text), BlockType::Text);
    }

    #[test]
    fn test_classify_binary() {
        // Pseudo-random bytes with high entropy (use a simple LCG).
        let mut data = vec![0u8; 4096];
        let mut state: u64 = 0xDEAD_BEEF;
        for b in data.iter_mut() {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            *b = (state >> 33) as u8;
        }
        assert_eq!(classify_block_type(&data), BlockType::Binary);
    }

    #[test]
    fn test_classify_low_entropy() {
        // Data with very few distinct values but not zeros and not printable.
        // Use two non-printable byte values in roughly equal proportion.
        let mut data = vec![0x01u8; 8192];
        for i in 0..data.len() {
            if i % 3 == 0 {
                data[i] = 0x02;
            }
        }
        let bt = classify_block_type(&data);
        assert_eq!(
            bt,
            BlockType::LowEntropy,
            "expected LowEntropy, got {:?}",
            bt
        );
    }

    #[test]
    fn test_compress_block_zeros() {
        let data = vec![0u8; 65536];
        let (bt, compressed) = compress_block(&data);
        assert_eq!(bt, BlockType::Zeros);
        assert!(
            compressed.len() < 20,
            "compressed 64KB zeros should be < 20 bytes, got {}",
            compressed.len()
        );
        // Round-trip.
        let decompressed = decompress_block(bt, &compressed).expect("decompress failed");
        assert_eq!(decompressed, data);
    }
}
