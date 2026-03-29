//! RLE + Huffman codec for binary/low-unique-byte data.
//!
//! Targets files like Calgary "pic" (513216 bytes) and Canterbury "ptt5" which
//! have very few unique byte values and long runs of identical bytes.
//!
//! The encoder tries two strategies and picks the smaller output:
//! - Mode 0: Direct Huffman encoding of raw bytes (best when many unique values
//!   or when runs are short)
//! - Mode 1: RLE + Huffman on both value and length streams. Run lengths are
//!   encoded as (length-1) split into a "length class" (Huffman-coded) plus
//!   optional extra bits, similar to how DEFLATE encodes match lengths.

use crate::lz77::huffman::{
    build_decode_table, build_huffman_codes, BitReader, BitWriter, HuffCode,
};
use thiserror::Error;

const MAGIC: &[u8; 4] = b"RHF1";
const HEADER_LEN: usize = 4 + 1 + 4 + 1;

#[derive(Debug, Error)]
pub enum RleHuffmanError {
    #[error("payload too short")]
    Truncated,
    #[error("invalid codec mode {0}")]
    InvalidMode(u8),
    #[error("invalid palette size")]
    InvalidPalette,
    #[error("decoded length mismatch: expected {expected}, got {got}")]
    LengthMismatch { expected: usize, got: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunAnalysis {
    pub unique_values: usize,
    pub run_count: usize,
    pub max_run_length: usize,
}

pub fn analyze_runs(data: &[u8]) -> RunAnalysis {
    let runs = rle_encode(data);
    let mut seen = [false; 256];
    let mut max_run_length = 0usize;
    for run in &runs {
        seen[run.value as usize] = true;
        max_run_length = max_run_length.max(run.length as usize);
    }
    RunAnalysis {
        unique_values: seen.into_iter().filter(|seen| *seen).count(),
        run_count: runs.len(),
        max_run_length,
    }
}

// ---------------------------------------------------------------------------
// Run-length encoding primitives
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    value: u8,
    length: u32,
}

fn rle_encode(data: &[u8]) -> Vec<Run> {
    let mut runs = Vec::new();
    if data.is_empty() {
        return runs;
    }
    let mut i = 0;
    while i < data.len() {
        let val = data[i];
        let mut run_len: u32 = 1;
        while i + (run_len as usize) < data.len() && data[i + (run_len as usize)] == val {
            run_len += 1;
        }
        runs.push(Run {
            value: val,
            length: run_len,
        });
        i += run_len as usize;
    }
    runs
}

#[allow(dead_code)]
fn rle_decode(runs: &[Run], expected_len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(expected_len);
    for r in runs {
        out.resize(out.len() + r.length as usize, r.value);
    }
    out
}

// ---------------------------------------------------------------------------
// Varint helpers
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn encode_varint(value: u32, out: &mut Vec<u8>) {
    let mut v = value;
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            break;
        } else {
            out.push(byte | 0x80);
        }
    }
}

#[allow(dead_code)]
fn decode_varint(data: &[u8], pos: &mut usize) -> u32 {
    let mut result: u32 = 0;
    let mut shift: u32 = 0;
    loop {
        assert!(*pos < data.len(), "varint decode: unexpected end of data");
        let byte = data[*pos];
        *pos += 1;
        result |= ((byte & 0x7F) as u32) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        assert!(shift < 35, "varint decode: value too large");
    }
    result
}

// ---------------------------------------------------------------------------
// Huffman helpers
// ---------------------------------------------------------------------------

fn rebuild_canonical_codes(code_lengths: &[u8], n: usize) -> Vec<HuffCode> {
    let max_len = code_lengths.iter().copied().max().unwrap_or(0) as usize;
    if max_len == 0 {
        let mut codes = vec![HuffCode::default(); n];
        if n > 0 {
            codes[0] = HuffCode { bits: 0, len: 1 };
        }
        return codes;
    }

    let mut bl_count = vec![0u32; max_len + 1];
    for &l in &code_lengths[..n] {
        if l > 0 {
            bl_count[l as usize] += 1;
        }
    }

    let mut next_code = vec![0u32; max_len + 1];
    let mut code: u32 = 0;
    for bits in 1..=max_len {
        code = (code + bl_count[bits - 1]) << 1;
        next_code[bits] = code;
    }

    let mut codes = vec![HuffCode::default(); n];
    for sym in 0..n {
        let len = code_lengths[sym];
        if len > 0 {
            codes[sym] = HuffCode {
                bits: next_code[len as usize],
                len,
            };
            next_code[len as usize] += 1;
        }
    }

    codes
}

