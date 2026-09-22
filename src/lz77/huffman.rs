// LZ77 Huffman encoding — DEFLATE-style dual-tree token coding
//
// Tree 1 (litlen): 286 symbols — literals 0-255, end-of-block 256, lengths 257-285
// Tree 2 (dist): 48 symbols — offset codes 0-43 + 4 rep-match codes (44-47)
//
// Each match token emits: litlen_code(length) + extra_bits(length) + dist_code(offset) + extra_bits(offset)
// Each literal emits: litlen_code(byte_value)
//
// Based on RFC 1951 tables, extended for 4MB window.

use std::collections::BinaryHeap;
use std::cmp::Reverse;

/// Number of litlen symbols: 0-255 literals + 256 EOB + 257-285 lengths = 286
pub const LITLEN_SYMBOLS: usize = 286;
/// Number of distance symbols: 44 normal codes (0-43) + 4 rep-match codes (44-47).
/// Standard DEFLATE uses 30 (codes 0-29) for 32KB window.
/// Each pair of codes adds 1 extra bit, doubling the range.
/// Codes 0-43 cover offsets up to 4,194,304.
/// Codes 44, 45, 46, 47 = repeat offset 0, 1, 2, 3 from MRU cache.
pub const DIST_SYMBOLS: usize = 48;

/// Distance code for "repeat offset 0" (most recent).
pub const REP_OFFSET_0: u16 = 44;
/// Distance code for "repeat offset 1".
pub const REP_OFFSET_1: u16 = 45;
/// Distance code for "repeat offset 2".
pub const REP_OFFSET_2: u16 = 46;
/// Distance code for "repeat offset 3".
pub const REP_OFFSET_3: u16 = 47;

/// Number of normal (non-rep) distance symbols.
pub const NORMAL_DIST_SYMBOLS: usize = 44;

/// MRU cache of 4 most recent offsets for repeated offset encoding.
/// Initialized to [1, 2, 3, 4] (common small offsets).
#[derive(Clone, Debug)]
pub struct MruCache {
    pub recent: [u32; 4],
}

impl MruCache {
    pub fn new() -> Self {
        Self { recent: [1, 2, 3, 4] }
    }

    /// Check if `offset` matches one of the cached entries.
    /// Returns Some(index) if found.
    #[inline]
    pub fn find(&self, offset: u32) -> Option<usize> {
        if offset == self.recent[0] {
            Some(0)
        } else if offset == self.recent[1] {
            Some(1)
        } else if offset == self.recent[2] {
            Some(2)
        } else if offset == self.recent[3] {
            Some(3)
        } else {
            None
        }
    }

    /// Update the cache: move the entry at `idx` to position 0,
    /// shifting others down.
    #[inline]
    pub fn promote(&mut self, idx: usize) {
        match idx {
            0 => {} // already at front
            1 => {
                self.recent.swap(0, 1);
            }
            2 => {
                let tmp = self.recent[2];
                self.recent[2] = self.recent[1];
                self.recent[1] = self.recent[0];
                self.recent[0] = tmp;
            }
            3 => {
                let tmp = self.recent[3];
                self.recent[3] = self.recent[2];
                self.recent[2] = self.recent[1];
                self.recent[1] = self.recent[0];
                self.recent[0] = tmp;
            }
            _ => {}
        }
    }

    /// Insert a new offset at position 0, shifting existing entries down.
    #[inline]
    pub fn insert(&mut self, offset: u32) {
        self.recent[3] = self.recent[2];
        self.recent[2] = self.recent[1];
        self.recent[1] = self.recent[0];
        self.recent[0] = offset;
    }
}

pub const EOB_SYMBOL: u16 = 256;

// ────────────────────────────────────────────
// DEFLATE length code table (RFC 1951 §3.2.5)
// ────────────────────────────────────────────

/// (base_length, extra_bits) for length codes 257-285
pub const LENGTH_TABLE: [(u16, u8); 29] = [
    (3, 0), (4, 0), (5, 0), (6, 0), (7, 0), (8, 0), (9, 0), (10, 0),    // 257-264
    (11, 1), (13, 1), (15, 1), (17, 1),                                    // 265-268
    (19, 2), (23, 2), (27, 2), (31, 2),                                    // 269-272
    (35, 3), (43, 3), (51, 3), (59, 3),                                    // 273-276
    (67, 4), (83, 4), (99, 4), (115, 4),                                   // 277-280
    (131, 5), (163, 5), (195, 5), (227, 5),                                // 281-284
    (258, 0),                                                               // 285
];

/// Get the length code (257-285) and extra bits for a match length (3-258).
pub fn encode_length(length: u16) -> (u16, u8, u16) {
    // Returns (code, extra_bits_count, extra_bits_value)
    for (i, &(base, extra)) in LENGTH_TABLE.iter().enumerate() {
        let code = 257 + i as u16;
        if extra == 0 {
            if length == base {
                return (code, 0, 0);
            }
        } else {
            let range = 1u16 << extra;
            if length >= base && length < base + range {
                return (code, extra, length - base);
            }
        }
    }
    // Fallback for length 258
    (285, 0, 0)
}

/// Decode a length code back to match length.
pub fn decode_length(code: u16, extra_val: u16) -> u16 {
    let idx = (code - 257) as usize;
    let (base, _) = LENGTH_TABLE[idx];
    base + extra_val
}

// ────────────────────────────────────────────
// Extended offset code table (4MB window)
// ────────────────────────────────────────────

/// (base_offset, extra_bits) for distance codes 0-37
/// Codes 0-29 match DEFLATE RFC 1951 §3.2.5.
/// Codes 30-37 extend to cover up to 4MB.
pub const DIST_TABLE: [(u32, u8); 44] = [
    (1, 0), (2, 0), (3, 0), (4, 0),                                        // 0-3
    (5, 1), (7, 1),                                                         // 4-5
    (9, 2), (13, 2),                                                        // 6-7
    (17, 3), (25, 3),                                                       // 8-9
    (33, 4), (49, 4),                                                       // 10-11
    (65, 5), (97, 5),                                                       // 12-13
    (129, 6), (193, 6),                                                     // 14-15
    (257, 7), (385, 7),                                                     // 16-17
    (513, 8), (769, 8),                                                     // 18-19
    (1025, 9), (1537, 9),                                                   // 20-21
    (2049, 10), (3073, 10),                                                 // 22-23
    (4097, 11), (6145, 11),                                                 // 24-25
    (8193, 12), (12289, 12),                                                // 26-27
    (16385, 13), (24577, 13),                                               // 28-29 (DEFLATE ends here)
    // Extended for 4MB window:
    (32769, 14), (49153, 14),                                               // 30-31
    (65537, 15), (98305, 15),                                               // 32-33
    (131073, 16), (196609, 16),                                             // 34-35
    (262145, 17), (393217, 17),                                             // 36-37
    (524289, 18), (786433, 18),                                             // 38-39
    (1048577, 19), (1572865, 19),                                           // 40-41
    (2097153, 20), (3145729, 20),                                           // 42-43: covers up to 4194304
];

/// Get the distance code and extra bits for an offset (1-4194304).
pub fn encode_offset(offset: u32) -> (u16, u8, u32) {
    // Returns (code, extra_bits_count, extra_bits_value)
    // Search backwards to find the right code
    for i in (0..DIST_TABLE.len()).rev() {
        let (base, extra) = DIST_TABLE[i];
        if offset >= base {
            let range = if extra > 0 { 1u32 << extra } else { 1 };
            if offset < base + range {
                return (i as u16, extra, offset - base);
            }
        }
    }
    // Should not reach here with the extended table covering 4MB
    // Fallback: use highest code with max extra bits
    (43, 20, offset - 3145729)
}

/// Decode a distance code back to offset.
pub fn decode_offset(code: u16, extra_val: u32) -> u32 {
    if code < DIST_TABLE.len() as u16 {
        let (base, _) = DIST_TABLE[code as usize];
        base + extra_val
    } else {
        extra_val + 1 // fallback
    }
}

// ────────────────────────────────────────────
// Huffman tree construction
// ────────────────────────────────────────────

/// A Huffman code: (bit_pattern, bit_length)
#[derive(Clone, Copy, Debug, Default)]
pub struct HuffCode {
    pub bits: u32,
    pub len: u8,
}

/// Build canonical Huffman codes from symbol frequencies.
/// Returns a code table indexed by symbol.
///
/// Algorithm:
/// 1. Build Huffman tree using a min-heap of (freq, symbol).
/// 2. Extract code lengths per symbol.
/// 3. Convert to canonical Huffman codes (sorted by length, then symbol).
pub fn build_huffman_codes(freqs: &[u32], max_symbols: usize) -> Vec<HuffCode> {
    let n = freqs.len().min(max_symbols);
    if n == 0 {
        return vec![HuffCode::default(); max_symbols];
    }

    // Count nonzero frequencies
    let nonzero: Vec<usize> = (0..n).filter(|&i| freqs[i] > 0).collect();
    if nonzero.is_empty() {
        return vec![HuffCode::default(); max_symbols];
    }
    if nonzero.len() == 1 {
        // Single symbol: assign code 0 with length 1
        let mut codes = vec![HuffCode::default(); max_symbols];
        codes[nonzero[0]] = HuffCode { bits: 0, len: 1 };
        return codes;
    }

    // Step 1: Build tree using min-heap
    // Each node is (frequency, node_id). Leaf nodes have id < n, internal nodes >= n.
    let mut heap: BinaryHeap<Reverse<(u64, usize)>> = BinaryHeap::new();
    let mut parent = vec![usize::MAX; 2 * n]; // parent[node] = parent node
    let mut next_internal = n;

    for &sym in &nonzero {
        heap.push(Reverse((freqs[sym] as u64, sym)));
    }

    while heap.len() > 1 {
        let Reverse((f1, n1)) = heap.pop().expect("heap not empty");
        let Reverse((f2, n2)) = heap.pop().expect("heap has 2+");
        let internal = next_internal;
        next_internal += 1;
        parent[n1] = internal;
        parent[n2] = internal;
        heap.push(Reverse((f1 + f2, internal)));
    }

    // Step 2: Compute code lengths by walking up to root
    let mut lengths = vec![0u8; n];
    for &sym in &nonzero {
        let mut depth = 0u8;
        let mut node = sym;
        while parent[node] != usize::MAX {
            depth += 1;
            node = parent[node];
        }
        lengths[sym] = depth;
    }

    // Cap at 15 bits and fix Kraft inequality (DEFLATE-style)
    limit_code_lengths(&mut lengths, &nonzero, 15);

    // Step 3: Canonical Huffman codes from lengths
    canonical_codes(&lengths, max_symbols)
}

