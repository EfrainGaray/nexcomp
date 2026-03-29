//! Adaptive block selector — chooses the best codec per block with no-regression guarantee.

use crate::classifier_v2::{classify_block_v2, CodecChoice};
use crate::codecs::bcj_filter;
use crate::codecs::bwt_codec;
use crate::codecs::delta_ans;
use crate::codecs::lzma_style;
use crate::codecs::ppm;
use crate::codecs::rle_huffman;
use crate::lz77::{self, huffman, Lz77Encoder};

/// Codec ID stored in the compressed block header
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CodecId {
    Lz77Huffman = 0,
    LzmaStyle = 1,
    DeltaAns = 2,
    RleHuffman = 3,
    Passthrough = 4,
    BwtRans = 5,
    Ppm = 6,
}

impl CodecId {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Lz77Huffman,
            1 => Self::LzmaStyle,
            2 => Self::DeltaAns,
            3 => Self::RleHuffman,
            5 => Self::BwtRans,
            6 => Self::Ppm,
            _ => Self::Passthrough,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Lz77Huffman => "lz77huf",
            Self::LzmaStyle => "lzma",
            Self::DeltaAns => "delta",
            Self::RleHuffman => "rlehuf",
            Self::Passthrough => "store",
            Self::BwtRans => "bwt",
            Self::Ppm => "ppm",
        }
    }
}

/// Baseline: LZ77 + per-block Huffman (v1.1 proven method)
fn compress_baseline(data: &[u8]) -> Vec<u8> {
    let mut enc = Lz77Encoder::new();
    let (tokens, _) = enc.encode(data);
    // Try multiple block sizes, pick smallest
    let bs8k = huffman::huffman_encode_blocked(&tokens, 8192);
    let bs16k = huffman::huffman_encode_blocked(&tokens, 16384);
    let bs4k = huffman::huffman_encode_blocked(&tokens, 4096);
    let mut best = bs8k;
    if bs16k.len() < best.len() { best = bs16k; }
    if bs4k.len() < best.len() { best = bs4k; }
    best
}

fn decompress_baseline(data: &[u8]) -> Vec<u8> {
    let tokens = huffman::huffman_decode_blocked(data);
    lz77::lz77_decode(&tokens).expect("LZ77 decode failed")
}

/// Result of adaptive compression, including whether BCJ pre-filter was applied.
struct AdaptiveResult {
    compressed: Vec<u8>,
    codec: CodecId,
    bcj_applied: bool,
}

/// Run the codec selection on `data`, returning the best compressed result.
fn select_best_codec(data: &[u8]) -> (Vec<u8>, CodecId) {
    let (choice, metrics) = classify_block_v2(data);

    // Always compute baseline
    let baseline = compress_baseline(data);
    let mut best = baseline;
    let mut best_codec = CodecId::Lz77Huffman;

    // Try the classifier-suggested codec
    let (candidate, candidate_codec) = match choice {
        CodecChoice::DeltaAns => {
            match delta_ans::delta_ans_encode(data) {
                Ok(c) => (c, CodecId::DeltaAns),
                Err(_) => (Vec::new(), CodecId::Lz77Huffman),
            }
        }
        CodecChoice::RleHuffman => {
            let c = rle_huffman::rle_huffman_encode(data);
            (c, CodecId::RleHuffman)
        }
        CodecChoice::LzmaStyle => {
            let c = lzma_style::encode_block(data);
            (c, CodecId::LzmaStyle)
        }
        CodecChoice::Passthrough => {
            (data.to_vec(), CodecId::Passthrough)
        }
        CodecChoice::Lz77Huffman => {
            (Vec::new(), CodecId::Lz77Huffman)
        }
    };

    if !candidate.is_empty() && candidate.len() < best.len() {
        best = candidate;
        best_codec = candidate_codec;
    }

    // Also try LZMA if classifier didn't already pick it (LZMA is strong on most data)
    if choice != CodecChoice::LzmaStyle {
        let lzma = lzma_style::encode_block(data);
        if lzma.len() < best.len() {
            best = lzma;
            best_codec = CodecId::LzmaStyle;
        }
    }

    // Try BWT for text-heavy data.
    // Tuned on Calgary corpus (tests/threshold_tuning.rs):
    //   - BWT only beats LZMA on high-ASCII blocks (ratio > 0.90)
    //   - BWT overhead doesn't amortize on blocks < 32 KB
    //   - BWT never helps on high-entropy blocks (> 5.5 bpb)
    //   - For medium blocks (4KB-32KB), only try if very text-heavy (> 0.96)
    //     and low entropy (< 5.0), targeting files like paper1-paper3
    let try_bwt = if data.len() >= 32768 {
        metrics.ascii_ratio > 0.90 && metrics.entropy <= 5.5
    } else if data.len() >= 4096 {
        metrics.ascii_ratio > 0.96 && metrics.entropy <= 5.0
    } else {
        false
    };

    if try_bwt {
        let bwt = bwt_codec::bwt_compress(data);
        if bwt.len() < best.len() {
            best = bwt;
            best_codec = CodecId::BwtRans;
        }
    }

    // Try PPM for text-heavy blocks where it can beat BWT and LZMA
    if metrics.ascii_ratio > 0.80 && metrics.entropy < 5.5 && data.len() >= 256 {
        let ppm_compressed = ppm::ppm_compress(data);
        if ppm_compressed.len() < best.len() {
            best = ppm_compressed;
            best_codec = CodecId::Ppm;
        }
    }

    (best, best_codec)
}

