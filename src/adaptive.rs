//! Adaptive block selector — chooses the best codec per block with no-regression guarantee.

use crate::classifier_v2::{classify_block_v2, CodecChoice};
use crate::codecs::bcj_filter;
use crate::codecs::bwt_codec;
use crate::codecs::delta_ans;
use crate::codecs::lzma_style;
use crate::codecs::ppm;
use crate::codecs::rle_huffman;
use crate::lz77::{self, huffman, Lz77Encoder};
use rayon::prelude::*;

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

/// Encode `data` with one codec; `None` if the codec rejects the input.
fn encode_with(codec: CodecId, data: &[u8]) -> Option<Vec<u8>> {
    match codec {
        CodecId::Lz77Huffman => Some(compress_baseline(data)),
        CodecId::LzmaStyle => Some(lzma_style::encode_block(data)),
        CodecId::DeltaAns => delta_ans::delta_ans_encode(data).ok(),
        CodecId::RleHuffman => Some(rle_huffman::rle_huffman_encode(data)),
        CodecId::Passthrough => Some(data.to_vec()),
        CodecId::BwtRans => Some(bwt_codec::bwt_compress(data)),
        CodecId::Ppm => Some(ppm::ppm_compress(data)),
    }
}

/// Run the codec selection on `data`, returning the best compressed result.
/// Candidates are encoded in parallel; ties keep the earlier candidate.
fn select_best_codec(data: &[u8]) -> (Vec<u8>, CodecId) {
    let (choice, metrics) = classify_block_v2(data);

    // Baseline first (no-regression guarantee), then LZMA (strong on most data)
    let mut candidates = vec![CodecId::Lz77Huffman];
    match choice {
        CodecChoice::DeltaAns => candidates.push(CodecId::DeltaAns),
        CodecChoice::RleHuffman => candidates.push(CodecId::RleHuffman),
        CodecChoice::Passthrough => candidates.push(CodecId::Passthrough),
        CodecChoice::LzmaStyle | CodecChoice::Lz77Huffman => {}
    }
    candidates.push(CodecId::LzmaStyle);

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
        candidates.push(CodecId::BwtRans);
    }

    // Try PPM for text-heavy blocks where it can beat BWT and LZMA
    if metrics.ascii_ratio > 0.80 && metrics.entropy < 5.5 && data.len() >= 256 {
        candidates.push(CodecId::Ppm);
    }

    let encoded: Vec<Option<Vec<u8>>> = candidates
        .par_iter()
        .map(|&codec| encode_with(codec, data))
        .collect();

    let mut best: Option<(Vec<u8>, CodecId)> = None;
    for (codec, out) in candidates.into_iter().zip(encoded) {
        let Some(out) = out else { continue };
        if out.is_empty() {
            continue;
        }
        if best.as_ref().map_or(true, |(b, _)| out.len() < b.len()) {
            best = Some((out, codec));
        }
    }
    best.expect("baseline always encodes")
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

/// Input is split into independent blocks of this size; each picks its own codec.
pub const BLOCK_SIZE: usize = 4 * 1024 * 1024;

const FILE_HEADER_LEN: usize = 16;
const BLOCK_HEADER_LEN: usize = 10;

/// Full adaptive compress: data -> wire format
/// Format: [4B magic "NX13"][8B orig_len LE][4B block_count LE] then per block:
///         [1B codec_id][1B bcj_flag][4B orig_len LE][4B comp_len LE][compressed_data]
/// Blocks are compressed in parallel.
pub fn adaptive_compress(data: &[u8]) -> Vec<u8> {
    let blocks: Vec<AdaptiveResult> = data
        .par_chunks(BLOCK_SIZE)
        .map(compress_block_adaptive)
        .collect();

    let payload_len: usize = blocks.iter().map(|b| BLOCK_HEADER_LEN + b.compressed.len()).sum();
    let mut out = Vec::with_capacity(FILE_HEADER_LEN + payload_len);
    out.extend_from_slice(b"NX13");
    out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    out.extend_from_slice(&(blocks.len() as u32).to_le_bytes());
    for (block, chunk) in blocks.iter().zip(data.chunks(BLOCK_SIZE)) {
        out.push(block.codec as u8);
        out.push(if block.bcj_applied { 1 } else { 0 });
        out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(&(block.compressed.len() as u32).to_le_bytes());
        out.extend_from_slice(&block.compressed);
    }
    out
}