/// Limit code lengths to max_bits while preserving optimal-ish assignment.
/// Uses the approach from RFC 1951 / zlib: cap overlong codes, then redistribute
/// the "overflow" by shortening some codes.
fn limit_code_lengths(lengths: &mut [u8], nonzero: &[usize], max_bits: u8) {
    // Check if any exceed max_bits
    let any_over = nonzero.iter().any(|&s| lengths[s] > max_bits);
    if !any_over {
        return;
    }

    // Count code lengths
    let mut bl_count = vec![0i32; max_bits as usize + 2];
    let mut overflow = 0i32;
    for &s in nonzero {
        if lengths[s] > max_bits {
            overflow += 1;
            lengths[s] = max_bits;
        }
        bl_count[lengths[s] as usize] += 1;
    }

    // Redistribute: move codes from max_bits down to shorter lengths
    // For each overflow item, we need to split a shorter code into two longer ones
    while overflow > 0 {
        // Find the deepest level below max_bits that has codes
        let mut bits = max_bits as usize - 1;
        while bits > 0 && bl_count[bits] == 0 {
            bits -= 1;
        }
        if bits == 0 {
            break; // can't fix further
        }
        // Move one code from `bits` to `bits+1` (split into 2 codes at next level)
        bl_count[bits] -= 1;
        bl_count[bits + 1] += 2;
        bl_count[max_bits as usize] -= 1;
        overflow -= 1;
    }

    // Reassign lengths based on new bl_count
    // Sort nonzero symbols by their original length (longest first for priority)
    let mut sorted: Vec<usize> = nonzero.to_vec();
    sorted.sort_unstable_by(|&a, &b| lengths[b].cmp(&lengths[a]).then(a.cmp(&b)));

    for &sym in &sorted {
        // Find the next available slot from the longest available length
        for bits in (1..=max_bits as usize).rev() {
            if bl_count[bits] > 0 {
                lengths[sym] = bits as u8;
                bl_count[bits] -= 1;
                break;
            }
        }
    }
}

#[allow(dead_code)]
/// Old fix — kept for reference
fn fix_code_lengths(lengths: &mut [u8], nonzero: &[usize], max_bits: u8) {
    // Check if any exceed max_bits
    let over: Vec<usize> = nonzero.iter().filter(|&&s| lengths[s] > max_bits).copied().collect();
    if over.is_empty() {
        return;
    }
    // Simple fix: set all over-length to max_bits, then adjust others
    for &s in &over {
        lengths[s] = max_bits;
    }
    // Verify Kraft inequality: sum(2^-len) <= 1
    // If violated, shorten some codes
    loop {
        let kraft: f64 = nonzero.iter().map(|&s| {
            if lengths[s] > 0 { (0.5f64).powi(lengths[s] as i32) } else { 0.0 }
        }).sum();
        if kraft <= 1.0001 {
            break;
        }
        // Find the longest code and shorten it by 1
        if let Some(&s) = nonzero.iter().max_by_key(|&&s| lengths[s]) {
            if lengths[s] > 1 {
                lengths[s] -= 1;
            } else {
                break;
            }
        } else {
            break;
        }
    }
}

/// Generate canonical Huffman codes from code lengths.
pub fn canonical_codes(lengths: &[u8], max_symbols: usize) -> Vec<HuffCode> {
    let n = lengths.len();
    let mut codes = vec![HuffCode::default(); max_symbols];

    // Count codes of each length
    let max_len = *lengths.iter().max().unwrap_or(&0) as usize;
    if max_len == 0 {
        return codes;
    }
    let mut bl_count = vec![0u32; max_len + 1];
    for &l in lengths {
        if l > 0 {
            bl_count[l as usize] += 1;
        }
    }

    // Compute starting code for each length (RFC 1951 algorithm)
    let mut next_code = vec![0u32; max_len + 1];
    let mut code = 0u32;
    for bits in 1..=max_len {
        code = (code + bl_count[bits - 1]) << 1;
        next_code[bits] = code;
    }

    // Assign codes to symbols
    for sym in 0..n {
        let len = lengths[sym];
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

// ────────────────────────────────────────────
// Bit-level I/O
// ────────────────────────────────────────────

pub struct BitWriter {
    buffer: Vec<u8>,
    current: u32,
    bits_in: u8,
}

impl BitWriter {
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            current: 0,
            bits_in: 0,
        }
    }

    /// Write `count` bits from the low bits of `value`.
    #[inline]
    pub fn write_bits(&mut self, value: u32, count: u8) {
        self.current |= value << self.bits_in;
        self.bits_in += count;
        while self.bits_in >= 8 {
            self.buffer.push(self.current as u8);
            self.current >>= 8;
            self.bits_in -= 8;
        }
    }

    /// Write a Huffman code (bits are stored MSB-first in the code,
    /// but we emit LSB-first into the bitstream, matching DEFLATE).
    #[inline]
    pub fn write_code(&mut self, code: &HuffCode) {
        // Reverse the bits to go from MSB-first canonical to LSB-first stream
        let reversed = reverse_bits(code.bits, code.len);
        self.write_bits(reversed, code.len);
    }

    pub fn finish(mut self) -> Vec<u8> {
        if self.bits_in > 0 {
            self.buffer.push(self.current as u8);
        }
        self.buffer
    }

    pub fn bit_count(&self) -> usize {
        self.buffer.len() * 8 + self.bits_in as usize
    }
}

pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    current: u32,
    bits_in: u8,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            current: 0,
            bits_in: 0,
        }
    }

    #[inline]
    fn ensure_bits(&mut self, need: u8) {
        while self.bits_in < need && self.bits_in < 25 && self.pos < self.data.len() {
            self.current |= (self.data[self.pos] as u32) << self.bits_in;
            self.pos += 1;
            self.bits_in += 8;
        }
    }

    /// Read `count` bits, returning the value in the low bits.
    #[inline]
    pub fn read_bits(&mut self, count: u8) -> u32 {
        self.ensure_bits(count);
        let val = self.current & ((1 << count) - 1);
        self.current >>= count;
        self.bits_in -= count;
        val
    }

    /// Decode one symbol using a Huffman decode table.
    pub fn read_huffman(&mut self, decode_table: &HuffDecodeTable) -> u16 {
        self.read_huffman_checked(decode_table)
            .unwrap_or_else(|| panic!("Huffman decode: bit pattern not covered, max_len={}", decode_table.max_len))
    }

    /// Like `read_huffman`, but `None` for a bit pattern the table does not
    /// cover, which only happens with corrupt code lengths.
    pub fn read_huffman_checked(&mut self, decode_table: &HuffDecodeTable) -> Option<u16> {
        self.ensure_bits(decode_table.max_len);
        let peek = self.current & ((1u32 << decode_table.max_len) - 1);
        let entry = decode_table.lookup[peek as usize];
        let len = entry >> 16;
        if len == 0 {
            return None;
        }
        let sym = entry & 0xFFFF;
        self.current >>= len;
        self.bits_in -= len as u8;
        Some(sym as u16)
    }
}

/// Reverse `count` bits of `value`.
fn reverse_bits(value: u32, count: u8) -> u32 {
    let mut result = 0u32;
    let mut v = value;
    for _ in 0..count {
        result = (result << 1) | (v & 1);
        v >>= 1;
    }
    result
}

// ────────────────────────────────────────────
// Huffman decode table (lookup-based)
// ────────────────────────────────────────────

pub struct HuffDecodeTable {
    /// lookup[bit_pattern] = (code_length << 16) | symbol
    pub lookup: Vec<u32>,
    pub max_len: u8,
}

/// Build a decode lookup table from Huffman codes.
pub fn build_decode_table(codes: &[HuffCode], n_symbols: usize) -> HuffDecodeTable {
    let max_len = codes.iter().take(n_symbols).map(|c| c.len).max().unwrap_or(1).max(1);
    let table_size = 1usize << max_len;
    let mut lookup = vec![0u32; table_size];

    for sym in 0..n_symbols {
        let code = &codes[sym];
        if code.len == 0 {
            continue;
        }
        let reversed = reverse_bits(code.bits, code.len);
        // Fill all entries that start with this code pattern
        let fill_count = 1 << (max_len - code.len);
        for j in 0..fill_count {
            let idx = reversed | (j << code.len);
            lookup[idx as usize] = ((code.len as u32) << 16) | sym as u32;
        }
    }

    HuffDecodeTable { lookup, max_len }
}

// ────────────────────────────────────────────
// Token encoding/decoding
// ────────────────────────────────────────────

use super::types::Token;

/// Encode LZ77 tokens using dual Huffman trees with repeated offset encoding.
/// Returns: [4B n_tokens LE] + compressed bitstream.
///
/// Distance codes 44, 45, 46, 47 signal rep-match offsets from a 4-entry MRU cache,
/// saving bits on matches that reuse recent distances.
pub fn huffman_encode(tokens: &[Token]) -> Vec<u8> {
    // Step 1: Count frequencies (with MRU cache to determine rep-match usage)
    let mut litlen_freq = vec![0u32; LITLEN_SYMBOLS];
    let mut dist_freq = vec![0u32; DIST_SYMBOLS];

    {
        let mut mru = MruCache::new();
        for token in tokens {
            match token {
                Token::Literal(b) => {
                    litlen_freq[*b as usize] += 1;
                }
                Token::Match { offset, length } => {
                    let (len_code, _, _) = encode_length(*length);
                    litlen_freq[len_code as usize] += 1;
                    if let Some(idx) = mru.find(*offset) {
                        dist_freq[(REP_OFFSET_0 as usize) + idx] += 1;
                        mru.promote(idx);
                    } else {
                        let (dist_code, _, _) = encode_offset(*offset);
                        dist_freq[dist_code as usize] += 1;
                        mru.insert(*offset);
                    }
                }
            }
        }
    }
    // Don't include EOB in frequencies — we use explicit token count instead

    // Step 2: Build Huffman codes
    let litlen_codes = build_huffman_codes(&litlen_freq, LITLEN_SYMBOLS);
    let dist_codes = build_huffman_codes(&dist_freq, DIST_SYMBOLS);

    // Step 3: Serialize: [4B n_tokens] + header (code lengths) + encoded tokens
    let mut prefix = Vec::new();
    prefix.extend_from_slice(&(tokens.len() as u32).to_le_bytes());

    let mut writer = BitWriter::new();

    // Header: write code lengths for litlen tree (286 entries, each 4 bits = 0-15)
    for i in 0..LITLEN_SYMBOLS {
        writer.write_bits(litlen_codes[i].len as u32, 4);
    }
    // Header: write code lengths for dist tree (48 entries, each 4 bits)
    for i in 0..DIST_SYMBOLS {
        writer.write_bits(dist_codes[i].len as u32, 4);
    }

    // Step 4: Encode tokens (with MRU cache for rep-match encoding)
    let mut mru = MruCache::new();
    for token in tokens {
        match token {
            Token::Literal(b) => {
                writer.write_code(&litlen_codes[*b as usize]);
            }
            Token::Match { offset, length } => {
                let (len_code, len_extra_bits, len_extra_val) = encode_length(*length);
                writer.write_code(&litlen_codes[len_code as usize]);
                if len_extra_bits > 0 {
                    writer.write_bits(len_extra_val as u32, len_extra_bits);
                }
                if let Some(idx) = mru.find(*offset) {
                    // Rep-match: emit special distance code, 0 extra bits
                    let rep_code = (REP_OFFSET_0 as usize) + idx;
                    writer.write_code(&dist_codes[rep_code]);
                    mru.promote(idx);
                } else {
                    let (dist_code, dist_extra_bits, dist_extra_val) = encode_offset(*offset);
                    writer.write_code(&dist_codes[dist_code as usize]);
                    if dist_extra_bits > 0 {
                        writer.write_bits(dist_extra_val, dist_extra_bits);
                    }
                    mru.insert(*offset);
                }
            }
        }
    }

    // No EOB needed — we store token count explicitly
    let mut result = prefix;
    result.extend_from_slice(&writer.finish());
    result
}

