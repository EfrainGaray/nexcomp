//! BWT compression pipeline: BWT + MTF + RLE-zeros + multi-tree Huffman
//!
//! Targets bzip2-class compression on text data (~2.1 bpb on Calgary corpus).
//!
//! Pipeline:
//!   compress:   data -> BWT (via suffix array) -> MTF -> RLE zeros -> multi-tree Huffman
//!   decompress: multi-tree Huffman -> inverse RLE zeros -> inverse MTF -> inverse BWT
//!
//! The multi-tree Huffman approach (like bzip2) uses up to 6 Huffman trees per block,
//! selecting the best tree for every group of 50 symbols. This adapts to the
//! non-stationary symbol distribution in BWT output.

use crate::entropy::{
    build_decode_table as rans_build_decode_table, build_table, normalize_freqs, rans_decode,
    rans_encode,
};
use crate::lz77::huffman::{
    build_decode_table as huff_build_decode_table, build_huffman_codes, canonical_codes, BitReader,
    BitWriter, HuffCode,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// BWT block size — 900KB for better compression with SA-IS.
pub const BWT_BLOCK_SIZE: usize = 900 * 1024;

/// Alphabet size for rANS after RLE-zero encoding.
/// Symbols 0 is unused (zeros are RLE-encoded), 1-255 are literal MTF values,
/// 256 = RUNA, 257 = RUNB. Total 258, but we map to 0..257 for rANS (byte-level).
/// Since rANS works on u8, we split into two streams: a symbol stream and
/// a run-length stream. Actually, we use a simpler approach: flatten to bytes.
const RUNA: u16 = 256;
const RUNB: u16 = 257;

// ---------------------------------------------------------------------------
// BWT Forward (SA-IS suffix array, O(n) time)
// ---------------------------------------------------------------------------

/// SA-IS for byte arrays. Returns suffix array of length n.
///
/// Appends a sentinel byte (0) smaller than any data byte (shifted to 1..256),
/// computes the suffix array, then strips the sentinel position.
fn suffix_array_sais(text: &[u8]) -> Vec<usize> {
    let n = text.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![0];
    }

    // Convert to usize, shifting by +1 so sentinel (0) is smallest
    let mut t: Vec<usize> = text.iter().map(|&b| b as usize + 1).collect();
    t.push(0); // sentinel
    let sa = sais_core(&t, 257); // alphabet = 256 values + sentinel
    // Remove the sentinel position from result
    sa.into_iter().filter(|&x| x < n).collect()
}

/// Core SA-IS algorithm working on integer alphabets.
fn sais_core(text: &[usize], alphabet_size: usize) -> Vec<usize> {
    let n = text.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![0];
    }
    if n == 2 {
        return if text[0] < text[1] {
            vec![0, 1]
        } else {
            vec![1, 0]
        };
    }

    const EMPTY: usize = usize::MAX;

    // 1. Classify S/L types
    let mut is_s = vec![false; n];
    is_s[n - 1] = true; // sentinel is always S-type
    for i in (0..n - 1).rev() {
        is_s[i] = text[i] < text[i + 1] || (text[i] == text[i + 1] && is_s[i + 1]);
    }

    // 2. Identify LMS positions
    let is_lms = |i: usize| -> bool { i > 0 && is_s[i] && !is_s[i - 1] };

    // 3. Bucket sizes
    let mut bucket_sizes = vec![0usize; alphabet_size];
    for &c in text {
        bucket_sizes[c] += 1;
    }

    // Helper: get bucket starts (end=false) or ends (end=true)
    let get_buckets = |end: bool| -> Vec<usize> {
        let mut b = vec![0usize; alphabet_size];
        let mut sum = 0usize;
        for i in 0..alphabet_size {
            if end {
                sum += bucket_sizes[i];
                b[i] = sum; // one past end
            } else {
                b[i] = sum;
                sum += bucket_sizes[i];
            }
        }
        b
    };

    // 4. Initial placement of LMS suffixes at bucket tails
    let mut sa = vec![EMPTY; n];
    {
        let mut tails = get_buckets(true);
        for i in (0..n).rev() {
            if is_lms(i) {
                tails[text[i]] -= 1;
                sa[tails[text[i]]] = i;
            }
        }
    }

    // 5. Induce L-type from left to right
    {
        let mut heads = get_buckets(false);
        // Handle position 0 specially: if sa[i] == 0, predecessor is n-1 (but we don't wrap)
        // Actually SA-IS on linear text: if sa[i] == 0, there's no predecessor, skip.
        for i in 0..n {
            if sa[i] == EMPTY || sa[i] == 0 {
                continue;
            }
            let j = sa[i] - 1;
            if !is_s[j] {
                sa[heads[text[j]]] = j;
                heads[text[j]] += 1;
            }
        }
    }

    // 6. Induce S-type from right to left
    {
        let mut tails = get_buckets(true);
        for i in (0..n).rev() {
            if sa[i] == EMPTY || sa[i] == 0 {
                continue;
            }
            let j = sa[i] - 1;
            if is_s[j] {
                tails[text[j]] -= 1;
                sa[tails[text[j]]] = j;
            }
        }
    }

    // 7. Collect sorted LMS substrings and name them
    let mut lms_names = vec![EMPTY; n];
    let mut name = 0usize;
    let mut prev = EMPTY;

    for i in 0..n {
        if !is_lms(sa[i]) {
            continue;
        }
        // Compare LMS substring at sa[i] with previous LMS substring
        let mut diff = prev == EMPTY;
        if !diff {
            let (a, b) = (prev, sa[i]);
            // Compare character by character until we reach the end of both LMS substrings
            let mut k = 0;
            loop {
                if text[a + k] != text[b + k] || is_s[a + k] != is_s[b + k] {
                    diff = true;
                    break;
                }
                if k > 0 && (is_lms(a + k) || is_lms(b + k)) {
                    // Reached the end of both LMS substrings
                    break;
                }
                k += 1;
            }
        }
        if diff {
            name += 1;
        }
        lms_names[sa[i]] = name - 1;
        prev = sa[i];
    }

    // 8. Compact reduced string: collect LMS positions in text order
    let lms_positions: Vec<usize> = (0..n).filter(|&i| is_lms(i)).collect();
    let reduced: Vec<usize> = lms_positions.iter().map(|&p| lms_names[p]).collect();

    // 9. Solve reduced problem
    let reduced_sa = if name < lms_positions.len() {
        sais_core(&reduced, name)
    } else {
        // All names are unique, directly compute SA
        let mut sa_r = vec![0usize; reduced.len()];
        for (i, &r) in reduced.iter().enumerate() {
            sa_r[r] = i;
        }
        sa_r
    };

    // 10. Final induction using correctly ordered LMS suffixes
    let mut sa = vec![EMPTY; n];
    {
        let mut tails = get_buckets(true);
        for i in (0..reduced_sa.len()).rev() {
            let pos = lms_positions[reduced_sa[i]];
            tails[text[pos]] -= 1;
            sa[tails[text[pos]]] = pos;
        }
    }

    // Re-induce L-type
    {
        let mut heads = get_buckets(false);
        for i in 0..n {
            if sa[i] == EMPTY || sa[i] == 0 {
                continue;
            }
            let j = sa[i] - 1;
            if !is_s[j] {
                sa[heads[text[j]]] = j;
                heads[text[j]] += 1;
            }
        }
    }

    // Re-induce S-type
    {
        let mut tails = get_buckets(true);
        for i in (0..n).rev() {
            if sa[i] == EMPTY || sa[i] == 0 {
                continue;
            }
            let j = sa[i] - 1;
            if is_s[j] {
                tails[text[j]] -= 1;
                sa[tails[text[j]]] = j;
            }
        }
    }

    sa
}