/// Compress a block adaptively with no-regression guarantee.
/// Also tries BCJ pre-filtering for binary/executable data.
fn compress_block_adaptive(data: &[u8]) -> AdaptiveResult {
    if data.is_empty() {
        return AdaptiveResult {
            compressed: Vec::new(),
            codec: CodecId::Passthrough,
            bcj_applied: false,
        };
    }

    // Compress without BCJ
    let (best_plain, best_plain_codec) = select_best_codec(data);

    // Try BCJ pre-filter for binary data that looks like x86 code
    let (_, metrics) = classify_block_v2(data);
    if metrics.ascii_ratio <= 0.70 && bcj_filter::is_likely_x86(data) {
        let filtered = bcj_filter::bcj_encode(data);
        let (best_bcj, best_bcj_codec) = select_best_codec(&filtered);
        if best_bcj.len() < best_plain.len() {
            return AdaptiveResult {
                compressed: best_bcj,
                codec: best_bcj_codec,
                bcj_applied: true,
            };
        }
    }

    AdaptiveResult {
        compressed: best_plain,
        codec: best_plain_codec,
        bcj_applied: false,
    }
}

/// Public API: compress a block adaptively and return (compressed_data, codec_id).
///
/// This is a convenience wrapper over the internal adaptive pipeline.
/// The BCJ flag is not exposed here; use `adaptive_compress` for the full wire format.
pub fn compress_block_adaptive_pub(data: &[u8]) -> (Vec<u8>, CodecId) {
    let result = compress_block_adaptive(data);
    (result.compressed, result.codec)
}

/// Decompress a block given its codec ID.
pub fn decompress_block_adaptive(codec: CodecId, data: &[u8]) -> Vec<u8> {
    match codec {
        CodecId::Lz77Huffman => decompress_baseline(data),
        CodecId::LzmaStyle => {
            lzma_style::decode_block(data).unwrap_or_else(|_| decompress_baseline(data))
        }
        CodecId::DeltaAns => {
            delta_ans::delta_ans_decode(data).unwrap_or_else(|_| decompress_baseline(data))
        }
        CodecId::RleHuffman => rle_huffman::rle_huffman_decode(data),
        CodecId::BwtRans => bwt_codec::bwt_decompress(data),
        CodecId::Ppm => ppm::ppm_decompress(data),
        CodecId::Passthrough => data.to_vec(),
    }
}

/// Full adaptive compress: data -> wire format
/// Format: [4B magic "NX12"][4B orig_len LE][1B codec_id][1B bcj_flag][compressed_data]
pub fn adaptive_compress(data: &[u8]) -> Vec<u8> {
    let result = compress_block_adaptive(data);
    let mut out = Vec::with_capacity(10 + result.compressed.len());
    out.extend_from_slice(b"NX12");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.push(result.codec as u8);
    out.push(if result.bcj_applied { 1 } else { 0 });
    out.extend_from_slice(&result.compressed);
    out
}