/// Decode Huffman-encoded LZ77 tokens.
/// Input format: [4B n_tokens LE] + bitstream with code-length headers.
pub fn huffman_decode(data: &[u8]) -> Vec<Token> {
    let n_tokens = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let mut reader = BitReader::new(&data[4..]);

    // Read litlen code lengths (286 × 4 bits)
    let mut litlen_lengths = vec![0u8; LITLEN_SYMBOLS];
    for i in 0..LITLEN_SYMBOLS {
        litlen_lengths[i] = reader.read_bits(4) as u8;
    }

    // Read dist code lengths (48 × 4 bits)
    let mut dist_lengths = vec![0u8; DIST_SYMBOLS];
    for i in 0..DIST_SYMBOLS {
        dist_lengths[i] = reader.read_bits(4) as u8;
    }

    // Rebuild codes from lengths
    let litlen_codes = canonical_codes(&litlen_lengths, LITLEN_SYMBOLS);
    let dist_codes = canonical_codes(&dist_lengths, DIST_SYMBOLS);

    // Build decode tables
    let litlen_table = build_decode_table(&litlen_codes, LITLEN_SYMBOLS);
    let dist_table = build_decode_table(&dist_codes, DIST_SYMBOLS);

    // Decode exactly n_tokens tokens (count stored in header)
    let mut tokens = Vec::with_capacity(n_tokens);
    let mut mru = MruCache::new();
    for _ in 0..n_tokens {
        let sym = reader.read_huffman(&litlen_table);
        if sym < 256 {
            tokens.push(Token::Literal(sym as u8));
        } else {
            // Length code
            let (base_len, extra_bits) = LENGTH_TABLE[(sym - 257) as usize];
            let extra_val = if extra_bits > 0 { reader.read_bits(extra_bits) as u16 } else { 0 };
            let length = base_len + extra_val;

            // Distance code
            let dist_sym = reader.read_huffman(&dist_table);
            let offset = if dist_sym >= REP_OFFSET_0 {
                // Rep-match: look up offset from MRU cache
                let idx = (dist_sym - REP_OFFSET_0) as usize;
                let off = mru.recent[idx];
                mru.promote(idx);
                off
            } else {
                let (base_dist, dist_extra) = if (dist_sym as usize) < DIST_TABLE.len() {
                    DIST_TABLE[dist_sym as usize]
                } else {
                    (1, 22) // fallback
                };
                let dist_extra_val = if dist_extra > 0 { reader.read_bits(dist_extra) } else { 0 };
                let off = base_dist + dist_extra_val;
                mru.insert(off);
                off
            };

            tokens.push(Token::Match {
                offset,
                length,
            });
        }
    }

    tokens
}

// ────────────────────────────────────────────
// Per-block Huffman encoding (DEFLATE-style)
// ────────────────────────────────────────────

/// Helper: encode a single block of tokens into a BitWriter, including the
/// code-lengths header for that block's Huffman trees.
/// Uses MRU cache for repeated offset encoding within the block.
fn encode_block(tokens: &[Token], writer: &mut BitWriter) {
    // Count frequencies for this block (with MRU cache)
    let mut litlen_freq = vec![0u32; LITLEN_SYMBOLS];
    let mut dist_freq = vec![0u32; DIST_SYMBOLS];

    {
        let mut mru = MruCache::new();
        for token in tokens {
            match token {
                Token::Literal(b) => {
                    litlen_freq[*b as usize] += 1;
                }
                Token::Match { offset, length } => {
                    let (len_code, _, _) = encode_length(*length);
                    litlen_freq[len_code as usize] += 1;
                    if let Some(idx) = mru.find(*offset) {
                        dist_freq[(REP_OFFSET_0 as usize) + idx] += 1;
                        mru.promote(idx);
                    } else {
                        let (dist_code, _, _) = encode_offset(*offset);
                        dist_freq[dist_code as usize] += 1;
                        mru.insert(*offset);
                    }
                }
            }
        }
    }

    // Build Huffman codes for this block
    let litlen_codes = build_huffman_codes(&litlen_freq, LITLEN_SYMBOLS);
    let dist_codes = build_huffman_codes(&dist_freq, DIST_SYMBOLS);

    // Write code lengths header for this block
    for i in 0..LITLEN_SYMBOLS {
        writer.write_bits(litlen_codes[i].len as u32, 4);
    }
    for i in 0..DIST_SYMBOLS {
        writer.write_bits(dist_codes[i].len as u32, 4);
    }

    // Encode tokens (with MRU cache)
    let mut mru = MruCache::new();
    for token in tokens {
        match token {
            Token::Literal(b) => {
                writer.write_code(&litlen_codes[*b as usize]);
            }
            Token::Match { offset, length } => {
                let (len_code, len_extra_bits, len_extra_val) = encode_length(*length);
                writer.write_code(&litlen_codes[len_code as usize]);
                if len_extra_bits > 0 {
                    writer.write_bits(len_extra_val as u32, len_extra_bits);
                }
                if let Some(idx) = mru.find(*offset) {
                    let rep_code = (REP_OFFSET_0 as usize) + idx;
                    writer.write_code(&dist_codes[rep_code]);
                    mru.promote(idx);
                } else {
                    let (dist_code, dist_extra_bits, dist_extra_val) = encode_offset(*offset);
                    writer.write_code(&dist_codes[dist_code as usize]);
                    if dist_extra_bits > 0 {
                        writer.write_bits(dist_extra_val, dist_extra_bits);
                    }
                    mru.insert(*offset);
                }
            }
        }
    }
}

/// Encode LZ77 tokens using per-block Huffman trees.
///
/// Output format:
///   [4B n_tokens LE][4B n_blocks LE]
///   for each block:
///     [4B block_n_tokens LE][code_lengths_header][encoded_bits (byte-aligned)]
///
/// Each block gets its own litlen + dist Huffman trees built from local
/// frequency statistics, just like DEFLATE's per-block adaptive trees.
pub fn huffman_encode_blocked(tokens: &[Token], block_size: usize) -> Vec<u8> {
    let n_tokens = tokens.len();
    let n_blocks = if n_tokens == 0 { 0 } else { (n_tokens + block_size - 1) / block_size };

    let mut result = Vec::new();
    result.extend_from_slice(&(n_tokens as u32).to_le_bytes());
    result.extend_from_slice(&(n_blocks as u32).to_le_bytes());

    for block_idx in 0..n_blocks {
        let start = block_idx * block_size;
        let end = (start + block_size).min(n_tokens);
        let block_tokens = &tokens[start..end];
        let block_n_tokens = block_tokens.len() as u32;

        let mut writer = BitWriter::new();
        encode_block(block_tokens, &mut writer);
        let block_bits = writer.finish();

        result.extend_from_slice(&block_n_tokens.to_le_bytes());
        result.extend_from_slice(&(block_bits.len() as u32).to_le_bytes());
        result.extend_from_slice(&block_bits);
    }

    result
}

/// Decode per-block Huffman-encoded LZ77 tokens.
///
/// Input format matches `huffman_encode_blocked` output.
pub fn huffman_decode_blocked(data: &[u8]) -> Vec<Token> {
    let n_tokens = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let n_blocks = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;

    let mut tokens = Vec::with_capacity(n_tokens);
    let mut pos = 8; // past the two 4-byte headers

    for _ in 0..n_blocks {
        let block_n_tokens = u32::from_le_bytes([
            data[pos], data[pos + 1], data[pos + 2], data[pos + 3],
        ]) as usize;
        pos += 4;

        let block_byte_len = u32::from_le_bytes([
            data[pos], data[pos + 1], data[pos + 2], data[pos + 3],
        ]) as usize;
        pos += 4;

        let block_data = &data[pos..pos + block_byte_len];
        pos += block_byte_len;

        let mut reader = BitReader::new(block_data);

        // Read litlen code lengths (286 x 4 bits)
        let mut litlen_lengths = vec![0u8; LITLEN_SYMBOLS];
        for i in 0..LITLEN_SYMBOLS {
            litlen_lengths[i] = reader.read_bits(4) as u8;
        }

        // Read dist code lengths (48 x 4 bits)
        let mut dist_lengths = vec![0u8; DIST_SYMBOLS];
        for i in 0..DIST_SYMBOLS {
            dist_lengths[i] = reader.read_bits(4) as u8;
        }

        // Rebuild codes and decode tables
        let litlen_codes = canonical_codes(&litlen_lengths, LITLEN_SYMBOLS);
        let dist_codes = canonical_codes(&dist_lengths, DIST_SYMBOLS);
        let litlen_table = build_decode_table(&litlen_codes, LITLEN_SYMBOLS);
        let dist_table = build_decode_table(&dist_codes, DIST_SYMBOLS);

        // Decode block_n_tokens tokens (with MRU cache for rep-matches)
        let mut mru = MruCache::new();
        for _ in 0..block_n_tokens {
            let sym = reader.read_huffman(&litlen_table);
            if sym < 256 {
                tokens.push(Token::Literal(sym as u8));
            } else {
                let (base_len, extra_bits) = LENGTH_TABLE[(sym - 257) as usize];
                let extra_val = if extra_bits > 0 { reader.read_bits(extra_bits) as u16 } else { 0 };
                let length = base_len + extra_val;

                let dist_sym = reader.read_huffman(&dist_table);
                let offset = if dist_sym >= REP_OFFSET_0 {
                    let idx = (dist_sym - REP_OFFSET_0) as usize;
                    let off = mru.recent[idx];
                    mru.promote(idx);
                    off
                } else {
                    let (base_dist, dist_extra) = if (dist_sym as usize) < DIST_TABLE.len() {
                        DIST_TABLE[dist_sym as usize]
                    } else {
                        (1, 22)
                    };
                    let dist_extra_val = if dist_extra > 0 { reader.read_bits(dist_extra) } else { 0 };
                    let off = base_dist + dist_extra_val;
                    mru.insert(off);
                    off
                };

                tokens.push(Token::Match { offset, length });
            }
        }
    }

    tokens
}

// ────────────────────────────────────────────
// Order-1 context modeling constants
// ────────────────────────────────────────────

/// Number of context groups for order-1 modeling.
pub const CONTEXT1_GROUPS: usize = 8;

/// Flag bit set in n_blocks header to indicate context-1 encoding.
const CONTEXT1_FLAG: u32 = 0x80000000;

/// Classify a byte into one of K=8 context groups.
/// Group 0: whitespace (space, tab, newline, CR)
/// Group 1: a-z (lowercase)
/// Group 2: A-Z (uppercase)
/// Group 3: 0-9 (digits)
/// Group 4: common punctuation (.,:;!?'"-)
/// Group 5: brackets and operators (()[]{}<>=+/*&|^~#@$%\)
/// Group 6: 0x00-0x1F control chars (except those in group 0)
/// Group 7: 0x80-0xFF high bytes
#[inline]
fn context_group(byte: u8) -> usize {
    match byte {
        b' ' | b'\t' | b'\n' | b'\r' => 0,
        b'a'..=b'z' => 1,
        b'A'..=b'Z' => 2,
        b'0'..=b'9' => 3,
        b'.' | b',' | b':' | b';' | b'!' | b'?' | b'\'' | b'"' | b'-' | b'_' => 4,
        b'(' | b')' | b'[' | b']' | b'{' | b'}' | b'<' | b'>' | b'=' | b'+' | b'/' | b'*'
        | b'&' | b'|' | b'^' | b'~' | b'#' | b'@' | b'$' | b'%' | b'\\' => 5,
        0x00..=0x1F => 6, // remaining control chars
        0x80..=0xFF => 7,
        // Any remaining printable ASCII (e.g. backtick) falls to punctuation
        _ => 4,
    }
}