fn pack_code_lengths_256(cl: &[u8]) -> Vec<u8> {
    assert!(cl.len() == 256);
    let mut packed = Vec::with_capacity(128);
    for pair in cl.chunks(2) {
        packed.push(pair[0] | (pair[1] << 4));
    }
    packed
}

fn unpack_code_lengths_256(packed: &[u8]) -> Vec<u8> {
    assert!(packed.len() == 128);
    let mut cl = Vec::with_capacity(256);
    for &byte in packed {
        cl.push(byte & 0x0F);
        cl.push(byte >> 4);
    }
    cl
}

// ---------------------------------------------------------------------------
// Length class encoding (similar to DEFLATE length codes)
//
// We encode (run_length - 1) using a class code + extra bits:
//   Class 0..127:   literal value 0..127, 0 extra bits
//   Class 128:      base 128, 1 extra bit  -> 128..129
//   Class 129:      base 130, 1 extra bit  -> 130..131
//   ...continuing with doubling extra bits at each step...
//
// Simplified scheme: encode into 256 symbols (classes 0-255):
//   Classes 0-63:    literal 0-63, 0 extra bits
//   Classes 64-127:  base = 64 + (c-64)*2, 1 extra bit each (range 64-191)
//   Classes 128-191: base = 192 + (c-128)*4, 2 extra bits each (range 192-447)
//   Classes 192-223: base = 448 + (c-192)*16, 4 extra bits each (range 448-959)
//   Classes 224-239: base = 960 + (c-224)*64, 6 extra bits each (range 960-1983)
//   Classes 240-247: base = 1984 + (c-240)*512, 9 extra bits each (range 1984-5983)
//   Classes 248-251: base = 5984 + (c-248)*4096, 12 extra bits each (range 5984-22367)
//   Classes 252-253: base = 22368 + (c-252)*65536, 16 extra bits each (range 22368-153439)
//   Class 254:       base = 153440, 20 extra bits -> up to 1202015
//   Class 255:       base = 1202016, 20 extra bits -> up to 2250591
// ---------------------------------------------------------------------------

/// Length class table: (first_class_in_group, num_classes, extra_bits, base_value)
const LENGTH_GROUPS: &[(u16, u16, u8, u32)] = &[
    (0, 64, 0, 0),          // classes 0-63: literals 0-63
    (64, 64, 1, 64),         // classes 64-127: 64-191
    (128, 64, 2, 192),       // classes 128-191: 192-447
    (192, 32, 4, 448),       // classes 192-223: 448-959
    (224, 16, 6, 960),       // classes 224-239: 960-1983
    (240, 8, 9, 1984),       // classes 240-247: 1984-5983
    (248, 4, 12, 5984),      // classes 248-251: 5984-22367
    (252, 2, 16, 22368),     // classes 252-253: 22368-153439
    (254, 1, 20, 153440),    // class 254: 153440-1202015
    (255, 1, 20, 1202016),   // class 255: 1202016-2250591
];

const NUM_LENGTH_CLASSES: usize = 256;

/// Encode a run length (minus 1) into (class, extra_bits_value, extra_bits_count).
fn encode_length_class(len_minus_1: u32) -> (u16, u32, u8) {
    let val = len_minus_1;
    for &(first_class, num_classes, extra_bits, base) in LENGTH_GROUPS {
        let step = if extra_bits == 0 { 1 } else { 1u32 << extra_bits };
        let group_range = (num_classes as u32) * step;
        if val >= base && val < base + group_range {
            let offset = val - base;
            let class_offset = offset / step;
            let extra = offset % step;
            return (first_class + class_offset as u16, extra, extra_bits);
        }
    }
    // Fallback: shouldn't happen for reasonable lengths
    panic!("run length too large: {}", len_minus_1 + 1);
}

