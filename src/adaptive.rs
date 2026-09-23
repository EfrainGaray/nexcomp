//! Adaptive block selector — chooses the best codec per block with no-regression guarantee.

use crate::classifier_v2::{classify_block_v2, CodecChoice};
use crate::codecs::bcj_filter;
use crate::codecs::bwt_codec;
use crate::codecs::delta_ans;
use crate::codecs::lzma_style;
use crate::codecs::ppm;
use crate::codecs::stride_cm;
use crate::codecs::rle_huffman;
use crate::lz77::{self, huffman, Lz77Encoder};
use rayon::prelude::*;
use thiserror::Error;

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
    StrideCm = 7,
}

/// Errors from parsing or decoding the container.
#[derive(Debug, Error)]
pub enum AdaptiveError {
    #[error("bad magic")]
    BadMagic,
    #[error("truncated container")]
    Truncated,
    #[error("trailing bytes after last block")]
    TrailingBytes,
    #[error("unknown codec id {0}")]
    UnknownCodec(u8),
    #[error("{0} block failed to decode")]
    Decode(&'static str),
    #[error("{0} block decoder panicked on corrupt input")]
    DecoderPanic(&'static str),
    #[error("block checksum mismatch: the data is corrupt")]
    ChecksumMismatch,
    #[error("{0} block declares lengths that do not match the container")]
    LengthMismatch(&'static str),
    #[error("block layout does not match the declared file length")]
    BadLayout,
    #[error("output of {declared} bytes exceeds the limit of {limit}")]
    OutputLimit { declared: usize, limit: usize },
    #[error("size mismatch: expected {expected}, got {got}")]
    SizeMismatch { expected: usize, got: usize },
    #[error("file hash mismatch: the data is corrupt")]
    DigestMismatch,
    #[error("writing the output failed: {0}")]
    Io(String),
}

impl CodecId {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Lz77Huffman),
            1 => Some(Self::LzmaStyle),
            2 => Some(Self::DeltaAns),
            3 => Some(Self::RleHuffman),
            4 => Some(Self::Passthrough),
            5 => Some(Self::BwtRans),
            6 => Some(Self::Ppm),
            7 => Some(Self::StrideCm),
            _ => None,
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
            Self::StrideCm => "stride-cm",
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

fn decompress_baseline(data: &[u8], expected_len: usize) -> Result<Vec<u8>, AdaptiveError> {
    let tokens = huffman::huffman_decode_blocked_checked(data).ok_or(AdaptiveError::Decode("lz77huf"))?;
    lz77::lz77_decode_with_limit(&tokens, expected_len).map_err(|_| AdaptiveError::Decode("lz77huf"))
}

/// Result of adaptive compression, including whether BCJ pre-filter was applied.
struct AdaptiveResult {
    compressed: Vec<u8>,
    codec: CodecId,
    bcj_applied: bool,
}

/// Encode `data` with one codec; `None` if the codec rejects the input.
pub fn encode_with(codec: CodecId, data: &[u8]) -> Option<Vec<u8>> {
    match codec {
        CodecId::Lz77Huffman => Some(compress_baseline(data)),
        CodecId::LzmaStyle => Some(lzma_style::encode_block(data)),
        CodecId::DeltaAns => delta_ans::delta_ans_encode(data).ok(),
        CodecId::RleHuffman => Some(rle_huffman::rle_huffman_encode(data)),
        CodecId::Passthrough => Some(data.to_vec()),
        CodecId::BwtRans => Some(bwt_codec::bwt_compress(data)),
        CodecId::Ppm => Some(ppm::ppm_compress(data)),
        CodecId::StrideCm => Some(stride_cm::encode(data)),
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

    // Stride-aware context mixing wins on numeric, image and table data and
    // never won a text block in the corpora (winners measured at <= 0.79 ASCII).
    if metrics.ascii_ratio <= 0.85 && data.len() >= 256 {
        candidates.push(CodecId::StrideCm);
    }

    // Try PPM for text-heavy blocks where it can beat BWT and LZMA
    if metrics.ascii_ratio > 0.80 && metrics.entropy < 5.5 && data.len() >= 256 {
        candidates.push(CodecId::Ppm);
    }

    // Reducing instead of collecting frees each loser as soon as it loses,
    // rather than holding every candidate until the end. Rayon reduces in
    // order, so a tie still keeps the earlier candidate.
    candidates
        .into_par_iter()
        .filter_map(|codec| encode_candidate(codec, data).map(|out| (out, codec)))
        .reduce_with(|best, other| if other.0.len() < best.0.len() { other } else { best })
        // Storing the block is always available and always correct, so one
        // codec that panics on an input costs ratio, never the compression.
        .unwrap_or_else(|| (data.to_vec(), CodecId::Passthrough))
}

/// One candidate's output, or `None` if the codec rejects the input, produces
/// nothing, or panics on it.
fn encode_candidate(codec: CodecId, data: &[u8]) -> Option<Vec<u8>> {
    let encode = std::panic::AssertUnwindSafe(|| encode_with(codec, data));
    std::panic::catch_unwind(encode).ok().flatten().filter(|out| !out.is_empty())
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

    // Try BCJ pre-filter for binary data that looks like x86 code, in parallel with the plain path
    let (_, metrics) = classify_block_v2(data);
    let try_bcj = metrics.ascii_ratio <= 0.70 && bcj_filter::is_likely_x86(data);
    let ((best_plain, best_plain_codec), bcj) = rayon::join(
        || select_best_codec(data),
        || try_bcj.then(|| select_best_codec(&bcj_filter::bcj_encode(data))),
    );
    if let Some((best_bcj, best_bcj_codec)) = bcj {
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

/// Check every length a codec's payload declares against the block length
/// from the container, before the codec runs: a corrupt block must not make a
/// decoder allocate or loop beyond what the block itself could produce.
fn check_declared_lengths(codec: CodecId, data: &[u8], expected: usize) -> Result<(), AdaptiveError> {
    let u32_at = |pos: usize| -> Result<usize, AdaptiveError> {
        read_le::<4>(data, pos).map(|b| u32::from_le_bytes(b) as usize)
    };
    let consistent = match codec {
        CodecId::Passthrough => data.len() == expected,
        // These payloads start with the block length.
        CodecId::LzmaStyle | CodecId::Ppm | CodecId::BwtRans | CodecId::StrideCm => u32_at(0)? == expected,
        // [magic 4][flags 1][length 4][mode 1], mode 1 then [runs 4]; every run is >= 1 byte.
        CodecId::RleHuffman => {
            u32_at(5)? == expected && (data.get(9) != Some(&1) || u32_at(10)? <= expected)
        }
        // [mode 1][lanes 1][length 4], then per lane [symbols 4][counts 1024][coded 4][coded bytes];
        // lane l holds every lanes-th byte starting at l.
        CodecId::DeltaAns => {
            let lanes = usize::from(*data.get(1).ok_or(AdaptiveError::Truncated)?);
            let mut pos = 6;
            let mut ok = u32_at(2)? == expected && lanes > 0;
            for lane in 0..lanes {
                let symbols = u32_at(pos)?;
                let coded = u32_at(pos + 4 + 1024)?;
                pos += 4 + 1024 + 4;
                ok &= symbols == (expected + lanes - 1 - lane) / lanes;
                if symbols > 0 {
                    pos = pos.checked_add(coded).filter(|&p| p <= data.len()).ok_or(AdaptiveError::Truncated)?;
                }
            }
            ok
        }
        // [tokens 4][blocks 4], then per block [tokens 4][bytes 4][bytes]; every token is >= 1 byte.
        CodecId::Lz77Huffman => {
            let tokens = u32_at(0)?;
            let mut pos = 8;
            let mut sum = 0usize;
            for _ in 0..u32_at(4)? {
                sum = sum.saturating_add(u32_at(pos)?);
                pos = (pos + 8)
                    .checked_add(u32_at(pos + 4)?)
                    .filter(|&p| p <= data.len())
                    .ok_or(AdaptiveError::Truncated)?;
            }
            tokens <= expected && sum == tokens
        }
    };
    if consistent {
        Ok(())
    } else {
        Err(AdaptiveError::LengthMismatch(codec.name()))
    }
}

/// Decompress a block given its codec ID and its length from the container.
pub fn decompress_block_adaptive(codec: CodecId, data: &[u8], expected_len: usize) -> Result<Vec<u8>, AdaptiveError> {
    check_declared_lengths(codec, data, expected_len)?;
    Ok(match codec {
        CodecId::Lz77Huffman => decompress_baseline(data, expected_len)?,
        CodecId::LzmaStyle => {
            lzma_style::decode_block(data).map_err(|_| AdaptiveError::Decode("lzma"))?
        }
        CodecId::DeltaAns => {
            delta_ans::delta_ans_decode(data).map_err(|_| AdaptiveError::Decode("delta"))?
        }
        CodecId::RleHuffman => {
            rle_huffman::rle_huffman_decode_checked(data).map_err(|_| AdaptiveError::Decode("rlehuf"))?
        }
        CodecId::BwtRans => bwt_codec::bwt_decompress(data),
        CodecId::Ppm => ppm::ppm_decompress_checked(data).ok_or(AdaptiveError::Decode("ppm"))?,
        CodecId::StrideCm => stride_cm::decode(data).ok_or(AdaptiveError::Decode("stride-cm"))?,
        CodecId::Passthrough => data.to_vec(),
    })
}

/// Input is split into independent blocks of this size; each picks its own codec.
pub const BLOCK_SIZE: usize = 4 * 1024 * 1024;

/// Magic of the container this build writes: NX13 plus a whole-file hash.
pub const CONTAINER_MAGIC: &[u8; 4] = b"NX14";

/// Containers this build reads, with the length of their footer.
const CONTAINER_FORMATS: [(&[u8; 4], usize); 2] = [(b"NX13", 0), (b"NX14", FILE_DIGEST_LEN)];

/// BLAKE3 of the original bytes, in the NX14 footer.
const FILE_DIGEST_LEN: usize = 32;

/// The container format of `payload`, if this build reads it.
pub fn container_format(payload: &[u8]) -> Option<&'static str> {
    CONTAINER_FORMATS
        .iter()
        .find(|(magic, _)| payload.starts_with(*magic))
        .map(|(magic, _)| std::str::from_utf8(*magic).expect("ascii magic"))
}

const FILE_HEADER_LEN: usize = 16;
const BLOCK_HEADER_LEN: usize = 14;

/// CRC-32 (IEEE 802.3) of a block's original bytes, checked after decoding.
fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        t
    });
    !data.iter().fold(!0u32, |c, &b| table[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8))
}

/// Full adaptive compress: data -> wire format
/// Format: [4B magic "NX14"][8B orig_len LE][4B block_count LE] then per block:
///         [1B codec_id][1B bcj_flag][4B orig_len LE][4B comp_len LE][4B crc32 LE][compressed_data]
/// and a [32B BLAKE3 of the original] footer. Blocks are compressed in parallel.
pub fn adaptive_compress(data: &[u8]) -> Vec<u8> {
    let blocks: Vec<AdaptiveResult> = data
        .par_chunks(BLOCK_SIZE)
        .map(compress_block_adaptive)
        .collect();

    let payload_len: usize = blocks.iter().map(|b| BLOCK_HEADER_LEN + b.compressed.len()).sum();
    let mut out = Vec::with_capacity(FILE_HEADER_LEN + payload_len);
    out.extend_from_slice(CONTAINER_MAGIC);
    out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    out.extend_from_slice(&(blocks.len() as u32).to_le_bytes());
    for (block, chunk) in blocks.iter().zip(data.chunks(BLOCK_SIZE)) {
        out.push(block.codec as u8);
        out.push(if block.bcj_applied { 1 } else { 0 });
        out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(&(block.compressed.len() as u32).to_le_bytes());
        out.extend_from_slice(&crc32(chunk).to_le_bytes());
        out.extend_from_slice(&block.compressed);
    }
    out.extend_from_slice(blake3::hash(data).as_bytes());
    out
}

/// One parsed block of the wire format.
pub struct BlockInfo<'a> {
    pub codec: CodecId,
    pub bcj_applied: bool,
    pub orig_len: usize,
    pub crc: u32,
    pub data: &'a [u8],
}

fn read_le<const N: usize>(payload: &[u8], pos: usize) -> Result<[u8; N], AdaptiveError> {
    let end = pos.checked_add(N).ok_or(AdaptiveError::Truncated)?;
    let bytes = payload.get(pos..end).ok_or(AdaptiveError::Truncated)?;
    Ok(bytes.try_into().unwrap())
}

/// Parse the wire format into (orig_len, blocks) without decompressing.
pub fn parse_blocks(payload: &[u8]) -> Result<(usize, Vec<BlockInfo<'_>>), AdaptiveError> {
    let (orig_len, blocks, _) = parse_container(payload)?;
    Ok((orig_len, blocks))
}

/// Like [`parse_blocks`], and also the whole-file hash an NX14 file carries.
#[allow(clippy::type_complexity)]
fn parse_container(payload: &[u8]) -> Result<(usize, Vec<BlockInfo<'_>>, Option<&[u8]>), AdaptiveError> {
    let &(_, footer_len) = CONTAINER_FORMATS
        .iter()
        .find(|(magic, _)| payload.starts_with(*magic))
        .ok_or(AdaptiveError::BadMagic)?;
    if payload.len() < FILE_HEADER_LEN + footer_len {
        return Err(AdaptiveError::Truncated);
    }
    let orig_len = usize::try_from(u64::from_le_bytes(read_le(payload, 4)?))
        .map_err(|_| AdaptiveError::Truncated)?;
    let block_count = u32::from_le_bytes(read_le(payload, 12)?) as usize;
    // Every block needs at least its header, so a hostile count cannot force a huge reservation.
    if block_count > (payload.len() - FILE_HEADER_LEN - footer_len) / BLOCK_HEADER_LEN {
        return Err(AdaptiveError::Truncated);
    }

    // The writer cuts every block at BLOCK_SIZE; only the last may be shorter.
    if block_count != orig_len.div_ceil(BLOCK_SIZE) {
        return Err(AdaptiveError::BadLayout);
    }

    let mut blocks = Vec::with_capacity(block_count);
    let mut pos = FILE_HEADER_LEN;
    for index in 0..block_count {
        let [codec_id, bcj] = read_le::<2>(payload, pos)?;
        let codec = CodecId::from_u8(codec_id).ok_or(AdaptiveError::UnknownCodec(codec_id))?;
        let block_orig = u32::from_le_bytes(read_le(payload, pos + 2)?) as usize;
        let comp_len = u32::from_le_bytes(read_le(payload, pos + 6)?) as usize;
        let crc = u32::from_le_bytes(read_le(payload, pos + 10)?);
        if block_orig != BLOCK_SIZE.min(orig_len - index * BLOCK_SIZE) {
            return Err(AdaptiveError::BadLayout);
        }
        pos += BLOCK_HEADER_LEN;
        let end = pos.checked_add(comp_len).ok_or(AdaptiveError::Truncated)?;
        let data = payload.get(pos..end).ok_or(AdaptiveError::Truncated)?;
        blocks.push(BlockInfo { codec, bcj_applied: bcj != 0, orig_len: block_orig, crc, data });
        pos = end;
    }
    match (pos + footer_len).cmp(&payload.len()) {
        std::cmp::Ordering::Greater => return Err(AdaptiveError::Truncated),
        std::cmp::Ordering::Less => return Err(AdaptiveError::TrailingBytes),
        std::cmp::Ordering::Equal => {}
    }
    Ok((orig_len, blocks, (footer_len > 0).then(|| &payload[pos..])))
}

/// Codec name if every block agrees, otherwise `Mixed(a+b)` in first-seen order.
pub fn codec_summary(compressed: &[u8]) -> Result<String, AdaptiveError> {
    let (_, blocks) = parse_blocks(compressed)?;
    let mut names: Vec<&'static str> = Vec::new();
    for block in &blocks {
        if !names.contains(&block.codec.name()) {
            names.push(block.codec.name());
        }
    }
    Ok(match names.as_slice() {
        [] => "none".to_string(),
        [single] => single.to_string(),
        _ => format!("Mixed({})", names.join("+")),
    })
}

/// Full adaptive decompress: wire format -> data
pub fn try_adaptive_decompress(payload: &[u8]) -> Result<Vec<u8>, AdaptiveError> {
    try_adaptive_decompress_limited(payload, usize::MAX)
}

/// Decode one block: its codec, the BCJ filter, its length and its checksum.
fn decode_block(block: &BlockInfo<'_>) -> Result<Vec<u8>, AdaptiveError> {
    // Some codec internals still assert on impossible input; a corrupt file
    // must surface as an error, never abort the process.
    let decode = || -> Result<Vec<u8>, AdaptiveError> {
        let decompressed = decompress_block_adaptive(block.codec, block.data, block.orig_len)?;
        Ok(if block.bcj_applied { bcj_filter::bcj_decode(&decompressed) } else { decompressed })
    };
    let decompressed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(decode))
        .map_err(|_| AdaptiveError::DecoderPanic(block.codec.name()))??;
    if decompressed.len() != block.orig_len {
        return Err(AdaptiveError::SizeMismatch { expected: block.orig_len, got: decompressed.len() });
    }
    if crc32(&decompressed) != block.crc {
        return Err(AdaptiveError::ChecksumMismatch);
    }
    Ok(decompressed)
}