/// Helper: encode a single block of tokens with order-1 context into a BitWriter.
/// Writes K litlen code-length headers + 1 dist code-length header + encoded tokens.
fn encode_block_context1(tokens: &[Token], writer: &mut BitWriter) {
    // ── Pass 1: count per-context-group litlen frequencies + global dist frequencies ──
    let mut litlen_freq = vec![vec![0u32; LITLEN_SYMBOLS]; CONTEXT1_GROUPS];
    let mut dist_freq = vec![0u32; DIST_SYMBOLS];

    {
        let mut mru = MruCache::new();
        let mut prev_byte: u8 = 0; // start of block: neutral context
        for token in tokens {
            match token {
                Token::Literal(b) => {
                    let g = context_group(prev_byte);
                    litlen_freq[g][*b as usize] += 1;
                    prev_byte = *b;
                }
                Token::Match { offset, length } => {
                    let g = context_group(prev_byte);
                    let (len_code, _, _) = encode_length(*length);
                    litlen_freq[g][len_code as usize] += 1;
                    if let Some(idx) = mru.find(*offset) {
                        dist_freq[(REP_OFFSET_0 as usize) + idx] += 1;
                        mru.promote(idx);
                    } else {
                        let (dist_code, _, _) = encode_offset(*offset);
                        dist_freq[dist_code as usize] += 1;
                        mru.insert(*offset);
                    }
                    prev_byte = 0; // reset after match
                }
            }
        }
    }

    // ── Build Huffman codes ──
    let mut litlen_codes: Vec<Vec<HuffCode>> = Vec::with_capacity(CONTEXT1_GROUPS);
    for g in 0..CONTEXT1_GROUPS {
        litlen_codes.push(build_huffman_codes(&litlen_freq[g], LITLEN_SYMBOLS));
    }
    let dist_codes = build_huffman_codes(&dist_freq, DIST_SYMBOLS);

    // Build a merged frequency table across all groups as fallback
    let mut merged_freq = vec![0u32; LITLEN_SYMBOLS];
    for g in 0..CONTEXT1_GROUPS {
        for i in 0..LITLEN_SYMBOLS {
            merged_freq[i] += litlen_freq[g][i];
        }
    }
    // Ensure at least 1 for every symbol that appears globally
    // This prevents zero-length codes for symbols that appear in some groups but not others
    let merged_codes = build_huffman_codes(&merged_freq, LITLEN_SYMBOLS);

    // For each group: if it has no codes at all, use merged tree.
    // For each group: ensure every symbol with freq>0 globally also has a code.
    for g in 0..CONTEXT1_GROUPS {
        let has_codes = litlen_codes[g].iter().any(|c| c.len > 0);
        if !has_codes {
            litlen_codes[g] = merged_codes.clone();
        } else {
            // Check if any globally-used symbol is missing from this group
            let mut needs_rebuild = false;
            for i in 0..LITLEN_SYMBOLS {
                if merged_freq[i] > 0 && litlen_codes[g][i].len == 0 {
                    needs_rebuild = true;
                    break;
                }
            }
            if needs_rebuild {
                // Add 1 to every globally-used symbol missing from this group, rebuild
                let mut patched_freq = litlen_freq[g].clone();
                for i in 0..LITLEN_SYMBOLS {
                    if merged_freq[i] > 0 && patched_freq[i] == 0 {
                        patched_freq[i] = 1;
                    }
                }
                litlen_codes[g] = build_huffman_codes(&patched_freq, LITLEN_SYMBOLS);
            }
        }
    }

    // ── Write K litlen code-length headers ──
    for g in 0..CONTEXT1_GROUPS {
        for i in 0..LITLEN_SYMBOLS {
            writer.write_bits(litlen_codes[g][i].len as u32, 4);
        }
    }

    // ── Write 1 dist code-length header ──
    for i in 0..DIST_SYMBOLS {
        writer.write_bits(dist_codes[i].len as u32, 4);
    }

    // ── Pass 2: encode tokens using context-dependent litlen trees ──
    let mut mru = MruCache::new();
    let mut prev_byte: u8 = 0;
    for token in tokens {
        let g = context_group(prev_byte);
        match token {
            Token::Literal(b) => {
                writer.write_code(&litlen_codes[g][*b as usize]);
                prev_byte = *b;
            }
            Token::Match { offset, length } => {
                let (len_code, len_extra_bits, len_extra_val) = encode_length(*length);
                writer.write_code(&litlen_codes[g][len_code as usize]);
                if len_extra_bits > 0 {
                    writer.write_bits(len_extra_val as u32, len_extra_bits);
                }
                if let Some(idx) = mru.find(*offset) {
                    let rep_code = (REP_OFFSET_0 as usize) + idx;
                    writer.write_code(&dist_codes[rep_code]);
                    mru.promote(idx);
                } else {
                    let (dist_code, dist_extra_bits, dist_extra_val) = encode_offset(*offset);
                    writer.write_code(&dist_codes[dist_code as usize]);
                    if dist_extra_bits > 0 {
                        writer.write_bits(dist_extra_val, dist_extra_bits);
                    }
                    mru.insert(*offset);
                }
                prev_byte = 0; // reset after match
            }
        }
    }
}

/// Encode LZ77 tokens using per-block order-1 context Huffman trees.
///
/// For each block, K=8 separate litlen Huffman trees are built (one per context group
/// determined by the previous byte's character class). A single dist tree is shared.
///
/// Output format:
///   [4B n_tokens LE][4B n_blocks LE | 0x80000000 flag]
///   per block:
///     [4B block_n_tokens LE][4B block_byte_len LE]
///     [K × 286 × 4-bit litlen code lengths]
///     [1 × 48 × 4-bit dist code lengths]
///     [encoded tokens with context-dependent literal coding]
pub fn huffman_encode_context1(tokens: &[Token], block_size: usize) -> Vec<u8> {
    let n_tokens = tokens.len();
    let n_blocks = if n_tokens == 0 { 0 } else { (n_tokens + block_size - 1) / block_size };

    let mut result = Vec::new();
    result.extend_from_slice(&(n_tokens as u32).to_le_bytes());
    result.extend_from_slice(&((n_blocks as u32) | CONTEXT1_FLAG).to_le_bytes());

    for block_idx in 0..n_blocks {
        let start = block_idx * block_size;
        let end = (start + block_size).min(n_tokens);
        let block_tokens = &tokens[start..end];
        let block_n_tokens = block_tokens.len() as u32;

        let mut writer = BitWriter::new();
        encode_block_context1(block_tokens, &mut writer);
        let block_bits = writer.finish();

        result.extend_from_slice(&block_n_tokens.to_le_bytes());
        result.extend_from_slice(&(block_bits.len() as u32).to_le_bytes());
        result.extend_from_slice(&block_bits);
    }

    result
}

/// Decode order-1 context Huffman-encoded LZ77 tokens.
///
/// Input format matches `huffman_encode_context1` output.
pub fn huffman_decode_context1(data: &[u8]) -> Vec<Token> {
    let n_tokens = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let raw_n_blocks = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    let n_blocks = (raw_n_blocks & !CONTEXT1_FLAG) as usize;

    let mut tokens = Vec::with_capacity(n_tokens);
    let mut pos = 8;

    for _ in 0..n_blocks {
        let block_n_tokens = u32::from_le_bytes([
            data[pos], data[pos + 1], data[pos + 2], data[pos + 3],
        ]) as usize;
        pos += 4;

        let block_byte_len = u32::from_le_bytes([
            data[pos], data[pos + 1], data[pos + 2], data[pos + 3],
        ]) as usize;
        pos += 4;

        let block_data = &data[pos..pos + block_byte_len];
        pos += block_byte_len;

        let mut reader = BitReader::new(block_data);

        // Read K litlen code-length headers
        let mut litlen_tables: Vec<HuffDecodeTable> = Vec::with_capacity(CONTEXT1_GROUPS);
        let mut litlen_all_lengths: Vec<Vec<u8>> = Vec::with_capacity(CONTEXT1_GROUPS);
        for _ in 0..CONTEXT1_GROUPS {
            let mut lengths = vec![0u8; LITLEN_SYMBOLS];
            for i in 0..LITLEN_SYMBOLS {
                lengths[i] = reader.read_bits(4) as u8;
            }
            litlen_all_lengths.push(lengths);
        }

        // Build decode tables for each context group
        for g in 0..CONTEXT1_GROUPS {
            let codes = canonical_codes(&litlen_all_lengths[g], LITLEN_SYMBOLS);
            litlen_tables.push(build_decode_table(&codes, LITLEN_SYMBOLS));
        }

        // Read 1 dist code-length header
        let mut dist_lengths = vec![0u8; DIST_SYMBOLS];
        for i in 0..DIST_SYMBOLS {
            dist_lengths[i] = reader.read_bits(4) as u8;
        }
        let dist_codes = canonical_codes(&dist_lengths, DIST_SYMBOLS);
        let dist_table = build_decode_table(&dist_codes, DIST_SYMBOLS);

        // Decode tokens with context tracking
        let mut mru = MruCache::new();
        let mut prev_byte: u8 = 0;

        for _ in 0..block_n_tokens {
            let g = context_group(prev_byte);
            let sym = reader.read_huffman(&litlen_tables[g]);
            if sym < 256 {
                prev_byte = sym as u8;
                tokens.push(Token::Literal(prev_byte));
            } else {
                // Length code
                let (base_len, extra_bits) = LENGTH_TABLE[(sym - 257) as usize];
                let extra_val = if extra_bits > 0 { reader.read_bits(extra_bits) as u16 } else { 0 };
                let length = base_len + extra_val;

                // Distance code
                let dist_sym = reader.read_huffman(&dist_table);
                let offset = if dist_sym >= REP_OFFSET_0 {
                    let idx = (dist_sym - REP_OFFSET_0) as usize;
                    let off = mru.recent[idx];
                    mru.promote(idx);
                    off
                } else {
                    let (base_dist, dist_extra) = if (dist_sym as usize) < DIST_TABLE.len() {
                        DIST_TABLE[dist_sym as usize]
                    } else {
                        (1, 22)
                    };
                    let dist_extra_val = if dist_extra > 0 { reader.read_bits(dist_extra) } else { 0 };
                    let off = base_dist + dist_extra_val;
                    mru.insert(off);
                    off
                };

                tokens.push(Token::Match { offset, length });
                prev_byte = 0; // reset after match
            }
        }
    }

    tokens
}

// ────────────────────────────────────────────
// Dynamic block splitting with RLE code-length headers
// ────────────────────────────────────────────

/// Number of code-length alphabet symbols (0-15 literal lengths + 16,17,18 RLE codes).
const CL_SYMBOLS: usize = 19;

/// DEFLATE-style ordering for code-length code lengths (RFC 1951 section 3.2.7).
const CL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Flag bit set in n_blocks header to indicate RLE split encoding (bit 30).
/// Bit 31 is already used by CONTEXT1_FLAG.
const SPLIT_RLE_FLAG: u32 = 0x40000000;

