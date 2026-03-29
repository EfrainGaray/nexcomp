// NEXCOMP v1.2 — adaptive block selector with no-regression guarantee

use crate::classifier_v2::{classify_block_v2, BlockMetrics, CodecChoice};
use crate::codecs::{delta_ans, lzma_style, rle_huffman};
use crate::lz77::{self, huffman, Lz77Encoder};
use std::panic::{catch_unwind, AssertUnwindSafe};
use thiserror::Error;

pub type BlockCodec = CodecChoice;

#[derive(Debug, Error)]
pub enum SelectorError {
    #[error("delta+ans: {0}")]
    DeltaAns(String),
    #[error("lzma-style: {0}")]
    LzmaStyle(String),
    #[error("rle+huffman: {0}")]
    RleHuffman(String),
    #[error("lz77 decode failed: {0}")]
    Lz77(String),
    #[error("invalid baseline payload")]
    InvalidBaselinePayload,
}

#[derive(Debug, Clone)]
pub struct CompressedBlock {
    pub orig_len: u32,
    pub codec: BlockCodec,
    pub payload: Vec<u8>,
    pub metrics: BlockMetrics,
}

impl CompressedBlock {
    pub fn serialized_len(&self) -> usize {
        4 + 1 + 4 + self.payload.len()
    }
}

pub fn compress_block_adaptive(data: &[u8]) -> Result<CompressedBlock, SelectorError> {
    let (choice, metrics) = classify_block_v2(data);
    let baseline = compress_lz77_huffman(data);
    let mut selected = if data.len() < baseline.len() {
        (BlockCodec::Passthrough, data.to_vec())
    } else {
        (BlockCodec::Lz77Huffman, baseline)
    };

    let candidate = match choice {
        BlockCodec::Lz77Huffman => None,
        BlockCodec::LzmaStyle => guarded_encode_candidate(BlockCodec::LzmaStyle, || {
            Ok(lzma_style::encode_block(data))
        }),
        BlockCodec::DeltaAns => guarded_encode_candidate(BlockCodec::DeltaAns, || {
            delta_ans::delta_ans_encode(data).map_err(|err| err.to_string())
        }),
        BlockCodec::RleHuffman => guarded_encode_candidate(BlockCodec::RleHuffman, || {
            Ok(rle_huffman::rle_huffman_encode(data))
        }),
        BlockCodec::Passthrough => Some((BlockCodec::Passthrough, data.to_vec())),
    };

    if let Some(candidate) = candidate {
        if candidate.1.len() < selected.1.len()
            && validate_candidate_roundtrip(candidate.0, &candidate.1, data)
        {
            selected = candidate;
        }
    }

    Ok(CompressedBlock {
        orig_len: data.len() as u32,
        codec: selected.0,
        payload: selected.1,
        metrics,
    })
}

pub fn decompress_block(codec: BlockCodec, payload: &[u8]) -> Result<Vec<u8>, SelectorError> {
    match codec {
        BlockCodec::Lz77Huffman => catch_unwind(AssertUnwindSafe(|| decompress_lz77_huffman(payload)))
            .unwrap_or_else(|_| Err(SelectorError::Lz77("panic during decode".to_string()))),
        BlockCodec::LzmaStyle => catch_unwind(AssertUnwindSafe(|| lzma_style::decode_block(payload)))
            .map_err(|_| SelectorError::LzmaStyle("panic during decode".to_string()))?
            .map_err(|err| SelectorError::LzmaStyle(format!("{err:?}"))),
        BlockCodec::DeltaAns => catch_unwind(AssertUnwindSafe(|| delta_ans::delta_ans_decode(payload)))
            .map_err(|_| SelectorError::DeltaAns("panic during decode".to_string()))?
            .map_err(|err| SelectorError::DeltaAns(err.to_string())),
        BlockCodec::RleHuffman => catch_unwind(AssertUnwindSafe(|| {
            rle_huffman::rle_huffman_decode_checked(payload)
        }))
        .map_err(|_| SelectorError::RleHuffman("panic during decode".to_string()))?
        .map_err(|err| SelectorError::RleHuffman(err.to_string())),
        BlockCodec::Passthrough => Ok(payload.to_vec()),
    }
}