/// Compute BWT using SA-IS suffix array on doubled text with sentinel.
///
/// To correctly sort circular rotations, we use the doubled-text approach:
/// construct `data ++ data ++ sentinel`, compute suffix array, and keep
/// only entries in [0, n). This ensures circular rotations are compared
/// correctly even when repeated patterns exist.
///
/// Complexity: O(n) time and space.
fn bwt_forward(data: &[u8]) -> (Vec<u8>, u32) {
    let n = data.len();
    if n == 0 {
        return (Vec::new(), 0);
    }

    // Build doubled text with sentinel: data ++ data ++ $ (length 2n + 1)
    // Shift all bytes by +1 so sentinel (0) is smallest.
    let mut text: Vec<usize> = Vec::with_capacity(2 * n + 1);
    for &b in data {
        text.push(b as usize + 1);
    }
    for &b in data {
        text.push(b as usize + 1);
    }
    text.push(0); // sentinel

    let sa = sais_core(&text, 257);

    // Extract BWT: only consider SA entries in [0, n) — these are the
    // n circular rotations. For each, the last column char is data[(s + n - 1) % n].
    let mut bwt = Vec::with_capacity(n);
    let mut index = 0u32;
    for &s in &sa {
        if s >= n {
            continue; // skip sentinel and second-half entries
        }
        if s == 0 {
            index = bwt.len() as u32;
            bwt.push(data[n - 1]);
        } else {
            bwt.push(data[s - 1]);
        }
    }
    (bwt, index)
}

/// Inverse BWT using the LF-mapping.
fn bwt_inverse(transformed: &[u8], index: u32) -> Vec<u8> {
    let n = transformed.len();
    if n == 0 {
        return Vec::new();
    }

    // Count occurrences of each byte.
    let mut counts = [0usize; 256];
    for &b in transformed {
        counts[b as usize] += 1;
    }

    // Cumulative counts.
    let mut cumul = [0usize; 256];
    let mut sum = 0usize;
    for i in 0..256 {
        cumul[i] = sum;
        sum += counts[i];
    }

    // Build LF-mapping.
    let mut lf = vec![0usize; n];
    let mut running = cumul;
    for i in 0..n {
        let b = transformed[i] as usize;
        lf[i] = running[b];
        running[b] += 1;
    }

    // Reconstruct original.
    let mut result = vec![0u8; n];
    let mut pos = index as usize;
    for i in (0..n).rev() {
        result[i] = transformed[pos];
        pos = lf[pos];
    }

    result
}

// ---------------------------------------------------------------------------
// Move-to-Front Transform
// ---------------------------------------------------------------------------

fn mtf_encode(data: &[u8]) -> Vec<u8> {
    let mut list: [u8; 256] = {
        let mut a = [0u8; 256];
        for i in 0..256 {
            a[i] = i as u8;
        }
        a
    };
    let mut output = Vec::with_capacity(data.len());

    for &b in data {
        // Find position of b in list
        let mut pos = 0usize;
        while list[pos] != b {
            pos += 1;
        }
        output.push(pos as u8);
        // Move to front
        if pos > 0 {
            let val = list[pos];
            // Shift elements right
            for j in (1..=pos).rev() {
                list[j] = list[j - 1];
            }
            list[0] = val;
        }
    }

    output
}

fn mtf_decode(data: &[u8]) -> Vec<u8> {
    let mut list: [u8; 256] = {
        let mut a = [0u8; 256];
        for i in 0..256 {
            a[i] = i as u8;
        }
        a
    };
    let mut output = Vec::with_capacity(data.len());

    for &pos in data {
        let pos = pos as usize;
        let b = list[pos];
        output.push(b);
        if pos > 0 {
            for j in (1..=pos).rev() {
                list[j] = list[j - 1];
            }
            list[0] = b;
        }
    }

    output
}

// ---------------------------------------------------------------------------
// RLE-zero encoding (RUNA/RUNB, bzip2-style)
// ---------------------------------------------------------------------------

/// Encode runs of zeros using RUNA/RUNB bijective binary encoding.
///
/// A run of N zeros is encoded as the bijective base-2 representation:
///   N=1 -> RUNA
///   N=2 -> RUNB
///   N=3 -> RUNA RUNA
///   N=4 -> RUNB RUNA
///   N=5 -> RUNA RUNB
///   etc.
///
/// Non-zero MTF values (1-255) pass through as symbols 1-255.
/// Symbol 0 never appears in output (zeros are always RLE-encoded).
fn rle_zeros_encode(data: &[u8]) -> Vec<u16> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;

    while i < data.len() {
        if data[i] == 0 {
            // Count run of zeros
            let mut run = 0u32;
            while i < data.len() && data[i] == 0 {
                run += 1;
                i += 1;
            }
            // Encode run using bijective base-2 (RUNA=0, RUNB=1)
            // Bijective numeration: digits are 1-indexed
            // run = sum of (digit_i + 1) * 2^i for i=0..
            // Decode: run = d0*(1) + d1*(2) + d2*(4) + ... where di in {1,2}
            // Encode: subtract powers of 2 from least significant
            let mut n = run;
            while n > 0 {
                n -= 1; // make zero-indexed
                if n & 1 == 0 {
                    out.push(RUNA);
                } else {
                    out.push(RUNB);
                }
                n >>= 1;
            }
        } else {
            // Non-zero MTF values: output as-is (they are 1-255)
            out.push(data[i] as u16);
            i += 1;
        }
    }

    out
}