/// Count litlen and dist frequencies for a slice of tokens (using MRU cache).
fn count_frequencies(tokens: &[Token]) -> ([u32; LITLEN_SYMBOLS], [u32; DIST_SYMBOLS]) {
    let mut litlen_freq = [0u32; LITLEN_SYMBOLS];
    let mut dist_freq = [0u32; DIST_SYMBOLS];
    let mut mru = MruCache::new();
    for token in tokens {
        match token {
            Token::Literal(b) => {
                litlen_freq[*b as usize] += 1;
            }
            Token::Match { offset, length } => {
                let (len_code, _, _) = encode_length(*length);
                litlen_freq[len_code as usize] += 1;
                if let Some(idx) = mru.find(*offset) {
                    dist_freq[(REP_OFFSET_0 as usize) + idx] += 1;
                    mru.promote(idx);
                } else {
                    let (dist_code, _, _) = encode_offset(*offset);
                    dist_freq[dist_code as usize] += 1;
                    mru.insert(*offset);
                }
            }
        }
    }
    (litlen_freq, dist_freq)
}

/// RLE-encode a sequence of code lengths using DEFLATE-style codes 16, 17, 18.
/// Returns a vec of (symbol, extra_bits_count, extra_bits_value).
fn rle_encode_lengths(lengths: &[u8]) -> Vec<(u8, u8, u8)> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < lengths.len() {
        let val = lengths[i];
        // Count consecutive identical values
        let mut run = 1usize;
        while i + run < lengths.len() && lengths[i + run] == val {
            run += 1;
        }
        if val == 0 {
            // Use code 18 (repeat zero 11-138, 7 extra bits) then 17 (repeat zero 3-10, 3 extra bits)
            let mut remaining = run;
            while remaining > 0 {
                if remaining >= 11 {
                    let count = remaining.min(138);
                    result.push((18, 7, (count - 11) as u8));
                    remaining -= count;
                } else if remaining >= 3 {
                    let count = remaining.min(10);
                    result.push((17, 3, (count - 3) as u8));
                    remaining -= count;
                } else {
                    // Emit individual zeros
                    result.push((0, 0, 0));
                    remaining -= 1;
                }
            }
        } else {
            // Emit the value itself first
            result.push((val, 0, 0));
            let mut remaining = run - 1;
            // Then use code 16 (repeat previous, 2 extra bits = 3-6 repeats)
            while remaining > 0 {
                if remaining >= 3 {
                    let count = remaining.min(6);
                    result.push((16, 2, (count - 3) as u8));
                    remaining -= count;
                } else {
                    result.push((val, 0, 0));
                    remaining -= 1;
                }
            }
        }
        i += run;
    }
    result
}

/// Estimate the exact output size in bits for encoding a block of tokens,
/// using RLE-compressed code-length headers.
/// Returns the total number of bits (header + encoded data).
pub fn estimate_block_bits(tokens: &[Token]) -> usize {
    if tokens.is_empty() {
        return 0;
    }

    let (litlen_freq, dist_freq) = count_frequencies(tokens);

    let litlen_codes = build_huffman_codes(&litlen_freq, LITLEN_SYMBOLS);
    let dist_codes = build_huffman_codes(&dist_freq, DIST_SYMBOLS);

    // Build RLE-encoded code lengths
    let mut all_lengths = Vec::with_capacity(LITLEN_SYMBOLS + DIST_SYMBOLS);
    for i in 0..LITLEN_SYMBOLS {
        all_lengths.push(litlen_codes[i].len);
    }
    for i in 0..DIST_SYMBOLS {
        all_lengths.push(dist_codes[i].len);
    }
    let rle = rle_encode_lengths(&all_lengths);

    // Count frequencies of the CL symbols
    let mut cl_freq = [0u32; CL_SYMBOLS];
    for &(sym, _, _) in &rle {
        cl_freq[sym as usize] += 1;
    }
    let cl_codes = build_huffman_codes(&cl_freq, CL_SYMBOLS);

    // Header bits:
    // 4 bits: HCLEN (number of CL code lengths to write, minus 4)
    // HCLEN * 3 bits: code-length code lengths in CL_ORDER
    // RLE-encoded code lengths for all 286 + 48 = 334 symbols
    let hclen = {
        let mut last = 3usize; // minimum 4 entries
        for i in (0..CL_SYMBOLS).rev() {
            if cl_codes[CL_ORDER[i]].len > 0 {
                last = i;
                break;
            }
        }
        (last + 1).max(4)
    };

    let mut bits = 0usize;
    bits += 4; // HCLEN
    bits += hclen * 3; // code-length code lengths

    // RLE-encoded code lengths
    for &(sym, extra_count, _) in &rle {
        let code = &cl_codes[sym as usize];
        bits += code.len as usize;
        bits += extra_count as usize;
    }

    // Encoded token data bits
    let mut mru = MruCache::new();
    for token in tokens {
        match token {
            Token::Literal(b) => {
                bits += litlen_codes[*b as usize].len as usize;
            }
            Token::Match { offset, length } => {
                let (len_code, len_extra_bits, _) = encode_length(*length);
                bits += litlen_codes[len_code as usize].len as usize;
                bits += len_extra_bits as usize;
                if let Some(idx) = mru.find(*offset) {
                    let rep_code = (REP_OFFSET_0 as usize) + idx;
                    bits += dist_codes[rep_code].len as usize;
                    mru.promote(idx);
                } else {
                    let (dist_code, dist_extra_bits, _) = encode_offset(*offset);
                    bits += dist_codes[dist_code as usize].len as usize;
                    bits += dist_extra_bits as usize;
                    mru.insert(*offset);
                }
            }
        }
    }

    bits
}

/// Recursively find optimal split points. Returns a list of (start, end) ranges.
fn find_splits(tokens: &[Token], start: usize, end: usize, min_block: usize) -> Vec<(usize, usize)> {
    let len = end - start;
    if len <= min_block {
        return vec![(start, end)];
    }

    let whole_bits = estimate_block_bits(&tokens[start..end]);
    let mid = start + len / 2;
    let left_bits = estimate_block_bits(&tokens[start..mid]);
    let right_bits = estimate_block_bits(&tokens[mid..end]);

    if left_bits + right_bits < whole_bits {
        // Splitting is beneficial; recurse on each half
        let mut result = find_splits(tokens, start, mid, min_block);
        result.extend(find_splits(tokens, mid, end, min_block));
        result
    } else {
        vec![(start, end)]
    }
}

/// Write a block with RLE-compressed code-length headers into a BitWriter.
fn encode_block_rle(tokens: &[Token], writer: &mut BitWriter) {
    let (litlen_freq, dist_freq) = count_frequencies(tokens);
    let litlen_codes = build_huffman_codes(&litlen_freq, LITLEN_SYMBOLS);
    let dist_codes = build_huffman_codes(&dist_freq, DIST_SYMBOLS);

    // Combine all code lengths
    let mut all_lengths = Vec::with_capacity(LITLEN_SYMBOLS + DIST_SYMBOLS);
    for i in 0..LITLEN_SYMBOLS {
        all_lengths.push(litlen_codes[i].len);
    }
    for i in 0..DIST_SYMBOLS {
        all_lengths.push(dist_codes[i].len);
    }
    let rle = rle_encode_lengths(&all_lengths);

    // Build code-length Huffman tree
    let mut cl_freq = [0u32; CL_SYMBOLS];
    for &(sym, _, _) in &rle {
        cl_freq[sym as usize] += 1;
    }
    let cl_codes = build_huffman_codes(&cl_freq, CL_SYMBOLS);

    // Determine HCLEN: how many code-length code lengths to write (at least 4)
    let hclen = {
        let mut last = 3usize;
        for i in (0..CL_SYMBOLS).rev() {
            if cl_codes[CL_ORDER[i]].len > 0 {
                last = i;
                break;
            }
        }
        (last + 1).max(4)
    };

    // Write HCLEN only (symbol counts are fixed at 286 + 48 = 334)
    writer.write_bits((hclen - 4) as u32, 4);

    // Write code-length code lengths in DEFLATE order (3 bits each)
    for i in 0..hclen {
        writer.write_bits(cl_codes[CL_ORDER[i]].len as u32, 3);
    }

    // Write RLE-encoded code lengths
    for &(sym, extra_count, extra_val) in &rle {
        writer.write_code(&cl_codes[sym as usize]);
        if extra_count > 0 {
            writer.write_bits(extra_val as u32, extra_count);
        }
    }

    // Encode tokens (with MRU cache)
    let mut mru = MruCache::new();
    for token in tokens {
        match token {
            Token::Literal(b) => {
                writer.write_code(&litlen_codes[*b as usize]);
            }
            Token::Match { offset, length } => {
                let (len_code, len_extra_bits, len_extra_val) = encode_length(*length);
                writer.write_code(&litlen_codes[len_code as usize]);
                if len_extra_bits > 0 {
                    writer.write_bits(len_extra_val as u32, len_extra_bits);
                }
                if let Some(idx) = mru.find(*offset) {
                    let rep_code = (REP_OFFSET_0 as usize) + idx;
                    writer.write_code(&dist_codes[rep_code]);
                    mru.promote(idx);
                } else {
                    let (dist_code, dist_extra_bits, dist_extra_val) = encode_offset(*offset);
                    writer.write_code(&dist_codes[dist_code as usize]);
                    if dist_extra_bits > 0 {
                        writer.write_bits(dist_extra_val, dist_extra_bits);
                    }
                    mru.insert(*offset);
                }
            }
        }
    }
}

/// Encode LZ77 tokens using dynamic block splitting with RLE-compressed headers.
///
/// Output format:
///   [4B n_tokens LE][4B n_blocks LE (bit 30 set = RLE split format)]
///   for each block:
///     [4B block_n_tokens LE][4B block_byte_len LE][block_bitstream]
///
/// The encoder:
/// 1. Starts with initial blocks of 32768 tokens
/// 2. Recursively tries splitting each in half (down to 4096 minimum)
/// 3. Keeps the split only when it produces fewer bits
/// 4. Writes blocks with DEFLATE-style RLE code-length headers
pub fn huffman_encode_split(tokens: &[Token]) -> Vec<u8> {
    let n_tokens = tokens.len();
    let initial_block_size = 32768usize;
    let min_block_size = 4096usize;

    // Find all split points
    let mut blocks: Vec<(usize, usize)> = Vec::new();
    let mut pos = 0;
    while pos < n_tokens {
        let end = (pos + initial_block_size).min(n_tokens);
        let mut sub_blocks = find_splits(tokens, pos, end, min_block_size);
        blocks.append(&mut sub_blocks);
        pos = end;
    }

    let n_blocks = blocks.len();

    let mut result = Vec::new();
    result.extend_from_slice(&(n_tokens as u32).to_le_bytes());
    // Set bit 30 to signal RLE code-length format
    let n_blocks_with_flag = (n_blocks as u32) | SPLIT_RLE_FLAG;
    result.extend_from_slice(&n_blocks_with_flag.to_le_bytes());

    for &(start, end) in &blocks {
        let block_tokens = &tokens[start..end];
        let block_n_tokens = block_tokens.len() as u32;

        let mut writer = BitWriter::new();
        encode_block_rle(block_tokens, &mut writer);
        let block_bits = writer.finish();

        result.extend_from_slice(&block_n_tokens.to_le_bytes());
        result.extend_from_slice(&(block_bits.len() as u32).to_le_bytes());
        result.extend_from_slice(&block_bits);
    }

    result
}