pub fn compress_lz77_huffman(data: &[u8]) -> Vec<u8> {
    let mut encoder = Lz77Encoder::new();
    let (tokens, _) = encoder.encode(data);

    let mut raw = Vec::with_capacity(1 + data.len());
    raw.push(0);
    raw.extend_from_slice(data);

    let mut candidates = vec![
        (6u8, huffman::huffman_encode(&tokens)),
        (1u8, huffman::huffman_encode_blocked(&tokens, 4096)),
        (2u8, huffman::huffman_encode_blocked(&tokens, 8192)),
        (3u8, huffman::huffman_encode_blocked(&tokens, 16384)),
        (7u8, huffman::huffman_encode_blocked(&tokens, 32768)),
        (8u8, huffman::huffman_encode_blocked(&tokens, 65536)),
        (4u8, huffman::huffman_encode_context1(&tokens, 8192)),
        (5u8, huffman::huffman_encode_split(&tokens)),
    ];
    candidates.sort_by_key(|(_, bytes)| bytes.len());

    let mut best = raw;
    for (tag, encoded) in candidates {
        if 1 + encoded.len() >= best.len() {
            continue;
        }
        if validate_baseline_candidate(tag, &encoded, data) {
            let mut out = Vec::with_capacity(1 + encoded.len());
            out.push(tag);
            out.extend_from_slice(&encoded);
            best = out;
        }
    }

    best
}

pub fn decompress_lz77_huffman(payload: &[u8]) -> Result<Vec<u8>, SelectorError> {
    if payload.is_empty() {
        return Err(SelectorError::InvalidBaselinePayload);
    }

    if payload[0] == 0 {
        return Ok(payload[1..].to_vec());
    }

    let huff_payload = &payload[1..];
    let tokens = match payload[0] {
        6 => huffman::huffman_decode(huff_payload),
        4 => huffman::huffman_decode_context1(huff_payload),
        5 => huffman::huffman_decode_split(huff_payload),
        1 | 2 | 3 | 7 | 8 => huffman::huffman_decode_blocked(huff_payload),
        _ => return Err(SelectorError::InvalidBaselinePayload),
    };
    lz77::lz77_decode(&tokens).map_err(|err| SelectorError::Lz77(err.to_string()))
}

fn validate_baseline_candidate(tag: u8, encoded: &[u8], expected: &[u8]) -> bool {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let mut payload = Vec::with_capacity(1 + encoded.len());
        payload.push(tag);
        payload.extend_from_slice(encoded);
        decompress_lz77_huffman(&payload)
    }));

    match result {
        Ok(Ok(decoded)) => decoded == expected,
        _ => false,
    }
}

fn guarded_encode_candidate<F>(
    codec: BlockCodec,
    encode: F,
) -> Option<(BlockCodec, Vec<u8>)>
where
    F: FnOnce() -> Result<Vec<u8>, String>,
{
    match catch_unwind(AssertUnwindSafe(encode)) {
        Ok(Ok(payload)) => Some((codec, payload)),
        _ => None,
    }
}

fn validate_candidate_roundtrip(codec: BlockCodec, payload: &[u8], expected: &[u8]) -> bool {
    match decompress_block(codec, payload) {
        Ok(decoded) => decoded == expected,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_selector_falls_back_to_baseline() {
        let data = b"The quick brown fox jumps over the lazy dog. ".repeat(128);
        let block = compress_block_adaptive(&data).expect("compress ok");
        let baseline = compress_lz77_huffman(&data);
        assert!(block.payload.len() <= baseline.len());
    }

    #[test]
    fn baseline_roundtrip() {
        let data = b"abracadabra abracadabra abracadabra".repeat(64);
        let compressed = compress_lz77_huffman(&data);
        let decompressed = decompress_lz77_huffman(&compressed).expect("decode ok");
        assert_eq!(decompressed, data);
    }

    #[test]
    fn passthrough_roundtrip() {
        let data: Vec<u8> = (0..4096u64)
            .scan(0xCAFE_BABEu64, |state, _| {
                *state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1);
                Some((*state >> 24) as u8)
            })
            .collect();
        let block = compress_block_adaptive(&data).expect("compress ok");
        let decoded = decompress_block(block.codec, &block.payload).expect("decode ok");
        assert_eq!(decoded, data);
    }

    #[test]
    fn delta_candidate_roundtrip() {
        let data = (0..8192u32)
            .scan(0u8, |value, i| {
                *value = value.wrapping_add((i % 3) as u8);
                Some(*value)
            })
            .collect::<Vec<_>>();
        let payload = delta_ans::delta_ans_encode(&data).expect("encode ok");
        let decoded = decompress_block(BlockCodec::DeltaAns, &payload).expect("decode ok");
        assert_eq!(decoded, data);
    }
}