/// Decode a length class back to (base_value, extra_bits_count).
fn decode_length_class(class: u16) -> (u32, u8) {
    for &(first_class, num_classes, extra_bits, base) in LENGTH_GROUPS {
        let last_class = first_class + num_classes - 1;
        if class >= first_class && class <= last_class {
            let class_offset = (class - first_class) as u32;
            let step = if extra_bits == 0 { 1 } else { 1u32 << extra_bits };
            let base_val = base + class_offset * step;
            return (base_val, extra_bits);
        }
    }
    panic!("invalid length class: {}", class);
}

// ---------------------------------------------------------------------------
// Mode 0: Direct Huffman encoding of raw bytes
// ---------------------------------------------------------------------------

fn encode_mode0(data: &[u8]) -> Vec<u8> {
    let mut freqs = vec![0u32; 256];
    for &b in data {
        freqs[b as usize] += 1;
    }
    let codes = build_huffman_codes(&freqs, 256);

    let mut bw = BitWriter::new();
    for &b in data {
        bw.write_code(&codes[b as usize]);
    }
    let huff_bytes = bw.finish();

    let code_lengths: Vec<u8> = codes.iter().take(256).map(|c| c.len).collect();
    let packed_cl = pack_code_lengths_256(&code_lengths);

    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.push(1);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.push(0); // mode
    out.extend_from_slice(&packed_cl); // 128 bytes
    out.extend_from_slice(&huff_bytes);
    out
}