/// Decode a block with RLE-compressed code-length headers from a BitReader.
/// Returns the decoded tokens.
fn decode_block_rle(reader: &mut BitReader, block_n_tokens: usize) -> Vec<Token> {
    // Read HCLEN (symbol counts are fixed at LITLEN_SYMBOLS + DIST_SYMBOLS = 334)
    let hclen = reader.read_bits(4) as usize + 4;

    // Read code-length code lengths (3 bits each, in DEFLATE order)
    let mut cl_lengths = [0u8; CL_SYMBOLS];
    for i in 0..hclen {
        cl_lengths[CL_ORDER[i]] = reader.read_bits(3) as u8;
    }

    // Build code-length decode table
    let cl_codes = canonical_codes(&cl_lengths, CL_SYMBOLS);
    let cl_table = build_decode_table(&cl_codes, CL_SYMBOLS);

    // Decode litlen + dist code lengths using the CL tree
    let total_codes = LITLEN_SYMBOLS + DIST_SYMBOLS;
    let mut all_lengths = Vec::with_capacity(total_codes);
    while all_lengths.len() < total_codes {
        let sym = reader.read_huffman(&cl_table);
        match sym {
            0..=15 => {
                all_lengths.push(sym as u8);
            }
            16 => {
                // Repeat previous length 3-6 times
                let count = reader.read_bits(2) as usize + 3;
                let prev = if all_lengths.is_empty() { 0 } else { *all_lengths.last().unwrap() };
                for _ in 0..count {
                    all_lengths.push(prev);
                }
            }
            17 => {
                // Repeat zero 3-10 times
                let count = reader.read_bits(3) as usize + 3;
                for _ in 0..count {
                    all_lengths.push(0);
                }
            }
            18 => {
                // Repeat zero 11-138 times
                let count = reader.read_bits(7) as usize + 11;
                for _ in 0..count {
                    all_lengths.push(0);
                }
            }
            _ => panic!("Invalid code-length symbol: {}", sym),
        }
    }
    // Trim to exact count in case RLE overshot
    all_lengths.truncate(total_codes);

    // Split into litlen and dist lengths (fixed sizes)
    let ll = &all_lengths[..LITLEN_SYMBOLS];
    let dl = &all_lengths[LITLEN_SYMBOLS..];

    let litlen_codes = canonical_codes(ll, LITLEN_SYMBOLS);
    let dist_codes = canonical_codes(dl, DIST_SYMBOLS);
    let litlen_table = build_decode_table(&litlen_codes, LITLEN_SYMBOLS);
    let dist_table = build_decode_table(&dist_codes, DIST_SYMBOLS);

    // Decode tokens
    let mut tokens = Vec::with_capacity(block_n_tokens);
    let mut mru = MruCache::new();
    for _ in 0..block_n_tokens {
        let sym = reader.read_huffman(&litlen_table);
        if sym < 256 {
            tokens.push(Token::Literal(sym as u8));
        } else {
            let (base_len, extra_bits) = LENGTH_TABLE[(sym - 257) as usize];
            let extra_val = if extra_bits > 0 { reader.read_bits(extra_bits) as u16 } else { 0 };
            let length = base_len + extra_val;

            let dist_sym = reader.read_huffman(&dist_table);
            let offset = if dist_sym >= REP_OFFSET_0 {
                let idx = (dist_sym - REP_OFFSET_0) as usize;
                let off = mru.recent[idx];
                mru.promote(idx);
                off
            } else {
                let (base_dist, dist_extra) = if (dist_sym as usize) < DIST_TABLE.len() {
                    DIST_TABLE[dist_sym as usize]
                } else {
                    (1, 22)
                };
                let dist_extra_val = if dist_extra > 0 { reader.read_bits(dist_extra) } else { 0 };
                let off = base_dist + dist_extra_val;
                mru.insert(off);
                off
            };

            tokens.push(Token::Match { offset, length });
        }
    }

    tokens
}

/// Decode dynamically-split Huffman-encoded LZ77 tokens.
///
/// Handles the RLE split format (bit 30 set in n_blocks header).
/// If bit 30 is not set, falls back to `huffman_decode_blocked`.
pub fn huffman_decode_split(data: &[u8]) -> Vec<Token> {
    let n_tokens = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let n_blocks_raw = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    let rle_format = (n_blocks_raw & SPLIT_RLE_FLAG) != 0;

    if !rle_format {
        // Fall back to the plain blocked decoder
        return huffman_decode_blocked(data);
    }

    let n_blocks = (n_blocks_raw & !(SPLIT_RLE_FLAG | CONTEXT1_FLAG)) as usize;

    let mut tokens = Vec::with_capacity(n_tokens);
    let mut pos = 8;

    for _ in 0..n_blocks {
        let block_n_tokens = u32::from_le_bytes([
            data[pos], data[pos + 1], data[pos + 2], data[pos + 3],
        ]) as usize;
        pos += 4;

        let block_byte_len = u32::from_le_bytes([
            data[pos], data[pos + 1], data[pos + 2], data[pos + 3],
        ]) as usize;
        pos += 4;

        let block_data = &data[pos..pos + block_byte_len];
        pos += block_byte_len;

        let mut reader = BitReader::new(block_data);
        let mut block_tokens = decode_block_rle(&mut reader, block_n_tokens);
        tokens.append(&mut block_tokens);
    }

    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_length_roundtrip() {
        for len in 3..=258u16 {
            let (code, extra_bits, extra_val) = encode_length(len);
            let decoded = decode_length(code, extra_val);
            assert_eq!(len, decoded, "Length {len}: code={code}, extra={extra_bits}b, val={extra_val}");
        }
    }

    #[test]
    fn test_offset_roundtrip() {
        // Test common offsets
        for &off in &[1, 2, 3, 4, 5, 10, 100, 1000, 10000, 32768, 65536, 100000] {
            let (code, extra_bits, extra_val) = encode_offset(off);
            let decoded = decode_offset(code, extra_val);
            assert_eq!(off, decoded, "Offset {off}: code={code}, extra={extra_bits}b");
        }
    }

    #[test]
    fn test_huffman_tokens_roundtrip() {
        let tokens = vec![
            Token::Literal(b'H'),
            Token::Literal(b'e'),
            Token::Literal(b'l'),
            Token::Literal(b'l'),
            Token::Literal(b'o'),
            Token::Match { offset: 5, length: 5 }, // copy "Hello"
            Token::Literal(b'!'),
        ];
        let encoded = huffman_encode(&tokens);
        let decoded = huffman_decode(&encoded);
        assert_eq!(tokens, decoded);
    }

    #[test]
    fn test_huffman_large_offset() {
        let tokens = vec![
            Token::Literal(b'A'),
            Token::Match { offset: 200_000, length: 50 },
            Token::Match { offset: 1, length: 258 }, // run-length
        ];
        let encoded = huffman_encode(&tokens);
        let decoded = huffman_decode(&encoded);
        assert_eq!(tokens, decoded);
    }
}

#[cfg(test)]
mod corpus_debug {
    use super::*;

    #[test]
    fn test_many_literals() {
        // Test with 1000 diverse literals — similar to real corpus
        let tokens: Vec<Token> = (0..1000u32).map(|i| {
            if i % 10 == 0 {
                Token::Match { offset: (i % 200 + 1) as u32, length: (i % 20 + 4) as u16 }
            } else {
                Token::Literal((i % 256) as u8)
            }
        }).collect();
        let encoded = huffman_encode(&tokens);
        let decoded = huffman_decode(&encoded);
        assert_eq!(tokens.len(), decoded.len(), "count mismatch");
        for (i, (o, d)) in tokens.iter().zip(decoded.iter()).enumerate() {
            assert_eq!(o, d, "mismatch at {i}");
        }
    }

    #[test]
    fn test_all_literal_values() {
        // Every possible literal byte value
        let mut tokens: Vec<Token> = (0..=255u8).map(|b| Token::Literal(b)).collect();
        // Add some matches too
        tokens.push(Token::Match { offset: 10, length: 10 });
        tokens.push(Token::Match { offset: 100000, length: 100 });
        let encoded = huffman_encode(&tokens);
        let decoded = huffman_decode(&encoded);
        assert_eq!(tokens, decoded);
    }
}

#[cfg(test)]
mod large_test {
    use super::*;

    #[test]
    fn test_large_token_stream() {
        // 400K tokens — close to corpus size
        let tokens: Vec<Token> = (0..400_000u32).map(|i| {
            let r = i.wrapping_mul(2654435761);
            if r % 5 < 3 {
                Token::Literal((r % 256) as u8)
            } else if r % 5 == 3 {
                Token::Match { offset: (r % 65536 + 1), length: ((r % 20) as u16 + 4) }
            } else {
                Token::Match { offset: (r % 1000000 + 1), length: ((r % 100) as u16 + 4) }
            }
        }).collect();
        let encoded = huffman_encode(&tokens);
        let decoded = huffman_decode(&encoded);
        assert_eq!(tokens.len(), decoded.len(), "count mismatch: {} vs {}", tokens.len(), decoded.len());
        for (i, (o, d)) in tokens.iter().zip(decoded.iter()).enumerate() {
            if o != d {
                panic!("mismatch at {i}: orig={o:?}, dec={d:?}");
            }
        }
    }
}

#[cfg(test)]
mod blocked_tests {
    use super::*;

    #[test]
    fn test_blocked_roundtrip_basic() {
        let tokens = vec![
            Token::Literal(b'H'),
            Token::Literal(b'e'),
            Token::Literal(b'l'),
            Token::Literal(b'l'),
            Token::Literal(b'o'),
            Token::Match { offset: 5, length: 5 },
            Token::Literal(b'!'),
        ];
        let encoded = huffman_encode_blocked(&tokens, 4);
        let decoded = huffman_decode_blocked(&encoded);
        assert_eq!(tokens, decoded);
    }

    #[test]
    fn test_blocked_roundtrip_large() {
        // 400K tokens with mixed literals and matches
        let tokens: Vec<Token> = (0..400_000u32).map(|i| {
            let r = i.wrapping_mul(2654435761);
            if r % 5 < 3 {
                Token::Literal((r % 256) as u8)
            } else if r % 5 == 3 {
                Token::Match { offset: (r % 65536 + 1), length: ((r % 20) as u16 + 4) }
            } else {
                Token::Match { offset: (r % 1000000 + 1), length: ((r % 100) as u16 + 4) }
            }
        }).collect();

        for &bs in &[4096, 8192, 16384, 32768] {
            let encoded = huffman_encode_blocked(&tokens, bs);
            let decoded = huffman_decode_blocked(&encoded);
            assert_eq!(tokens.len(), decoded.len(), "blocked(bs={bs}) count mismatch");
            for (i, (o, d)) in tokens.iter().zip(decoded.iter()).enumerate() {
                if o != d {
                    panic!("blocked(bs={bs}) mismatch at {i}: orig={o:?}, dec={d:?}");
                }
            }
        }
    }

    #[test]
    fn test_blocked_empty() {
        let tokens: Vec<Token> = vec![];
        let encoded = huffman_encode_blocked(&tokens, 16384);
        let decoded = huffman_decode_blocked(&encoded);
        assert_eq!(tokens, decoded);
    }

    #[test]
    fn test_blocked_single_token() {
        let tokens = vec![Token::Literal(b'X')];
        let encoded = huffman_encode_blocked(&tokens, 16384);
        let decoded = huffman_decode_blocked(&encoded);
        assert_eq!(tokens, decoded);
    }