/// One parsed block of the wire format.
pub struct BlockInfo<'a> {
    pub codec: CodecId,
    pub bcj_applied: bool,
    pub orig_len: usize,
    pub data: &'a [u8],
}

/// Parse the wire format into (orig_len, blocks) without decompressing.
pub fn parse_blocks(payload: &[u8]) -> (usize, Vec<BlockInfo<'_>>) {
    assert!(payload.len() >= FILE_HEADER_LEN, "payload too short");
    assert_eq!(&payload[0..4], b"NX13", "bad magic");
    let orig_len = u64::from_le_bytes(payload[4..12].try_into().unwrap()) as usize;
    let block_count = u32::from_le_bytes(payload[12..16].try_into().unwrap()) as usize;

    let mut blocks = Vec::with_capacity(block_count);
    let mut pos = FILE_HEADER_LEN;
    for _ in 0..block_count {
        assert!(payload.len() >= pos + BLOCK_HEADER_LEN, "truncated block header");
        let codec = CodecId::from_u8(payload[pos]);
        let bcj_applied = payload[pos + 1] != 0;
        let block_orig = u32::from_le_bytes(payload[pos + 2..pos + 6].try_into().unwrap()) as usize;
        let comp_len = u32::from_le_bytes(payload[pos + 6..pos + 10].try_into().unwrap()) as usize;
        pos += BLOCK_HEADER_LEN;
        assert!(payload.len() >= pos + comp_len, "truncated block data");
        blocks.push(BlockInfo { codec, bcj_applied, orig_len: block_orig, data: &payload[pos..pos + comp_len] });
        pos += comp_len;
    }
    (orig_len, blocks)
}

/// Full adaptive decompress: wire format -> data
pub fn adaptive_decompress(payload: &[u8]) -> Vec<u8> {
    let (orig_len, blocks) = parse_blocks(payload);
    let decoded: Vec<Vec<u8>> = blocks
        .par_iter()
        .map(|b| {
            let decompressed = decompress_block_adaptive(b.codec, b.data);
            let decompressed = if b.bcj_applied {
                bcj_filter::bcj_decode(&decompressed)
            } else {
                decompressed
            };
            assert_eq!(decompressed.len(), b.orig_len, "block size mismatch");
            decompressed
        })
        .collect();
    let out = decoded.concat();
    assert_eq!(out.len(), orig_len, "size mismatch");
    out
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
        assert_eq!(&compressed[0..4], b"NX13");
        let (orig_len, blocks) = parse_blocks(&compressed);
        assert_eq!(orig_len, data.len());
        assert_eq!(blocks.len(), 1);
        // For text data, BCJ should not be applied
        assert!(!blocks[0].bcj_applied, "BCJ flag should be 0 for text data");
    }

    #[test]
    fn test_adaptive_multi_block_roundtrip() {
        // Text then noise across a block boundary: each block must pick its own codec.
        let mut data = b"The quick brown fox jumps over the lazy dog. ".repeat(BLOCK_SIZE / 45 + 1);
        data.truncate(BLOCK_SIZE);
        let mut state: u64 = 0xDEAD_BEEF;
        data.extend((0..100_000).map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            (state >> 33) as u8
        }));

        let compressed = adaptive_compress(&data);
        let (orig_len, blocks) = parse_blocks(&compressed);
        assert_eq!(orig_len, data.len());
        assert_eq!(blocks.len(), 2);
        assert_ne!(blocks[0].codec, blocks[1].codec);
        assert_eq!(adaptive_decompress(&compressed), data);
    }
}