/// Decode a container into `out`, block by block, so memory stays bounded by
/// the blocks in flight instead of the whole output. Returns the bytes written.
///
/// The hash of an NX14 file is checked once everything is written, so a caller
/// writing to a file must discard it if this returns an error.
pub fn decompress_to<W: std::io::Write>(payload: &[u8], out: &mut W) -> Result<usize, AdaptiveError> {
    let (orig_len, blocks, digest) = parse_container(payload)?;
    write_blocks(orig_len, &blocks, digest, out)
}

/// The body of [`decompress_to`], on a container someone else parsed.
fn write_blocks<W: std::io::Write>(
    orig_len: usize,
    blocks: &[BlockInfo<'_>],
    digest: Option<&[u8]>,
    out: &mut W,
) -> Result<usize, AdaptiveError> {
    let in_flight = rayon::current_num_threads().max(1);
    let mut hasher = blake3::Hasher::new();
    let mut rest = blocks;
    while !rest.is_empty() {
        let take = group_within_budget(rest, in_flight);
        let (group, tail) = rest.split_at(take);
        rest = tail;
        let decoded: Vec<Result<Vec<u8>, AdaptiveError>> = group.par_iter().map(decode_block).collect();
        for block in decoded {
            let block = block?;
            if digest.is_some() {
                hasher.update(&block);
            }
            out.write_all(&block).map_err(|e| AdaptiveError::Io(e.to_string()))?;
        }
    }
    if digest.is_some_and(|d| hasher.finalize().as_bytes() != d) {
        return Err(AdaptiveError::DigestMismatch);
    }
    Ok(orig_len)
}

/// Working memory a codec needs while decoding, as a multiple of the block it
/// produces. PPM builds a context table per byte seen and is by far the
/// hungriest; the rest stay within a few times their block.
fn decode_cost(block: &BlockInfo<'_>) -> usize {
    let factor = match block.codec {
        CodecId::Ppm => 384,
        CodecId::BwtRans => 24,
        CodecId::StrideCm => 8,
        _ => 4,
    };
    block.orig_len.saturating_mul(factor)
}

/// How many of the next blocks may decode at once: as many as there are
/// threads, but never so many that their working memory passes the budget.
/// Always at least one, so a single expensive block still decodes.
fn group_within_budget(blocks: &[BlockInfo<'_>], in_flight: usize) -> usize {
    let mut total = 0usize;
    for (taken, block) in blocks.iter().enumerate().take(in_flight) {
        total = total.saturating_add(decode_cost(block));
        if taken > 0 && total > DECODE_MEMORY_BUDGET {
            return taken;
        }
    }
    blocks.len().min(in_flight).max(1)
}

/// Working memory the decoder aims to stay under, whatever the thread count.
/// One block may still exceed it on its own; nothing below a format change
/// can bound a codec's own model.
const DECODE_MEMORY_BUDGET: usize = 2 << 30;

/// Like [`try_adaptive_decompress`], but refuses files that declare more than
/// `max_output` bytes before allocating anything for them.
pub fn try_adaptive_decompress_limited(payload: &[u8], max_output: usize) -> Result<Vec<u8>, AdaptiveError> {
    let (orig_len, blocks, digest) = parse_container(payload)?;
    if orig_len > max_output {
        return Err(AdaptiveError::OutputLimit { declared: orig_len, limit: max_output });
    }
    // The header alone can declare an output far larger than the payload could
    // ever produce — 14 bytes of block header stand for 4 MiB — so the buffer
    // grows with what actually decodes instead of with what the file claims.
    let mut out = Vec::with_capacity(orig_len.min(INITIAL_CAPACITY));
    write_blocks(orig_len, &blocks, digest, &mut out)?;
    Ok(out)
}

/// How much output to reserve up front, whatever the container declares.
const INITIAL_CAPACITY: usize = 64 * 1024 * 1024;

/// Panicking convenience wrapper over `try_adaptive_decompress`.
pub fn adaptive_decompress(payload: &[u8]) -> Vec<u8> {
    try_adaptive_decompress(payload).expect("adaptive decompress failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PPM keeps a context table per byte of its block, so decoding several of
    /// them at once is what drives the decoder's peak memory. The budget must
    /// hold them back without ever stalling on a single block.
    #[test]
    fn expensive_blocks_decode_fewer_at_a_time() {
        let block = |codec| BlockInfo { codec, bcj_applied: false, orig_len: BLOCK_SIZE, crc: 0, data: &[] };
        let ppm: Vec<BlockInfo> = (0..8).map(|_| block(CodecId::Ppm)).collect();
        let store: Vec<BlockInfo> = (0..8).map(|_| block(CodecId::Passthrough)).collect();
        assert_eq!(group_within_budget(&ppm, 8), 1, "a 4 MiB PPM block needs ~1.5 GiB");
        assert_eq!(group_within_budget(&store, 8), 8, "stored blocks cost their own size");
        assert_eq!(group_within_budget(&ppm, 1), 1);
        assert_eq!(group_within_budget(&store[..3], 8), 3);
    }

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
        assert_eq!(&compressed[0..4], CONTAINER_MAGIC);
        let (orig_len, blocks) = parse_blocks(&compressed).unwrap();
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
        let (orig_len, blocks) = parse_blocks(&compressed).unwrap();
        assert_eq!(orig_len, data.len());
        assert_eq!(blocks.len(), 2);
        assert_ne!(blocks[0].codec, blocks[1].codec);
        assert_eq!(adaptive_decompress(&compressed), data);
    }

    #[test]
    fn test_corrupt_rle_block_is_an_error_not_a_panic() {
        let mut container = b"NX13".to_vec();
        container.extend_from_slice(&100u64.to_le_bytes());
        container.extend_from_slice(&1u32.to_le_bytes());
        container.push(CodecId::RleHuffman as u8);
        container.push(0);
        container.extend_from_slice(&100u32.to_le_bytes());
        container.extend_from_slice(&4u32.to_le_bytes());
        container.extend_from_slice(&0u32.to_le_bytes());
        container.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

        let result = std::panic::catch_unwind(|| try_adaptive_decompress(&container));
        assert!(matches!(result, Ok(Err(_))), "corrupt RLE block must return Err");
    }

    fn single_block_container(codec: CodecId, orig_len: u32, payload: &[u8]) -> Vec<u8> {
        let mut c = b"NX13".to_vec();
        c.extend_from_slice(&u64::from(orig_len).to_le_bytes());
        c.extend_from_slice(&1u32.to_le_bytes());
        c.push(codec as u8);
        c.push(0);
        c.extend_from_slice(&orig_len.to_le_bytes());
        c.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        c.extend_from_slice(&0u32.to_le_bytes());
        c.extend_from_slice(payload);
        c
    }

    #[test]
    fn test_hostile_rle_code_lengths_are_an_error_not_a_panic() {
        // Mode 0 whose Huffman table only covers the pattern "0": the first 1 bit has no code.
        let mut mode0 = b"RHF1".to_vec();
        mode0.push(0);
        mode0.extend_from_slice(&100u32.to_le_bytes());
        mode0.push(0);
        let mut lengths = [0u8; 128];
        lengths[0] = 0x01;
        mode0.extend_from_slice(&lengths);
        mode0.extend_from_slice(&[0xFF; 16]);

        // Mode 1 with a 255-bit value code, beyond the 15-bit limit the encoder uses.
        let mut mode1 = b"RHF1".to_vec();
        mode1.push(0);
        mode1.extend_from_slice(&100u32.to_le_bytes());
        mode1.push(1);
        mode1.extend_from_slice(&1u32.to_le_bytes());
        mode1.extend_from_slice(&1u16.to_le_bytes());
        mode1.extend_from_slice(&[7, 255]);
        mode1.extend_from_slice(&[0x11; 128]);
        mode1.extend_from_slice(&[0xFF; 16]);

        for payload in [mode0, mode1] {
            let container = single_block_container(CodecId::RleHuffman, 100, &payload);
            let result = std::panic::catch_unwind(|| try_adaptive_decompress(&container));
            assert!(matches!(result, Ok(Err(_))), "hostile RLE payload must return Err");
        }
    }

    #[test]
    fn test_numeric_block_selects_stride_cm() {
        let data: Vec<u8> = (0..20_000u32).flat_map(|i| (5 * i + 1000).to_le_bytes()).collect();
        let compressed = adaptive_compress(&data);
        let (_, blocks) = parse_blocks(&compressed).unwrap();
        assert_eq!(blocks[0].codec, CodecId::StrideCm);
        assert_eq!(adaptive_decompress(&compressed), data);
    }

    #[test]
    fn test_crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn test_corrupt_payload_fails_the_checksum() {
        let data = b"payload that a flipped bit must not silently change. ".repeat(30);
        let good = adaptive_compress(&data);
        let mut detected = 0;
        for pos in FILE_HEADER_LEN + BLOCK_HEADER_LEN..good.len() {
            let mut bad = good.clone();
            bad[pos] ^= 0x10;
            let result = std::panic::catch_unwind(|| try_adaptive_decompress(&bad));
            match result {
                Ok(Ok(out)) => assert_eq!(out, data, "corruption at {pos} returned wrong data"),
                _ => detected += 1,
            }
        }
        assert!(detected > 0);
    }

    #[test]
    fn test_footer_hash_is_checked() {
        let data = b"footer hash test data, compressible enough to take a real codec path";
        let mut file = adaptive_compress(data);
        assert_eq!(try_adaptive_decompress(&file).unwrap(), data);
        let last = file.len() - 1;
        file[last] ^= 1;
        assert!(matches!(try_adaptive_decompress(&file), Err(AdaptiveError::DigestMismatch)));
    }

    #[test]
    fn test_nx13_files_without_a_footer_still_decode() {
        let data = b"NX13 has no whole-file hash; its files must keep decoding";
        let mut file = adaptive_compress(data);
        file.truncate(file.len() - FILE_DIGEST_LEN);
        file[..4].copy_from_slice(b"NX13");
        assert_eq!(container_format(&file), Some("NX13"));
        assert_eq!(try_adaptive_decompress(&file).unwrap(), data);
    }

    #[test]
    fn test_parse_rejects_hostile_headers() {
        let good = adaptive_compress(b"hostile header test data, long enough to compress a bit");

        let mut huge_count = good[..FILE_HEADER_LEN].to_vec();
        huge_count[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(parse_blocks(&huge_count), Err(AdaptiveError::Truncated)));

        let mut bad_codec = good.clone();
        bad_codec[FILE_HEADER_LEN] = 255;
        assert!(matches!(parse_blocks(&bad_codec), Err(AdaptiveError::UnknownCodec(255))));

        let mut trailing = good.clone();
        trailing.push(0);
        assert!(matches!(parse_blocks(&trailing), Err(AdaptiveError::TrailingBytes)));

        for cut in [FILE_HEADER_LEN - 1, FILE_HEADER_LEN + BLOCK_HEADER_LEN - 1, good.len() - 1] {
            assert!(matches!(parse_blocks(&good[..cut]), Err(AdaptiveError::Truncated)));
        }

        let mut wrong_len = good;
        wrong_len[4..12].copy_from_slice(&999_999u64.to_le_bytes());
        assert!(matches!(try_adaptive_decompress(&wrong_len), Err(AdaptiveError::BadLayout)));
    }
}