    #[test]
    fn test_blocked_vs_global_corpus() {
        // Read the corpus fixture and generate tokens from it
        let corpus_path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/corpus.tar");
        let corpus_data = std::fs::read(corpus_path)
            .expect("tests/fixtures/corpus.tar must exist");

        // Generate tokens from corpus data using LZ77 encoder
        let mut enc = crate::lz77::Lz77Encoder::new();
        let (tokens, _rep_count) = enc.encode(&corpus_data);

        eprintln!("corpus tokens: {}", tokens.len());

        // Global (single-tree) encode
        let global_encoded = huffman_encode(&tokens);
        let global_decoded = huffman_decode(&global_encoded);
        assert_eq!(tokens.len(), global_decoded.len(), "global roundtrip count mismatch");
        for (i, (o, d)) in tokens.iter().zip(global_decoded.iter()).enumerate() {
            if o != d {
                panic!("global roundtrip mismatch at {i}: orig={o:?}, dec={d:?}");
            }
        }

        let global_size = global_encoded.len();
        eprintln!("global huffman size: {} bytes", global_size);

        // Per-block encode at various block sizes
        let mut best_size = usize::MAX;
        let mut best_bs = 0;
        for &bs in &[4096, 8192, 16384, 32768] {
            let blocked_encoded = huffman_encode_blocked(&tokens, bs);
            let blocked_decoded = huffman_decode_blocked(&blocked_encoded);
            assert_eq!(
                tokens.len(),
                blocked_decoded.len(),
                "blocked(bs={bs}) roundtrip count mismatch"
            );
            for (i, (o, d)) in tokens.iter().zip(blocked_decoded.iter()).enumerate() {
                if o != d {
                    panic!("blocked(bs={bs}) roundtrip mismatch at {i}: orig={o:?}, dec={d:?}");
                }
            }
            let blocked_size = blocked_encoded.len();
            let savings_pct = if global_size > 0 {
                (1.0 - blocked_size as f64 / global_size as f64) * 100.0
            } else {
                0.0
            };
            eprintln!(
                "blocked(bs={:>5}): {} bytes  ({:+.2}% vs global)",
                bs, blocked_size, savings_pct
            );
            if blocked_size < best_size {
                best_size = blocked_size;
                best_bs = bs;
            }
        }

        eprintln!("BEST block size: {} ({} bytes, global was {} bytes)", best_bs, best_size, global_size);

        // The blocked encoding should not be catastrophically larger
        assert!(
            best_size <= global_size + global_size / 10,
            "blocked encoding should be within 10% of global at worst: best={best_size}, global={global_size}"
        );
    }
}

/// Encode without rep-offsets (for benchmarking / comparison).
/// Same format as `huffman_encode` but treats every match offset as a fresh distance code.
#[cfg(test)]
fn huffman_encode_no_rep(tokens: &[Token]) -> Vec<u8> {
    let mut litlen_freq = vec![0u32; LITLEN_SYMBOLS];
    let mut dist_freq = vec![0u32; DIST_SYMBOLS];

    for token in tokens {
        match token {
            Token::Literal(b) => {
                litlen_freq[*b as usize] += 1;
            }
            Token::Match { offset, length } => {
                let (len_code, _, _) = encode_length(*length);
                litlen_freq[len_code as usize] += 1;
                let (dist_code, _, _) = encode_offset(*offset);
                dist_freq[dist_code as usize] += 1;
            }
        }
    }

    let litlen_codes = build_huffman_codes(&litlen_freq, LITLEN_SYMBOLS);
    let dist_codes = build_huffman_codes(&dist_freq, DIST_SYMBOLS);

    let mut prefix = Vec::new();
    prefix.extend_from_slice(&(tokens.len() as u32).to_le_bytes());

    let mut writer = BitWriter::new();

    for i in 0..LITLEN_SYMBOLS {
        writer.write_bits(litlen_codes[i].len as u32, 4);
    }
    for i in 0..DIST_SYMBOLS {
        writer.write_bits(dist_codes[i].len as u32, 4);
    }

    for token in tokens {
        match token {
            Token::Literal(b) => {
                writer.write_code(&litlen_codes[*b as usize]);
            }
            Token::Match { offset, length } => {
                let (len_code, len_extra_bits, len_extra_val) = encode_length(*length);
                writer.write_code(&litlen_codes[len_code as usize]);
                if len_extra_bits > 0 {
                    writer.write_bits(len_extra_val as u32, len_extra_bits);
                }
                let (dist_code, dist_extra_bits, dist_extra_val) = encode_offset(*offset);
                writer.write_code(&dist_codes[dist_code as usize]);
                if dist_extra_bits > 0 {
                    writer.write_bits(dist_extra_val, dist_extra_bits);
                }
            }
        }
    }

    let mut result = prefix;
    result.extend_from_slice(&writer.finish());
    result
}

#[cfg(test)]
mod rep_offset_tests {
    use super::*;

    #[test]
    fn test_rep_offset_roundtrip() {
        // Tokens that heavily reuse offsets — exercises all 4 MRU slots
        let tokens = vec![
            Token::Literal(b'A'),
            Token::Match { offset: 100, length: 5 },   // new offset -> MRU [100, 2, 3, 4]
            Token::Literal(b'B'),
            Token::Match { offset: 100, length: 4 },   // rep[0]
            Token::Match { offset: 200, length: 6 },   // new -> MRU [200, 100, 2, 3]
            Token::Match { offset: 100, length: 3 },   // rep[1] -> MRU [100, 200, 2, 3]
            Token::Match { offset: 200, length: 7 },   // rep[1] -> MRU [200, 100, 2, 3]
            Token::Match { offset: 2, length: 3 },     // rep[2] -> MRU [2, 200, 100, 3]
            Token::Match { offset: 300, length: 10 },  // new -> MRU [300, 2, 200, 100]
            Token::Match { offset: 200, length: 4 },   // rep[2] -> MRU [200, 300, 2, 100]
            Token::Match { offset: 300, length: 5 },   // rep[1] -> MRU [300, 200, 2, 100]
            Token::Match { offset: 100, length: 3 },   // rep[3] -> MRU [100, 300, 200, 2]
            Token::Literal(b'Z'),
        ];
        let encoded = huffman_encode(&tokens);
        let decoded = huffman_decode(&encoded);
        assert_eq!(tokens, decoded, "rep-offset roundtrip failed");
    }

    #[test]
    fn test_rep_offset_initial_cache_values() {
        // The MRU cache starts as [1, 2, 3, 4].
        // Use those initial values directly.
        let tokens = vec![
            Token::Match { offset: 1, length: 3 },   // rep[0] (initial cache hit)
            Token::Match { offset: 2, length: 4 },   // rep[1] -> MRU [2, 1, 3, 4]
            Token::Match { offset: 3, length: 5 },   // rep[2] -> MRU [3, 2, 1, 4]
            Token::Match { offset: 4, length: 3 },   // rep[3] -> MRU [4, 3, 2, 1]
            Token::Match { offset: 1, length: 3 },   // rep[3] -> MRU [1, 4, 3, 2]
        ];
        let encoded = huffman_encode(&tokens);
        let decoded = huffman_decode(&encoded);
        assert_eq!(tokens, decoded, "initial cache rep-offset roundtrip failed");
    }

    #[test]
    fn test_rep_offset_mixed_large() {
        // Mix of rep and non-rep offsets at scale
        let tokens: Vec<Token> = (0..10_000u32).map(|i| {
            if i % 3 == 0 {
                Token::Literal((i % 256) as u8)
            } else {
                // Cycle through a small set of offsets to trigger many rep-matches
                let offsets = [10, 20, 30, 10, 10, 20, 50, 100];
                Token::Match {
                    offset: offsets[(i as usize) % offsets.len()],
                    length: ((i % 20) as u16 + 3),
                }
            }
        }).collect();
        let encoded = huffman_encode(&tokens);
        let decoded = huffman_decode(&encoded);
        assert_eq!(tokens.len(), decoded.len(), "count mismatch");
        for (i, (o, d)) in tokens.iter().zip(decoded.iter()).enumerate() {
            assert_eq!(o, d, "mismatch at token {i}");
        }
    }

    #[test]
    fn test_rep_offset_saves_space() {
        // Build tokens with many repeated offsets
        let tokens: Vec<Token> = (0..5_000u32).map(|i| {
            if i % 5 == 0 {
                Token::Literal((i % 256) as u8)
            } else {
                // Cycle through just 3 offsets -> ~100% rep-match rate after warmup
                let offsets = [42, 137, 999];
                Token::Match {
                    offset: offsets[(i as usize) % offsets.len()],
                    length: ((i % 20) as u16 + 3),
                }
            }
        }).collect();

        let with_rep = huffman_encode(&tokens);
        let without_rep = huffman_encode_no_rep(&tokens);

        eprintln!(
            "rep-offset savings: with_rep={} bytes, without_rep={} bytes, saved={} bytes ({:.1}%)",
            with_rep.len(),
            without_rep.len(),
            without_rep.len() as isize - with_rep.len() as isize,
            (1.0 - with_rep.len() as f64 / without_rep.len() as f64) * 100.0,
        );

        assert!(
            with_rep.len() < without_rep.len(),
            "rep-offset encoding should be smaller: with_rep={}, without_rep={}",
            with_rep.len(),
            without_rep.len(),
        );
    }

    #[test]
    fn test_rep_offset_all_4_slots() {
        // Exercise all 4 MRU slots with roundtrip through all encode/decode paths.
        // MRU starts as [1, 2, 3, 4].
        let tokens = vec![
            // Use initial cache values
            Token::Match { offset: 4, length: 3 },   // rep[3] -> MRU [4, 1, 2, 3]
            Token::Match { offset: 3, length: 4 },   // rep[3] -> MRU [3, 4, 1, 2]
            Token::Match { offset: 2, length: 5 },   // rep[3] -> MRU [2, 3, 4, 1]
            Token::Match { offset: 1, length: 3 },   // rep[3] -> MRU [1, 2, 3, 4]
            // Insert new offsets and exercise slot 3
            Token::Match { offset: 10, length: 3 },  // new -> MRU [10, 1, 2, 3]
            Token::Match { offset: 20, length: 4 },  // new -> MRU [20, 10, 1, 2]
            Token::Match { offset: 30, length: 5 },  // new -> MRU [30, 20, 10, 1]
            Token::Match { offset: 40, length: 6 },  // new -> MRU [40, 30, 20, 10]
            Token::Match { offset: 10, length: 3 },  // rep[3] -> MRU [10, 40, 30, 20]
            Token::Match { offset: 20, length: 4 },  // rep[3] -> MRU [20, 10, 40, 30]
            Token::Match { offset: 30, length: 5 },  // rep[3] -> MRU [30, 20, 10, 40]
            Token::Match { offset: 40, length: 6 },  // rep[3] -> MRU [40, 30, 20, 10]
            // Promote from each slot
            Token::Match { offset: 40, length: 3 },  // rep[0]
            Token::Match { offset: 20, length: 3 },  // rep[2] -> MRU [20, 40, 30, 10]
            Token::Match { offset: 10, length: 3 },  // rep[3] -> MRU [10, 20, 40, 30]
            Token::Literal(b'X'),
        ];

        // Test all encode/decode paths
        let enc1 = huffman_encode(&tokens);
        let dec1 = huffman_decode(&enc1);
        assert_eq!(tokens, dec1, "global roundtrip with 4 MRU slots failed");

        let enc2 = huffman_encode_blocked(&tokens, 8);
        let dec2 = huffman_decode_blocked(&enc2);
        assert_eq!(tokens, dec2, "blocked roundtrip with 4 MRU slots failed");

        let enc3 = huffman_encode_context1(&tokens, 8);
        let dec3 = huffman_decode_context1(&enc3);
        assert_eq!(tokens, dec3, "context1 roundtrip with 4 MRU slots failed");

        let enc4 = huffman_encode_split(&tokens);
        let dec4 = huffman_decode_split(&enc4);
        assert_eq!(tokens, dec4, "split roundtrip with 4 MRU slots failed");
    }
}