fn decode_mode0(payload: &[u8]) -> Result<Vec<u8>, RleHuffmanError> {
    if payload.len() < HEADER_LEN + 128 {
        return Err(RleHuffmanError::Truncated);
    }
    let orig_len = u32::from_le_bytes(payload[5..9].try_into().unwrap()) as usize;
    let mut pos = HEADER_LEN;

    let packed_cl = &payload[pos..pos + 128];
    pos += 128;
    let code_lengths = unpack_code_lengths_256(packed_cl);

    let codes = rebuild_canonical_codes(&code_lengths, 256);
    let decode_table = build_decode_table(&codes, 256);

    let huff_data = &payload[pos..];
    let mut br = BitReader::new(huff_data);
    let mut out = Vec::with_capacity(orig_len);
    for _ in 0..orig_len {
        out.push(br.read_huffman(&decode_table) as u8);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Mode 1: RLE + Huffman with length-class encoding
// ---------------------------------------------------------------------------

fn encode_mode1(data: &[u8]) -> Vec<u8> {
    let runs = rle_encode(data);
    let num_runs = runs.len() as u32;

    // Build palette.
    let unique_vals: Vec<u8> = {
        let mut seen = [false; 256];
        for r in &runs {
            seen[r.value as usize] = true;
        }
        (0..=255u8).filter(|&b| seen[b as usize]).collect()
    };
    let num_unique = unique_vals.len();

    let mut val_to_idx = [0u16; 256];
    for (i, &v) in unique_vals.iter().enumerate() {
        val_to_idx[v as usize] = i as u16;
    }

    // Huffman-encode value indices.
    let mut val_freqs = vec![0u32; num_unique];
    for r in &runs {
        val_freqs[val_to_idx[r.value as usize] as usize] += 1;
    }
    let val_codes = build_huffman_codes(&val_freqs, num_unique);
    let val_cl: Vec<u8> = val_codes.iter().take(num_unique).map(|c| c.len).collect();

    // Encode lengths using length-class scheme.
    // First pass: collect classes and count frequencies.
    let mut len_class_freqs = vec![0u32; NUM_LENGTH_CLASSES];
    let mut encoded_lengths: Vec<(u16, u32, u8)> = Vec::with_capacity(runs.len());
    for r in &runs {
        let (class, extra, extra_bits) = encode_length_class(r.length - 1);
        encoded_lengths.push((class, extra, extra_bits));
        len_class_freqs[class as usize] += 1;
    }
    let len_codes = build_huffman_codes(&len_class_freqs, NUM_LENGTH_CLASSES);
    let len_cl: Vec<u8> = len_codes.iter().take(NUM_LENGTH_CLASSES).map(|c| c.len).collect();

    // Interleaved encoding: for each run, write value_code then length_class_code + extra_bits.
    let mut bw = BitWriter::new();
    for (i, r) in runs.iter().enumerate() {
        let vidx = val_to_idx[r.value as usize] as usize;
        bw.write_code(&val_codes[vidx]);
        let (class, extra, extra_bits) = encoded_lengths[i];
        bw.write_code(&len_codes[class as usize]);
        if extra_bits > 0 {
            bw.write_bits(extra, extra_bits);
        }
    }
    let huff_bytes = bw.finish();

    // Assemble.
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.push(1);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.push(1); // mode
    out.extend_from_slice(&num_runs.to_le_bytes());
    out.extend_from_slice(&(num_unique as u16).to_le_bytes());
    out.extend_from_slice(&unique_vals);
    out.extend_from_slice(&val_cl); // num_unique bytes
    // Length class code lengths: 256 entries, nibble-packed
    let len_cl_packed = pack_code_lengths_256(&len_cl);
    out.extend_from_slice(&len_cl_packed); // 128 bytes
    out.extend_from_slice(&huff_bytes);
    out
}

fn decode_mode1(payload: &[u8]) -> Result<Vec<u8>, RleHuffmanError> {
    if payload.len() < HEADER_LEN + 4 + 2 + 128 {
        return Err(RleHuffmanError::Truncated);
    }
    let orig_len = u32::from_le_bytes(payload[5..9].try_into().unwrap()) as usize;
    let mut pos = HEADER_LEN;

    let num_runs = u32::from_le_bytes(payload[pos..pos + 4].try_into().unwrap()) as usize;
    pos += 4;
    let num_unique = u16::from_le_bytes(payload[pos..pos + 2].try_into().unwrap()) as usize;
    pos += 2;
    if num_unique == 0 || num_unique > 256 || payload.len().saturating_sub(pos) < num_unique {
        return Err(RleHuffmanError::InvalidPalette);
    }

    let unique_vals = payload[pos..pos + num_unique].to_vec();
    pos += num_unique;

    if payload.len().saturating_sub(pos) < num_unique + 128 {
        return Err(RleHuffmanError::Truncated);
    }
    let val_cl = &payload[pos..pos + num_unique];
    pos += num_unique;

    let val_codes = rebuild_canonical_codes(val_cl, num_unique);
    let val_dt = build_decode_table(&val_codes, num_unique);

    let len_cl_packed = &payload[pos..pos + 128];
    pos += 128;
    let len_cl = unpack_code_lengths_256(len_cl_packed);
    let len_codes = rebuild_canonical_codes(&len_cl, NUM_LENGTH_CLASSES);
    let len_dt = build_decode_table(&len_codes, NUM_LENGTH_CLASSES);

    let huff_data = &payload[pos..];
    let mut br = BitReader::new(huff_data);

    let mut out = Vec::with_capacity(orig_len);
    for _ in 0..num_runs {
        let vidx = br.read_huffman(&val_dt) as usize;
        let value = unique_vals[vidx];

        let class = br.read_huffman(&len_dt);
        let (base_val, extra_bits) = decode_length_class(class);
        let extra = if extra_bits > 0 {
            br.read_bits(extra_bits)
        } else {
            0
        };
        let length = (base_val + extra + 1) as usize; // +1 because we encoded length-1

        out.resize(out.len() + length, value);
    }
    if out.len() != orig_len {
        return Err(RleHuffmanError::LengthMismatch {
            expected: orig_len,
            got: out.len(),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compress data using the best of direct-Huffman or RLE+Huffman.
pub fn rle_huffman_encode(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        let mut out = Vec::with_capacity(HEADER_LEN);
        out.extend_from_slice(MAGIC);
        out.push(1);
        out.extend_from_slice(&0u32.to_le_bytes());
        out.push(0);
        return out;
    }

    let mode0 = encode_mode0(data);
    let mode1 = encode_mode1(data);

    if mode0.len() <= mode1.len() {
        mode0
    } else {
        mode1
    }
}

/// Decompress data that was compressed with `rle_huffman_encode`.
pub fn rle_huffman_decode_checked(payload: &[u8]) -> Result<Vec<u8>, RleHuffmanError> {
    if payload.len() < HEADER_LEN || &payload[..4] != MAGIC {
        return Err(RleHuffmanError::Truncated);
    }

    let orig_len = u32::from_le_bytes(payload[5..9].try_into().unwrap()) as usize;
    if orig_len == 0 {
        return Ok(Vec::new());
    }

    match payload[9] {
        0 => decode_mode0(payload),
        1 => decode_mode1(payload),
        m => Err(RleHuffmanError::InvalidMode(m)),
    }
}

pub fn rle_huffman_decode(payload: &[u8]) -> Vec<u8> {
    rle_huffman_decode_checked(payload).expect("rle_huffman payload must be valid")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rle_encode_decode_roundtrip() {
        let data = vec![0u8; 100];
        let runs = rle_encode(&data);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0], Run { value: 0, length: 100 });
        let decoded = rle_decode(&runs, 100);
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_rle_encode_decode_mixed() {
        let mut data = Vec::new();
        data.extend(std::iter::repeat(0xAA).take(50));
        data.extend(std::iter::repeat(0xBB).take(30));
        data.push(0xCC);
        data.extend(std::iter::repeat(0xAA).take(20));

        let runs = rle_encode(&data);
        assert_eq!(runs.len(), 4);
        let decoded = rle_decode(&runs, data.len());
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_rle_encode_single_byte() {
        let data = vec![42u8];
        let runs = rle_encode(&data);
        assert_eq!(runs.len(), 1);
        let decoded = rle_decode(&runs, 1);
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_rle_encode_empty() {
        let data: Vec<u8> = Vec::new();
        let runs = rle_encode(&data);
        assert!(runs.is_empty());
        let decoded = rle_decode(&runs, 0);
        assert!(decoded.is_empty());
    }

    #[test]
    fn test_varint_roundtrip_small() {
        for val in [0u32, 1, 42, 127] {
            let mut buf = Vec::new();
            encode_varint(val, &mut buf);
            assert_eq!(buf.len(), 1, "value {} should be 1 byte", val);
            let mut pos = 0;
            let decoded = decode_varint(&buf, &mut pos);
            assert_eq!(decoded, val);
            assert_eq!(pos, buf.len());
        }
    }

    #[test]
    fn test_varint_roundtrip_medium() {
        for val in [128u32, 255, 1000, 16383] {
            let mut buf = Vec::new();
            encode_varint(val, &mut buf);
            assert_eq!(buf.len(), 2, "value {} should be 2 bytes", val);
            let mut pos = 0;
            let decoded = decode_varint(&buf, &mut pos);
            assert_eq!(decoded, val);
        }
    }

    #[test]
    fn test_varint_roundtrip_large() {
        for val in [16384u32, 100_000, 1_000_000, u32::MAX] {
            let mut buf = Vec::new();
            encode_varint(val, &mut buf);
            let mut pos = 0;
            let decoded = decode_varint(&buf, &mut pos);
            assert_eq!(decoded, val, "varint roundtrip failed for {}", val);
            assert_eq!(pos, buf.len());
        }
    }

    #[test]
    fn test_length_class_roundtrip() {
        // Test various length values through the class encoding.
        for len_m1 in [0, 1, 63, 64, 127, 191, 447, 959, 1983, 5983, 22367, 100000] {
            let (class, extra, extra_bits) = encode_length_class(len_m1);
            let (base, eb) = decode_length_class(class);
            assert_eq!(eb, extra_bits);
            assert_eq!(base + extra, len_m1, "length class roundtrip failed for {}", len_m1);
        }
    }

    #[test]
    fn test_full_roundtrip_simple() {
        let data = vec![0u8; 1000];
        let compressed = rle_huffman_encode(&data);
        let decompressed = rle_huffman_decode(&compressed);
        assert_eq!(decompressed, data);
    }

    #[test]
    fn test_full_roundtrip_empty() {
        let data: Vec<u8> = Vec::new();
        let compressed = rle_huffman_encode(&data);
        let decompressed = rle_huffman_decode(&compressed);
        assert_eq!(decompressed, data);
    }

    #[test]
    fn test_full_roundtrip_two_values() {
        let mut data = Vec::new();
        for _ in 0..100 {
            data.extend(std::iter::repeat(0x00).take(50));
            data.extend(std::iter::repeat(0xFF).take(30));
        }
        let compressed = rle_huffman_encode(&data);
        let decompressed = rle_huffman_decode(&compressed);
        assert_eq!(decompressed, data);
        assert!(
            compressed.len() < data.len() / 4,
            "expected good compression, got {}/{} bytes",
            compressed.len(),
            data.len()
        );
    }

    #[test]
    fn test_full_roundtrip_long_runs() {
        let mut data = Vec::new();
        data.extend(std::iter::repeat(0x01).take(100_000));
        data.extend(std::iter::repeat(0x02).take(200_000));
        data.extend(std::iter::repeat(0x03).take(50_000));

        let compressed = rle_huffman_encode(&data);
        let decompressed = rle_huffman_decode(&compressed);
        assert_eq!(decompressed, data);
    }

    #[test]
    fn test_full_roundtrip_no_runs() {
        let data: Vec<u8> = (0..=255).collect();
        let compressed = rle_huffman_encode(&data);
        let decompressed = rle_huffman_decode(&compressed);
        assert_eq!(decompressed, data);
    }

    #[test]
    fn test_full_roundtrip_single_value() {
        let data = vec![0x42u8; 5000];
        let compressed = rle_huffman_encode(&data);
        let decompressed = rle_huffman_decode(&compressed);
        assert_eq!(decompressed, data);
    }

    #[test]
    fn test_full_roundtrip_pic_file() {
        let path = "/tmp/nexcomp_corpora/calgary/pic";
        let Ok(data) = std::fs::read(path) else {
            eprintln!("Skipping pic test: {} not found", path);
            return;
        };

        assert_eq!(data.len(), 513216, "unexpected pic file size");

        let compressed = rle_huffman_encode(&data);
        let decompressed = rle_huffman_decode(&compressed);
        assert_eq!(decompressed, data, "pic roundtrip failed");

        let bpb = compressed.len() as f64 * 8.0 / data.len() as f64;
        eprintln!(
            "pic: {} -> {} bytes ({:.3} bpb)",
            data.len(),
            compressed.len(),
            bpb,
        );

        // The LZ77+Huffman baseline is 0.829 bpb. This RLE+Huffman codec
        // should achieve competitive compression on low-unique-byte data.
        // Direct Huffman (mode 0) achieves ~1 bpb with 2 unique values;
        // RLE+Huffman (mode 1) can do better when runs are long.
        assert!(
            bpb < 1.1,
            "expected < 1.1 bpb on pic, got {:.3}",
            bpb,
        );
    }

    #[test]
    fn test_compression_vs_plain_rle() {
        let mut data = Vec::new();
        for _ in 0..500 {
            data.extend(std::iter::repeat(0x00).take(100));
            data.extend(std::iter::repeat(0x01).take(50));
            data.extend(std::iter::repeat(0x02).take(20));
        }

        let rle_only = crate::codecs::rle_encode(&data);
        let rle_huff = rle_huffman_encode(&data);

        eprintln!(
            "Plain RLE: {} bytes, RLE+Huffman: {} bytes (original: {})",
            rle_only.len(),
            rle_huff.len(),
            data.len(),
        );

        assert!(
            rle_huff.len() <= rle_only.len(),
            "RLE+Huffman ({}) should be <= plain RLE ({})",
            rle_huff.len(),
            rle_only.len(),
        );
    }
}