/// Full adaptive decompress: wire format -> data
pub fn adaptive_decompress(payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() >= 10, "payload too short");
    assert_eq!(&payload[0..4], b"NX12", "bad magic");
    let _orig_len = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]) as usize;
    let codec = CodecId::from_u8(payload[8]);
    let bcj_applied = payload[9] != 0;
    let decompressed = decompress_block_adaptive(codec, &payload[10..]);
    if bcj_applied {
        bcj_filter::bcj_decode(&decompressed)
    } else {
        decompressed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adaptive_roundtrip_text() {
        let data = b"The quick brown fox jumps over the lazy dog. \
                     Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
                     Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. \
                     The quick brown fox jumps over the lazy dog again and again.";
        let compressed = adaptive_compress(data);
        let decompressed = adaptive_decompress(&compressed);
        assert_eq!(&data[..], &decompressed[..]);
    }

    #[test]
    fn test_adaptive_roundtrip_empty() {
        let data = b"";
        let compressed = adaptive_compress(data);
        let decompressed = adaptive_decompress(&compressed);
        assert_eq!(&data[..], &decompressed[..]);
    }

    #[test]
    fn test_adaptive_no_regression() {
        // Generate various data types and verify candidate <= baseline
        let text = b"Hello world! This is a test of the adaptive compressor. \
                     It should never produce output larger than the baseline.";
        let result = compress_block_adaptive(text);
        let baseline = compress_baseline(text);
        assert!(result.compressed.len() <= baseline.len(),
            "regression: {} ({}) > baseline ({})", result.codec.name(), result.compressed.len(), baseline.len());
    }

    #[test]
    fn test_adaptive_roundtrip_correlated() {
        // Highly correlated data (simulated geophysical)
        let mut data = vec![0u8; 4096];
        let mut val: u8 = 128;
        for i in 0..data.len() {
            val = val.wrapping_add(((i as u8).wrapping_mul(3)) & 0x03);
            data[i] = val;
        }
        let compressed = adaptive_compress(&data);
        let decompressed = adaptive_decompress(&compressed);
        assert_eq!(data, decompressed);
    }

    #[test]
    fn test_adaptive_roundtrip_runs() {
        // Data with long runs (image-like)
        let mut data = Vec::new();
        for _ in 0..100 { data.extend(std::iter::repeat(0x00).take(500)); }
        for _ in 0..50 { data.extend(std::iter::repeat(0xFF).take(300)); }
        let compressed = adaptive_compress(&data);
        let decompressed = adaptive_decompress(&compressed);
        assert_eq!(data, decompressed);
    }

    #[test]
    fn test_adaptive_roundtrip_x86_like() {
        // Simulate x86-like binary data with ELF header and CALL/JMP instructions
        let mut data = vec![0u8; 8192];
        // ELF magic header
        data[0] = 0x7F;
        data[1] = b'E';
        data[2] = b'L';
        data[3] = b'F';
        // Sprinkle CALL and JMP instructions throughout
        let mut pos = 16;
        let mut state: u32 = 0x1234;
        while pos + 5 < data.len() {
            data[pos] = 0xE8; // CALL
            let target = (state % 4096) as i32;
            let bytes = target.to_le_bytes();
            data[pos + 1] = bytes[0];
            data[pos + 2] = bytes[1];
            data[pos + 3] = bytes[2];
            data[pos + 4] = bytes[3];
            state = state.wrapping_mul(1103515245).wrapping_add(12345);
            pos += 5 + (state as usize % 8); // Skip some bytes between calls
            if pos + 5 < data.len() && state % 3 == 0 {
                data[pos] = 0xE9; // JMP
                let target2 = (state as i32) % 2048;
                let bytes2 = target2.to_le_bytes();
                data[pos + 1] = bytes2[0];
                data[pos + 2] = bytes2[1];
                data[pos + 3] = bytes2[2];
                data[pos + 4] = bytes2[3];
                pos += 5;
            }
        }
        let compressed = adaptive_compress(&data);
        let decompressed = adaptive_decompress(&compressed);
        assert_eq!(data, decompressed, "BCJ adaptive roundtrip failed");
    }

    #[test]
    fn test_adaptive_bcj_flag_in_header() {
        // Verify the header format includes the BCJ flag byte
        let data = b"Simple text data for header format test. \
                     Adding enough content to avoid being too short.";
        let compressed = adaptive_compress(data);
        assert_eq!(&compressed[0..4], b"NX12");
        // Byte 8 = codec_id, Byte 9 = bcj_flag
        assert!(compressed.len() >= 10);
        // For text data, BCJ should not be applied
        assert_eq!(compressed[9], 0, "BCJ flag should be 0 for text data");
    }
}