#[cfg(test)]
mod context1_tests {
    use super::*;

    #[test]
    fn test_context1_roundtrip_basic() {
        let tokens = vec![
            Token::Literal(b'H'),
            Token::Literal(b'e'),
            Token::Literal(b'l'),
            Token::Literal(b'l'),
            Token::Literal(b'o'),
            Token::Match { offset: 5, length: 5 },
            Token::Literal(b'!'),
        ];
        let encoded = huffman_encode_context1(&tokens, 4);
        let decoded = huffman_decode_context1(&encoded);
        assert_eq!(tokens, decoded);
    }

    #[test]
    fn test_context1_roundtrip_large() {
        let tokens: Vec<Token> = (0..400_000u32).map(|i| {
            let r = i.wrapping_mul(2654435761);
            if r % 5 < 3 {
                Token::Literal((r % 256) as u8)
            } else if r % 5 == 3 {
                Token::Match { offset: (r % 65536 + 1), length: ((r % 20) as u16 + 4) }
            } else {
                Token::Match { offset: (r % 1000000 + 1), length: ((r % 100) as u16 + 4) }
            }
        }).collect();

        for &bs in &[4096, 16384] {
            let encoded = huffman_encode_context1(&tokens, bs);
            let decoded = huffman_decode_context1(&encoded);
            assert_eq!(tokens.len(), decoded.len(), "context1(bs={bs}) count mismatch");
            for (i, (o, d)) in tokens.iter().zip(decoded.iter()).enumerate() {
                if o != d {
                    panic!("context1(bs={bs}) mismatch at {i}: orig={o:?}, dec={d:?}");
                }
            }
        }
    }

    #[test]
    fn test_context1_empty() {
        let tokens: Vec<Token> = vec![];
        let encoded = huffman_encode_context1(&tokens, 16384);
        let decoded = huffman_decode_context1(&encoded);
        assert_eq!(tokens, decoded);
    }

    #[test]
    fn test_context1_single_token() {
        let tokens = vec![Token::Literal(b'X')];
        let encoded = huffman_encode_context1(&tokens, 16384);
        let decoded = huffman_decode_context1(&encoded);
        assert_eq!(tokens, decoded);
    }

    #[test]
    fn test_context1_flag_set() {
        let tokens = vec![Token::Literal(b'A'), Token::Literal(b'B')];
        let encoded = huffman_encode_context1(&tokens, 16384);
        let raw_n_blocks = u32::from_le_bytes([encoded[4], encoded[5], encoded[6], encoded[7]]);
        assert!(raw_n_blocks & 0x80000000 != 0, "context1 flag must be set");
        assert_eq!(raw_n_blocks & !0x80000000, 1, "should be 1 block");
    }

    #[test]
    fn test_context_group_classification() {
        assert_eq!(context_group(b' '), 0);
        assert_eq!(context_group(b'\n'), 0);
        assert_eq!(context_group(b'\t'), 0);
        assert_eq!(context_group(b'a'), 1);
        assert_eq!(context_group(b'z'), 1);
        assert_eq!(context_group(b'A'), 2);
        assert_eq!(context_group(b'Z'), 2);
        assert_eq!(context_group(b'0'), 3);
        assert_eq!(context_group(b'9'), 3);
        assert_eq!(context_group(b'.'), 4);
        assert_eq!(context_group(b','), 4);
        assert_eq!(context_group(b'('), 5);
        assert_eq!(context_group(b'+'), 5);
        assert_eq!(context_group(0x01), 6);
        assert_eq!(context_group(0x80), 7);
        assert_eq!(context_group(0xFF), 7);
    }

    #[test]
    fn test_context1_vs_blocked_corpus() {
        let corpus_path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/corpus.tar");
        let corpus_data = std::fs::read(corpus_path)
            .expect("tests/fixtures/corpus.tar must exist");

        let mut enc = crate::lz77::Lz77Encoder::new();
        let (tokens, _rep_count) = enc.encode(&corpus_data);

        let blocked = huffman_encode_blocked(&tokens, 16384);
        let ctx1 = huffman_encode_context1(&tokens, 16384);

        eprintln!(
            "context1 vs blocked: ctx1={} bytes, blocked={} bytes, delta={} bytes ({:.2}%)",
            ctx1.len(),
            blocked.len(),
            blocked.len() as isize - ctx1.len() as isize,
            (1.0 - ctx1.len() as f64 / blocked.len() as f64) * 100.0,
        );

        // Verify roundtrip
        let decoded = huffman_decode_context1(&ctx1);
        assert_eq!(tokens.len(), decoded.len(), "context1 corpus count mismatch");
        for (i, (o, d)) in tokens.iter().zip(decoded.iter()).enumerate() {
            if o != d {
                panic!("context1 corpus mismatch at {i}: orig={o:?}, dec={d:?}");
            }
        }
    }
}

#[cfg(test)]
mod split_tests {
    use super::*;

    #[test]
    fn test_split_roundtrip_basic() {
        let tokens = vec![
            Token::Literal(b'H'),
            Token::Literal(b'e'),
            Token::Literal(b'l'),
            Token::Literal(b'l'),
            Token::Literal(b'o'),
            Token::Match { offset: 5, length: 5 },
            Token::Literal(b'!'),
        ];
        let encoded = huffman_encode_split(&tokens);
        let decoded = huffman_decode_split(&encoded);
        assert_eq!(tokens, decoded);
    }

    #[test]
    fn test_split_roundtrip_large() {
        // 50K tokens with mixed literals and matches
        let tokens: Vec<Token> = (0..50_000u32).map(|i| {
            let r = i.wrapping_mul(2654435761);
            if r % 5 < 3 {
                Token::Literal((r % 256) as u8)
            } else if r % 5 == 3 {
                Token::Match { offset: (r % 65536 + 1), length: ((r % 20) as u16 + 4) }
            } else {
                Token::Match { offset: (r % 1000000 + 1), length: ((r % 100) as u16 + 4) }
            }
        }).collect();

        let encoded = huffman_encode_split(&tokens);
        let decoded = huffman_decode_split(&encoded);
        assert_eq!(tokens.len(), decoded.len(), "split count mismatch");
        for (i, (o, d)) in tokens.iter().zip(decoded.iter()).enumerate() {
            if o != d {
                panic!("split mismatch at {i}: orig={o:?}, dec={d:?}");
            }
        }
    }

    #[test]
    fn test_split_empty() {
        let tokens: Vec<Token> = vec![];
        let encoded = huffman_encode_split(&tokens);
        let decoded = huffman_decode_split(&encoded);
        assert_eq!(tokens, decoded);
    }

    #[test]
    fn test_split_single_token() {
        let tokens = vec![Token::Literal(b'X')];
        let encoded = huffman_encode_split(&tokens);
        let decoded = huffman_decode_split(&encoded);
        assert_eq!(tokens, decoded);
    }

    #[test]
    fn test_split_flag_set() {
        let tokens = vec![Token::Literal(b'A'), Token::Literal(b'B')];
        let encoded = huffman_encode_split(&tokens);
        let raw_n_blocks = u32::from_le_bytes([encoded[4], encoded[5], encoded[6], encoded[7]]);
        assert!(raw_n_blocks & SPLIT_RLE_FLAG != 0, "split RLE flag must be set");
    }

    #[test]
    fn test_split_vs_blocked_size() {
        // Tokens with varying statistics that should benefit from splitting
        let tokens: Vec<Token> = (0..50_000u32).map(|i| {
            let r = i.wrapping_mul(2654435761);
            if i < 25_000 {
                // First half: mostly literals
                if r % 10 == 0 {
                    Token::Match { offset: (r % 100 + 1), length: ((r % 10) as u16 + 3) }
                } else {
                    Token::Literal((r % 128) as u8)
                }
            } else {
                // Second half: mostly matches with large offsets
                if r % 10 == 0 {
                    Token::Literal((r % 256) as u8)
                } else {
                    Token::Match { offset: (r % 500000 + 1), length: ((r % 50) as u16 + 3) }
                }
            }
        }).collect();

        let blocked = huffman_encode_blocked(&tokens, 8192);
        let split = huffman_encode_split(&tokens);
        let decoded = huffman_decode_split(&split);
        assert_eq!(tokens.len(), decoded.len(), "split roundtrip count mismatch");
        for (i, (o, d)) in tokens.iter().zip(decoded.iter()).enumerate() {
            if o != d {
                panic!("split roundtrip mismatch at {i}: orig={o:?}, dec={d:?}");
            }
        }

        eprintln!(
            "split vs blocked: split={} bytes, blocked={} bytes, delta={} bytes ({:.2}%)",
            split.len(),
            blocked.len(),
            blocked.len() as isize - split.len() as isize,
            (1.0 - split.len() as f64 / blocked.len() as f64) * 100.0,
        );
    }

    #[test]
    fn test_rle_encode_lengths_basic() {
        // All zeros should use code 18 and 17
        let lengths = vec![0u8; 100];
        let rle = rle_encode_lengths(&lengths);
        // Decode back and verify
        let mut decoded = Vec::new();
        for &(sym, extra_count, extra_val) in &rle {
            match sym {
                0 => decoded.push(0),
                17 => {
                    let count = extra_val as usize + 3;
                    for _ in 0..count { decoded.push(0); }
                }
                18 => {
                    let count = extra_val as usize + 11;
                    for _ in 0..count { decoded.push(0); }
                }
                _ => panic!("unexpected symbol for all-zero input"),
            }
        }
        assert_eq!(lengths, decoded);
    }

    #[test]
    fn test_rle_encode_lengths_mixed() {
        let lengths = vec![5, 5, 5, 5, 5, 0, 0, 0, 0, 0, 3, 3, 3, 3, 7];
        let rle = rle_encode_lengths(&lengths);
        // Decode back
        let mut decoded = Vec::new();
        let mut prev = 0u8;
        for &(sym, _extra_count, extra_val) in &rle {
            match sym {
                0..=15 => {
                    decoded.push(sym);
                    prev = sym;
                }
                16 => {
                    let count = extra_val as usize + 3;
                    for _ in 0..count { decoded.push(prev); }
                }
                17 => {
                    let count = extra_val as usize + 3;
                    for _ in 0..count { decoded.push(0); }
                    prev = 0;
                }
                18 => {
                    let count = extra_val as usize + 11;
                    for _ in 0..count { decoded.push(0); }
                    prev = 0;
                }
                _ => unreachable!(),
            }
        }
        assert_eq!(lengths, decoded);
    }

    #[test]
    fn test_estimate_block_bits_nonzero() {
        let tokens = vec![
            Token::Literal(b'A'),
            Token::Literal(b'B'),
            Token::Literal(b'C'),
            Token::Match { offset: 3, length: 3 },
        ];
        let bits = estimate_block_bits(&tokens);
        assert!(bits > 0, "estimate should be positive for nonempty tokens");
    }
}