/// Decode RUNA/RUNB back to runs of zeros, non-zero values pass through.
fn rle_zeros_decode(data: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;

    while i < data.len() {
        if data[i] == RUNA || data[i] == RUNB {
            // Decode bijective base-2 run length
            let mut run = 0u32;
            let mut power = 1u32;
            while i < data.len() && (data[i] == RUNA || data[i] == RUNB) {
                if data[i] == RUNA {
                    run += power;
                } else {
                    run += 2 * power;
                }
                power <<= 1;
                i += 1;
            }
            // Emit `run` zeros
            out.resize(out.len() + run as usize, 0);
        } else {
            out.push(data[i] as u8);
            i += 1;
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Symbol stream <-> byte stream conversion for rANS
// ---------------------------------------------------------------------------
// The RLE output uses symbols 1-255 (literals) and 256-257 (RUNA/RUNB).
// Total alphabet: 258 symbols (0 unused, 1-255 literals, 256=RUNA, 257=RUNB).
// But rANS works on u8 (0-255). We need to map to bytes.
//
// Strategy: use a two-byte escape encoding.
//   - Symbols 1-254: emit as byte value directly (offset by 0)
//   - Symbol 255: emit 255, 0
//   - Symbol 256 (RUNA): emit 0 (since literal 0 never appears after MTF+RLE)
//   - Symbol 257 (RUNB): emit 255, 1
//
// Wait - literal 0 never appears because zeros are RLE-encoded. So byte 0
// can represent RUNA. We need one more escape for RUNB.
//
// Better mapping (no escapes needed!):
//   - RUNA (256) -> byte 0
//   - RUNB (257) -> byte 1
//   - Literal MTF values 1-255 -> byte value + 1 (so 1->2, 2->3, ..., 254->255)
//
// Problem: literal 255 maps to 256 which doesn't fit in a byte.
//
// Use escape: byte 255 is an escape prefix.
//   - RUNA -> byte 0
//   - RUNB -> byte 1
//   - Literal 1 -> byte 2
//   - Literal 2 -> byte 3
//   - ...
//   - Literal 253 -> byte 254
//   - Literal 254 -> byte 255, byte 0
//   - Literal 255 -> byte 255, byte 1
//
// This means literal values 1-253 map to bytes 2-254 (single byte).
// Literal 254-255 use 2 bytes each. RUNA=0, RUNB=1.
// Since MTF output is dominated by small values, this is efficient.

fn symbols_to_bytes(symbols: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(symbols.len());
    for &s in symbols {
        match s {
            RUNA => out.push(0),  // 256 -> 0
            RUNB => out.push(1),  // 257 -> 1
            1..=253 => out.push(s as u8 + 1), // 1->2, 2->3, ..., 253->254
            254 => {
                out.push(255);
                out.push(0);
            }
            255 => {
                out.push(255);
                out.push(1);
            }
            _ => {} // symbol 0 should never appear
        }
    }
    out
}

fn bytes_to_symbols(data: &[u8]) -> Vec<u16> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        match b {
            0 => {
                out.push(RUNA);
                i += 1;
            }
            1 => {
                out.push(RUNB);
                i += 1;
            }
            2..=254 => {
                out.push((b - 1) as u16); // 2->1, 3->2, ..., 254->253
                i += 1;
            }
            255 => {
                i += 1;
                if i < data.len() {
                    match data[i] {
                        0 => out.push(254),
                        1 => out.push(255),
                        _ => out.push(254), // shouldn't happen
                    }
                    i += 1;
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// rANS compression of byte stream
// ---------------------------------------------------------------------------

/// Compress a byte stream with rANS, prepending the frequency table.
///
/// Output format: [2B alphabet_size][freq table: alphabet_size * 2B each][rANS payload]
fn rans_compress_block(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }

    // Count frequencies
    let mut counts = [0u64; 256];
    for &b in data {
        counts[b as usize] += 1;
    }

    // Normalize to sum=4096
    let freqs = normalize_freqs(&counts, 256);
    let table = build_table(&freqs).expect("normalized freqs must be valid");

    // Encode with rANS
    let encoded = rans_encode(data, &table).expect("rANS encode must succeed");

    // Build output: freq table (256 * 2 bytes) + encoded data
    let mut out = Vec::with_capacity(512 + encoded.len());

    // Store frequency table as 256 u16 values (each freq fits in u16 since max=4096)
    for &f in &freqs {
        out.extend_from_slice(&(f as u16).to_le_bytes());
    }

    // Store encoded data
    out.extend_from_slice(&encoded);
    out
}

/// Decompress a rANS-compressed block.
fn rans_decompress_block(data: &[u8], n_symbols: usize) -> Vec<u8> {
    if data.is_empty() || n_symbols == 0 {
        return Vec::new();
    }

    // Read frequency table (256 * 2 bytes = 512 bytes)
    let freq_table_size = 256 * 2;
    if data.len() < freq_table_size {
        return Vec::new();
    }

    let mut freqs = vec![0u32; 256];
    for i in 0..256 {
        let lo = data[i * 2] as u32;
        let hi = data[i * 2 + 1] as u32;
        freqs[i] = lo | (hi << 8);
    }

    let table = build_table(&freqs).expect("stored freq table must be valid");
    let dtable = rans_build_decode_table(&table);

    let payload = &data[freq_table_size..];
    rans_decode(payload, &dtable, n_symbols).expect("rANS decode must succeed")
}

// ---------------------------------------------------------------------------
// Multi-tree Huffman encoding (bzip2-style)
// ---------------------------------------------------------------------------

/// Symbols per group (bzip2 standard)
const GROUP_SIZE: usize = 50;

/// Maximum number of Huffman trees per block (bzip2 standard)
const MAX_TREES: usize = 6;

/// Version flag: legacy rANS encoding
const VERSION_RANS: u8 = 0x00;

/// Version flag: multi-tree Huffman encoding
const VERSION_MULTI_HUFFMAN: u8 = 0x01;

/// Compute histogram for a group of symbols.
fn group_histogram(symbols: &[u16], start: usize, end: usize, num_symbols: usize) -> Vec<u32> {
    let mut hist = vec![0u32; num_symbols];
    for &s in &symbols[start..end] {
        hist[s as usize] += 1;
    }
    hist
}

/// Estimate encoding cost of a group using a set of Huffman codes.
fn estimate_group_cost(group_hist: &[u32], codes: &[HuffCode]) -> usize {
    let mut cost = 0usize;
    for (sym, &count) in group_hist.iter().enumerate() {
        if count > 0 {
            let len = if sym < codes.len() && codes[sym].len > 0 {
                codes[sym].len as usize
            } else {
                15 // max code length as penalty for missing symbols
            };
            cost += count as usize * len;
        }
    }
    cost
}

/// Assign groups to trees using iterative refinement (simplified K-means).
/// Returns (assignments, tree_histograms).
fn assign_groups_to_trees(
    group_hists: &[Vec<u32>],
    num_trees: usize,
    num_symbols: usize,
) -> (Vec<usize>, Vec<Vec<u32>>) {
    let num_groups = group_hists.len();

    // Initial assignment: distribute groups evenly across trees
    let mut assignments: Vec<usize> = (0..num_groups)
        .map(|i| i * num_trees / num_groups)
        .collect();

    // Iterate: compute tree histograms, reassign groups to cheapest tree
    for _iter in 0..10 {
        // Build tree histograms from assignments
        let mut tree_hists = vec![vec![0u32; num_symbols]; num_trees];
        for (g, &t) in assignments.iter().enumerate() {
            for s in 0..num_symbols {
                tree_hists[t][s] += group_hists[g][s];
            }
        }

        // Build Huffman codes for each tree
        let tree_codes: Vec<Vec<HuffCode>> = tree_hists
            .iter()
            .map(|h| build_huffman_codes(h, num_symbols))
            .collect();

        // Reassign each group to the tree that encodes it cheapest
        let mut changed = false;
        for g in 0..num_groups {
            let mut best_tree = assignments[g];
            let mut best_cost = usize::MAX;
            for t in 0..num_trees {
                let cost = estimate_group_cost(&group_hists[g], &tree_codes[t]);
                if cost < best_cost {
                    best_cost = cost;
                    best_tree = t;
                }
            }
            if best_tree != assignments[g] {
                assignments[g] = best_tree;
                changed = true;
            }
        }

        if !changed {
            break;
        }
    }

    // Final tree histograms
    let mut tree_hists = vec![vec![0u32; num_symbols]; num_trees];
    for (g, &t) in assignments.iter().enumerate() {
        for s in 0..num_symbols {
            tree_hists[t][s] += group_hists[g][s];
        }
    }

    (assignments, tree_hists)
}

/// Choose optimal number of trees (2-6) by estimated total cost.
fn choose_num_trees(group_hists: &[Vec<u32>], num_symbols: usize) -> usize {
    if group_hists.len() <= 1 {
        return 1;
    }

    let max_k = MAX_TREES.min(group_hists.len());
    let mut best_k = 1;
    let mut best_total = usize::MAX;

    // Also evaluate k=1 (single tree)
    for k in 1..=max_k {
        let (assignments, tree_hists) =
            assign_groups_to_trees(group_hists, k, num_symbols);
        let tree_codes: Vec<Vec<HuffCode>> = tree_hists
            .iter()
            .map(|h| build_huffman_codes(h, num_symbols))
            .collect();

        // Total cost = encoded data + tree headers + selectors
        let mut data_bits = 0usize;
        for (g, &t) in assignments.iter().enumerate() {
            data_bits += estimate_group_cost(&group_hists[g], &tree_codes[t]);
        }
        let header_bits = k * num_symbols * 5; // ~5 bits per code length
        let selector_bits = if k > 1 {
            let sel_bits = if k <= 2 { 1 } else if k <= 4 { 2 } else { 3 };
            group_hists.len() * sel_bits
        } else {
            0
        };
        let total = data_bits + header_bits + selector_bits;

        if total < best_total {
            best_total = total;
            best_k = k;
        }
    }
    best_k
}

/// Multi-tree Huffman encode a symbol stream.
///
/// Format:
///   [3 bits] num_trees (1-6)
///   [16 bits] num_groups
///   [16 bits] num_symbols
///   [4 bits] total_symbol_count_bit_width (for last-group size)
///   [total_symbol_count_bit_width bits] total symbol count (modulo)
///   For each tree: num_symbols x 5-bit code lengths
///   If num_trees > 1: group selectors (ceil(log2(num_trees)) bits each)
///   Encoded data: each group's symbols encoded with assigned tree
fn multi_tree_encode(symbols: &[u16], max_symbol: usize) -> Vec<u8> {
    let num_symbols = max_symbol + 1;
    let num_groups = (symbols.len() + GROUP_SIZE - 1) / GROUP_SIZE;

    if symbols.is_empty() {
        let mut writer = BitWriter::new();
        writer.write_bits(1, 3); // 1 tree
        writer.write_bits(0, 16); // 0 groups
        writer.write_bits(num_symbols as u32, 16);
        writer.write_bits(0, 4); // 0-bit count
        return writer.finish();
    }

    // Compute per-group histograms
    let group_hists: Vec<Vec<u32>> = (0..num_groups)
        .map(|g| {
            let start = g * GROUP_SIZE;
            let end = (start + GROUP_SIZE).min(symbols.len());
            group_histogram(symbols, start, end, num_symbols)
        })
        .collect();

    // Choose number of trees and assign groups
    let num_trees = choose_num_trees(&group_hists, num_symbols);
    let (assignments, tree_hists) =
        assign_groups_to_trees(&group_hists, num_trees, num_symbols);

    // Build final Huffman codes
    let tree_codes: Vec<Vec<HuffCode>> = tree_hists
        .iter()
        .map(|h| build_huffman_codes(h, num_symbols))
        .collect();

    // Encode
    let mut writer = BitWriter::new();

    // Header
    writer.write_bits(num_trees as u32, 3);
    writer.write_bits(num_groups as u32, 16);
    writer.write_bits(num_symbols as u32, 16);

    // Store total symbol count so decoder knows the last group's size.
    // Use 20 bits (max ~1M symbols per 900KB block after RLE).
    writer.write_bits(symbols.len() as u32, 20);

    // Tree code lengths: for each tree, num_symbols x 5 bits
    for t in 0..num_trees {
        for s in 0..num_symbols {
            writer.write_bits(tree_codes[t][s].len as u32, 5);
        }
    }

    // Group selectors
    if num_trees > 1 {
        let sel_bits: u8 = if num_trees <= 2 {
            1
        } else if num_trees <= 4 {
            2
        } else {
            3
        };
        for &a in &assignments {
            writer.write_bits(a as u32, sel_bits);
        }
    }

    // Encoded data: for each group, encode symbols using assigned tree
    for g in 0..num_groups {
        let start = g * GROUP_SIZE;
        let end = (start + GROUP_SIZE).min(symbols.len());
        let t = assignments[g];
        for &sym in &symbols[start..end] {
            writer.write_code(&tree_codes[t][sym as usize]);
        }
    }

    writer.finish()
}

/// Multi-tree Huffman decode.
fn multi_tree_decode(data: &[u8]) -> Vec<u16> {
    let mut reader = BitReader::new(data);

    let num_trees = reader.read_bits(3) as usize;
    let num_groups = reader.read_bits(16) as usize;
    let num_symbols = reader.read_bits(16) as usize;
    let total_symbols = reader.read_bits(20) as usize;

    if num_groups == 0 || total_symbols == 0 {
        return Vec::new();
    }

    // Read tree code lengths and build decode tables
    let mut tree_tables = Vec::with_capacity(num_trees);
    for _ in 0..num_trees {
        let mut lengths = vec![0u8; num_symbols];
        for s in 0..num_symbols {
            lengths[s] = reader.read_bits(5) as u8;
        }
        let codes = canonical_codes(&lengths, num_symbols);
        tree_tables.push(huff_build_decode_table(&codes, num_symbols));
    }

    // Read group selectors
    let assignments: Vec<usize> = if num_trees > 1 {
        let sel_bits: u8 = if num_trees <= 2 {
            1
        } else if num_trees <= 4 {
            2
        } else {
            3
        };
        (0..num_groups)
            .map(|_| reader.read_bits(sel_bits) as usize)
            .collect()
    } else {
        vec![0; num_groups]
    };

    // Decode data
    let mut symbols = Vec::with_capacity(total_symbols);
    for g in 0..num_groups {
        let group_start = g * GROUP_SIZE;
        let count = if g == num_groups - 1 {
            total_symbols - group_start
        } else {
            GROUP_SIZE
        };
        let t = assignments[g];
        for _ in 0..count {
            let sym = reader.read_huffman(&tree_tables[t]);
            symbols.push(sym);
        }
    }

    symbols
}

// ---------------------------------------------------------------------------
// Public API: full BWT compression pipeline
// ---------------------------------------------------------------------------

/// Compress data using BWT + MTF + RLE-zeros + multi-tree Huffman.
///
/// Processes data in `BWT_BLOCK_SIZE` chunks. Output format:
/// ```text
/// [4B] original_length (u32 LE)
/// [4B] num_blocks (u32 LE)
/// For each block:
///   [1B] version flag (0x00=rANS, 0x01=multi-tree Huffman)
///   [4B] bwt_index (u32 LE)
///   [4B] original_block_size (u32 LE)
///   [4B] byte_stream_len (u32 LE) — length of byte stream (for rANS path)
///   [4B] compressed_len (u32 LE) — length of compressed payload
///   [compressed_len bytes] compressed data
/// ```
pub fn bwt_compress(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        let mut out = Vec::with_capacity(8);
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        return out;
    }

    let chunks: Vec<&[u8]> = data.chunks(BWT_BLOCK_SIZE).collect();
    let num_blocks = chunks.len() as u32;

    let mut out = Vec::with_capacity(8 + data.len());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&num_blocks.to_le_bytes());

    for chunk in chunks {
        // 1. Compute BWT via suffix array with sentinel
        let (bwt_data, bwt_index) = bwt_forward(chunk);

        // 2. Apply MTF
        let mtf_data = mtf_encode(&bwt_data);

        // 3. RLE zeros (RUNA/RUNB encoding)
        let rle_symbols = rle_zeros_encode(&mtf_data);

        // 4. Multi-tree Huffman encode (works directly on u16 symbol stream)
        // Symbol range: 1-255 for MTF values, 256=RUNA, 257=RUNB => max symbol 257
        let max_symbol = rle_symbols.iter().copied().max().unwrap_or(0) as usize;
        let huffman_compressed = multi_tree_encode(&rle_symbols, max_symbol);

        // 5. Also try rANS path and pick the smaller one
        let byte_stream = symbols_to_bytes(&rle_symbols);
        let byte_stream_len = byte_stream.len();
        let rans_compressed = rans_compress_block(&byte_stream);

        if huffman_compressed.len() <= rans_compressed.len() {
            // Use multi-tree Huffman
            out.push(VERSION_MULTI_HUFFMAN);
            out.extend_from_slice(&bwt_index.to_le_bytes());
            out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            out.extend_from_slice(&(byte_stream_len as u32).to_le_bytes());
            out.extend_from_slice(&(huffman_compressed.len() as u32).to_le_bytes());
            out.extend_from_slice(&huffman_compressed);
        } else {
            // Fallback to rANS
            out.push(VERSION_RANS);
            out.extend_from_slice(&bwt_index.to_le_bytes());
            out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            out.extend_from_slice(&(byte_stream_len as u32).to_le_bytes());
            out.extend_from_slice(&(rans_compressed.len() as u32).to_le_bytes());
            out.extend_from_slice(&rans_compressed);
        }
    }

    out
}

/// Decompress BWT-compressed data.
pub fn bwt_decompress(payload: &[u8]) -> Vec<u8> {
    if payload.len() < 8 {
        return Vec::new();
    }

    let original_len =
        u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
    let num_blocks =
        u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]) as usize;

    if original_len == 0 || num_blocks == 0 {
        return Vec::new();
    }

    let mut output = Vec::with_capacity(original_len);
    let mut pos = 8;

    for _ in 0..num_blocks {
        // Need at least version byte + 16 bytes header
        if pos + 17 > payload.len() {
            break;
        }

        let version = payload[pos];
        pos += 1;

        let bwt_index = u32::from_le_bytes([
            payload[pos],
            payload[pos + 1],
            payload[pos + 2],
            payload[pos + 3],
        ]);
        let _block_size = u32::from_le_bytes([
            payload[pos + 4],
            payload[pos + 5],
            payload[pos + 6],
            payload[pos + 7],
        ]) as usize;
        let byte_stream_len = u32::from_le_bytes([
            payload[pos + 8],
            payload[pos + 9],
            payload[pos + 10],
            payload[pos + 11],
        ]) as usize;
        let compressed_len = u32::from_le_bytes([
            payload[pos + 12],
            payload[pos + 13],
            payload[pos + 14],
            payload[pos + 15],
        ]) as usize;
        pos += 16;

        if pos + compressed_len > payload.len() {
            break;
        }

        let compressed_data = &payload[pos..pos + compressed_len];
        pos += compressed_len;

        let rle_symbols = match version {
            VERSION_MULTI_HUFFMAN => {
                // Multi-tree Huffman decode (directly to u16 symbols)
                multi_tree_decode(compressed_data)
            }
            _ => {
                // Legacy rANS path
                let byte_stream = rans_decompress_block(compressed_data, byte_stream_len);
                bytes_to_symbols(&byte_stream)
            }
        };

        // 3. Inverse RLE zeros
        let mtf_data = rle_zeros_decode(&rle_symbols);

        // 4. Inverse MTF
        let bwt_data = mtf_decode(&mtf_data);

        // 5. Inverse BWT
        let original_block = bwt_inverse(&bwt_data, bwt_index);
        output.extend_from_slice(&original_block);
    }

    output
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- BWT via SA tests --

    #[test]
    fn test_bwt_roundtrip_banana() {
        let data = b"banana";
        let (bwt, idx) = bwt_forward(data);
        let recovered = bwt_inverse(&bwt, idx);
        assert_eq!(&recovered, data);
    }

    #[test]
    fn test_bwt_roundtrip_various() {
        let cases: &[&[u8]] = &[
            b"a",
            b"ab",
            b"abracadabra",
            b"mississippi",
            b"the quick brown fox jumps over the lazy dog",
            b"aaaaaaa",
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
        ];
        for input in cases {
            let (bwt, idx) = bwt_forward(input);
            let recovered = bwt_inverse(&bwt, idx);
            assert_eq!(
                &recovered, input,
                "BWT roundtrip failed for {:?}",
                std::str::from_utf8(input).unwrap_or("<binary>")
            );
        }
    }

    // -- RLE zero encoding tests --

    #[test]
    fn test_rle_zeros_roundtrip() {
        let cases: &[&[u8]] = &[
            &[0],
            &[0, 0],
            &[0, 0, 0],
            &[0, 0, 0, 0, 0],
            &[1, 0, 0, 2, 0, 3],
            &[1, 2, 3, 4, 5],
            &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 2],
        ];
        for input in cases {
            let encoded = rle_zeros_encode(input);
            let decoded = rle_zeros_decode(&encoded);
            assert_eq!(
                &decoded, input,
                "RLE zeros roundtrip failed for {:?}",
                input
            );
        }
    }

    #[test]
    fn test_rle_zeros_long_run() {
        let input = vec![0u8; 10000];
        let encoded = rle_zeros_encode(&input);
        let decoded = rle_zeros_decode(&encoded);
        assert_eq!(decoded, input);
        // Should be much shorter than 10000 symbols
        assert!(
            encoded.len() < 30,
            "RLE of 10000 zeros should be compact, got {} symbols",
            encoded.len()
        );
    }

    // -- Symbol <-> byte stream conversion tests --

    #[test]
    fn test_symbol_byte_roundtrip() {
        // Test all symbol values
        let symbols: Vec<u16> = (1..=255)
            .chain(std::iter::once(RUNA))
            .chain(std::iter::once(RUNB))
            .collect();
        let bytes = symbols_to_bytes(&symbols);
        let recovered = bytes_to_symbols(&bytes);
        assert_eq!(recovered, symbols);
    }

    // -- Full pipeline tests --

    #[test]
    fn test_full_roundtrip_hello_world() {
        let input = b"Hello, World!";
        let compressed = bwt_compress(input);
        let decompressed = bwt_decompress(&compressed);
        assert_eq!(&decompressed, input);
    }

    #[test]
    fn test_full_roundtrip_empty() {
        let compressed = bwt_compress(b"");
        let decompressed = bwt_decompress(&compressed);
        assert!(decompressed.is_empty());
    }

    #[test]
    fn test_full_roundtrip_repeated() {
        let input = "abcdefghij".repeat(1000);
        let input = input.as_bytes();
        let compressed = bwt_compress(input);
        let decompressed = bwt_decompress(&compressed);
        assert_eq!(&decompressed, input);
    }

    #[test]
    fn test_full_roundtrip_english_text() {
        let sentences = [
            "The Burrows-Wheeler Transform groups similar characters together. ",
            "Move-to-front exploits locality by assigning small indices to recent symbols. ",
            "Combined with entropy coding, BWT achieves ratios competitive with bzip2. ",
            "Data compression reduces the number of bits needed to represent information. ",
            "Shannon entropy provides a theoretical lower bound on compression. ",
            "Lossless compression guarantees perfect reconstruction of original data. ",
            "The suffix array enables efficient BWT computation in linear time. ",
            "Run-length encoding of zeros after MTF dramatically improves compression. ",
        ];
        let mut text = String::new();
        let mut i = 0;
        while text.len() < 50_000 {
            text.push_str(sentences[i % sentences.len()]);
            i += 1;
        }
        let input = text.as_bytes();

        let compressed = bwt_compress(input);
        let decompressed = bwt_decompress(&compressed);
        assert_eq!(decompressed.len(), input.len());
        assert_eq!(&decompressed, input, "English text roundtrip failed");

        // Measure compression ratio
        let bpb = (compressed.len() as f64 * 8.0) / input.len() as f64;
        eprintln!(
            "BWT codec: {} bytes -> {} bytes, {:.3} bpb",
            input.len(),
            compressed.len(),
            bpb
        );
        // Should be significantly better than raw entropy (~4.5 bpb for English)
        assert!(
            bpb < 5.0,
            "BWT pipeline should compress English text below 5.0 bpb, got {:.3}",
            bpb
        );
    }

    #[test]
    fn test_full_roundtrip_all_bytes() {
        // All 256 byte values repeated
        let input: Vec<u8> = (0..=255u8).cycle().take(2560).collect();
        let compressed = bwt_compress(&input);
        let decompressed = bwt_decompress(&compressed);
        assert_eq!(decompressed, input);
    }

    #[test]
    fn test_full_roundtrip_single_char() {
        let input = vec![b'A'; 5000];
        let compressed = bwt_compress(&input);
        let decompressed = bwt_decompress(&compressed);
        assert_eq!(decompressed, input);
    }

    #[test]
    fn test_full_roundtrip_large_block() {
        // Test with data larger than BWT_BLOCK_SIZE to exercise multi-block
        let mut input = Vec::with_capacity(BWT_BLOCK_SIZE * 2 + 1000);
        let pattern = b"The quick brown fox jumps over the lazy dog. ";
        while input.len() < BWT_BLOCK_SIZE * 2 + 1000 {
            input.extend_from_slice(pattern);
        }

        let compressed = bwt_compress(&input);
        let decompressed = bwt_decompress(&compressed);
        assert_eq!(decompressed.len(), input.len());
        assert_eq!(decompressed, input, "Multi-block roundtrip failed");
    }

    #[test]
    fn test_compression_ratio_english() {
        // Larger English text sample for meaningful bpb measurement
        let text = "In computer science and information theory, data compression \
            involves encoding information using fewer bits than the original representation. \
            Compression can be either lossy or lossless. Lossless compression reduces bits \
            by identifying and eliminating statistical redundancy. No information is lost \
            in lossless compression. Lossy compression reduces bits by removing unnecessary \
            or less important information. The process of reducing the size of a data file \
            is referred to as data compression. In the context of data transmission, it is \
            called source coding. Encoding done before transmission means that the data \
            transfer rate is reduced. The Burrows-Wheeler Transform is particularly effective \
            for text compression because it tends to group identical characters together, \
            making the output highly compressible by subsequent stages like move-to-front \
            and entropy coding. ";
        let input = text.repeat(20);
        let input = input.as_bytes();

        let compressed = bwt_compress(input);
        let bpb = (compressed.len() as f64 * 8.0) / input.len() as f64;
        eprintln!(
            "Compression ratio test: {} -> {} bytes ({:.3} bpb)",
            input.len(),
            compressed.len(),
            bpb
        );

        // Verify roundtrip
        let decompressed = bwt_decompress(&compressed);
        assert_eq!(&decompressed, input);
    }

    // -- Calgary corpus test (if available) --

    #[test]
    fn test_calgary_text_if_available() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/english_sample.txt"
        );
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(_) => {
                eprintln!("Skipping Calgary test: {} not found", path);
                return;
            }
        };
        if data.is_empty() {
            eprintln!("Skipping: empty file");
            return;
        }

        let compressed = bwt_compress(&data);
        let decompressed = bwt_decompress(&compressed);
        assert_eq!(decompressed.len(), data.len());
        assert_eq!(decompressed, data, "Calgary text roundtrip failed");

        let bpb = (compressed.len() as f64 * 8.0) / data.len() as f64;
        eprintln!(
            "Calgary english_sample.txt: {} -> {} bytes ({:.3} bpb)",
            data.len(),
            compressed.len(),
            bpb
        );
    }

    // -- Pipeline stage isolation tests --

    #[test]
    fn test_pipeline_stages_english() {
        // Test each stage individually on English text (50KB to match failing test)
        let mut text = String::new();
        let sentences = [
            "The Burrows-Wheeler Transform groups similar characters together. ",
            "Move-to-front exploits locality by assigning small indices to recent symbols. ",
            "Combined with entropy coding, BWT achieves ratios competitive with bzip2. ",
            "Data compression reduces the number of bits needed to represent information. ",
            "Shannon entropy provides a theoretical lower bound on compression. ",
            "Lossless compression guarantees perfect reconstruction of original data. ",
            "The suffix array enables efficient BWT computation in linear time. ",
            "Run-length encoding of zeros after MTF dramatically improves compression. ",
        ];
        let mut i = 0;
        while text.len() < 50_000 {
            text.push_str(sentences[i % sentences.len()]);
            i += 1;
        }
        let input = text.as_bytes();

        // Stage 1: BWT
        let (bwt_data, bwt_index) = bwt_forward(input);
        let bwt_recovered = bwt_inverse(&bwt_data, bwt_index);
        assert_eq!(&bwt_recovered, input, "BWT stage failed");

        // Stage 2: MTF
        let mtf_data = mtf_encode(&bwt_data);
        let mtf_recovered = mtf_decode(&mtf_data);
        assert_eq!(mtf_recovered, bwt_data, "MTF stage failed");

        // Stage 3: RLE zeros
        let rle_symbols = rle_zeros_encode(&mtf_data);
        let rle_recovered = rle_zeros_decode(&rle_symbols);
        assert_eq!(rle_recovered, mtf_data, "RLE zeros stage failed");

        // Stage 4: Symbol-to-byte conversion
        let byte_stream = symbols_to_bytes(&rle_symbols);
        let sym_recovered = bytes_to_symbols(&byte_stream);
        assert_eq!(sym_recovered, rle_symbols, "Symbol-byte conversion failed");

        // Stage 5: rANS compression
        let compressed = rans_compress_block(&byte_stream);
        let rans_recovered = rans_decompress_block(&compressed, byte_stream.len());
        assert_eq!(rans_recovered, byte_stream, "rANS stage failed");

        // Full reverse pipeline from rans_recovered
        let sym_from_rans = bytes_to_symbols(&rans_recovered);
        let mtf_from_rle = rle_zeros_decode(&sym_from_rans);
        let bwt_from_mtf = mtf_decode(&mtf_from_rle);
        let original_from_bwt = bwt_inverse(&bwt_from_mtf, bwt_index);
        assert_eq!(&original_from_bwt, input, "Full pipeline reverse failed");
    }

    #[test]
    fn test_bwt_roundtrip_medium() {
        // Test BWT on the exact same data that the full pipeline test uses
        let sentences = [
            "The Burrows-Wheeler Transform groups similar characters together. ",
            "Move-to-front exploits locality by assigning small indices to recent symbols. ",
            "Combined with entropy coding, BWT achieves ratios competitive with bzip2. ",
            "Data compression reduces the number of bits needed to represent information. ",
            "Shannon entropy provides a theoretical lower bound on compression. ",
            "Lossless compression guarantees perfect reconstruction of original data. ",
            "The suffix array enables efficient BWT computation in linear time. ",
            "Run-length encoding of zeros after MTF dramatically improves compression. ",
        ];
        let mut text = String::new();
        let mut i = 0;
        while text.len() < 50_000 {
            text.push_str(sentences[i % sentences.len()]);
            i += 1;
        }
        let input = text.as_bytes();

        let (bwt, idx) = bwt_forward(input);
        let recovered = bwt_inverse(&bwt, idx);
        assert_eq!(
            recovered.len(),
            input.len(),
            "BWT roundtrip length mismatch"
        );
        assert_eq!(
            &recovered, input,
            "BWT roundtrip failed on 50KB multi-sentence text"
        );
    }

    // -- MTF isolated tests --

    #[test]
    fn test_mtf_roundtrip() {
        let inputs: &[&[u8]] = &[
            b"hello world",
            b"abcdefghijklmnop",
            b"aaaaabbbbbccccc",
            b"",
        ];
        for input in inputs {
            let encoded = mtf_encode(input);
            let decoded = mtf_decode(&encoded);
            assert_eq!(
                &decoded, *input,
                "MTF roundtrip failed for {:?}",
                std::str::from_utf8(input).unwrap_or("<binary>")
            );
        }
    }

    // -- SA-IS correctness test --

    #[test]
    fn test_sais_matches_naive_1000() {
        fn naive_sa(text: &[u8]) -> Vec<usize> {
            let n = text.len();
            let mut sa: Vec<usize> = (0..n).collect();
            sa.sort_by(|&a, &b| text[a..].cmp(&text[b..]));
            sa
        }

        let mut rng_state: u64 = 42;
        let next = |s: &mut u64| -> u64 {
            *s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
            *s >> 33
        };

        for _ in 0..1000 {
            let len = (next(&mut rng_state) % 200 + 1) as usize;
            let alpha = (next(&mut rng_state) % 10 + 2) as u8;
            let text: Vec<u8> = (0..len)
                .map(|_| (next(&mut rng_state) % alpha as u64) as u8)
                .collect();
            let sa_naive = naive_sa(&text);
            let sa_sais = suffix_array_sais(&text);
            assert_eq!(
                sa_naive, sa_sais,
                "MISMATCH on text of len {} alpha {}",
                len, alpha
            );
        }
    }

    #[test]
    fn test_calgary_corpus_bwt_all_files() {
        let dir = "/tmp/nexcomp_corpora/calgary";
        if !std::path::Path::new(dir).exists() {
            eprintln!("Calgary corpus not found at {}, skipping", dir);
            return;
        }

        let files = [
            "bib", "book1", "book2", "geo", "news", "obj1", "obj2",
            "paper1", "paper2", "paper3", "paper4", "paper5", "paper6",
            "pic", "progc", "progl", "progp", "trans",
        ];

        eprintln!("\n{:=<80}", "");
        eprintln!("  CALGARY CORPUS — BWT + Multi-tree Huffman");
        eprintln!("{:=<80}", "");
        eprintln!(
            "{:<12} {:>8} {:>8} {:>8}",
            "File", "Orig", "Compr", "bpb"
        );
        eprintln!("{:-<44}", "");

        let mut total_orig = 0usize;
        let mut total_comp = 0usize;
        let mut count = 0;

        for f in &files {
            let path = format!("{}/{}", dir, f);
            let data = match std::fs::read(&path) {
                Ok(d) => d,
                Err(_) => continue,
            };
            if data.is_empty() {
                continue;
            }

            let compressed = bwt_compress(&data);
            let decompressed = bwt_decompress(&compressed);
            assert_eq!(
                decompressed.len(),
                data.len(),
                "Roundtrip length mismatch for {}",
                f
            );
            assert_eq!(decompressed, data, "Roundtrip data mismatch for {}", f);

            let bpb = (compressed.len() as f64 * 8.0) / data.len() as f64;
            eprintln!(
                "{:<12} {:>8} {:>8} {:>8.3}",
                f,
                data.len(),
                compressed.len(),
                bpb
            );

            total_orig += data.len();
            total_comp += compressed.len();
            count += 1;
        }

        if count > 0 {
            let avg_bpb = (total_comp as f64 * 8.0) / total_orig as f64;
            eprintln!("{:-<44}", "");
            eprintln!(
                "{:<12} {:>8} {:>8} {:>8.3}",
                "TOTAL", total_orig, total_comp, avg_bpb
            );
            eprintln!(
                "\nbzip2-9 reference: ~2.109 bpb on Calgary"
            );
        } else {
            eprintln!("No Calgary files found");
        }
    }

    #[test]
    fn test_multi_tree_huffman_roundtrip() {
        // Direct test of multi-tree encode/decode
        let symbols: Vec<u16> = vec![
            RUNA, RUNB, 1, 2, 3, RUNA, RUNA, RUNA, 5, 10, 20, 1, 1, 1, 1, 1,
            RUNB, RUNB, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
            1, 1, 1, 1, RUNA, RUNA, RUNB, 2, 3, 4, 5, 6, 7, 8, 9, 10,
            20, 30, 40, 50, 60, 70, RUNA, RUNB, 1, 2, 3, 4, 5,
        ];
        let max_sym = *symbols.iter().max().unwrap() as usize;
        let encoded = multi_tree_encode(&symbols, max_sym);
        let decoded = multi_tree_decode(&encoded);
        assert_eq!(decoded, symbols, "Multi-tree Huffman roundtrip failed");
    }

    #[test]
    fn test_multi_tree_huffman_large() {
        // Simulate BWT-like output: mostly small MTF values with RUNA/RUNB
        let mut symbols = Vec::new();
        for i in 0..2000 {
            if i % 5 == 0 {
                symbols.push(RUNA);
            } else if i % 7 == 0 {
                symbols.push(RUNB);
            } else {
                symbols.push((i % 30 + 1) as u16);
            }
        }
        let max_sym = *symbols.iter().max().unwrap() as usize;
        let encoded = multi_tree_encode(&symbols, max_sym);
        let decoded = multi_tree_decode(&encoded);
        assert_eq!(decoded, symbols, "Large multi-tree Huffman roundtrip failed");
    }

    #[test]
    fn test_single_block_for_small_files() {
        // Verify that files smaller than BWT_BLOCK_SIZE are compressed as a single BWT block.
        // The header format is: [4B orig_len][4B num_blocks]...
        // For files <= BWT_BLOCK_SIZE, num_blocks must be 1.
        let sizes = [1000, 4096, 65536, 71646, 100_000, BWT_BLOCK_SIZE - 1, BWT_BLOCK_SIZE];
        for &size in &sizes {
            let input: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            let compressed = bwt_compress(&input);

            // Read num_blocks from header (bytes 4..8)
            let num_blocks = u32::from_le_bytes([
                compressed[4], compressed[5], compressed[6], compressed[7],
            ]);
            assert_eq!(
                num_blocks, 1,
                "Expected 1 block for {} byte input, got {}",
                size, num_blocks
            );

            // Verify roundtrip
            let decompressed = bwt_decompress(&compressed);
            assert_eq!(decompressed, input, "Roundtrip failed for {} byte input", size);
        }
    }

    #[test]
    fn test_multi_block_for_large_files() {
        // Verify that files larger than BWT_BLOCK_SIZE produce multiple blocks.
        let size = BWT_BLOCK_SIZE + 1;
        let input: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        let compressed = bwt_compress(&input);

        let num_blocks = u32::from_le_bytes([
            compressed[4], compressed[5], compressed[6], compressed[7],
        ]);
        assert_eq!(
            num_blocks, 2,
            "Expected 2 blocks for {} byte input (BWT_BLOCK_SIZE+1), got {}",
            size, num_blocks
        );

        let decompressed = bwt_decompress(&compressed);
        assert_eq!(decompressed, input);
    }
}
