//! BWT compression pipeline: BWT + context-mixing entropy coder
//!
//! Pipeline:
//!   compress:   data -> BWT (via SA-IS suffix array) -> context mixing (`bwt_cm`)
//!   decompress: context mixing -> inverse BWT
//!
//! The coder models the raw BWT output directly (order-0/1/2 counters plus
//! SSE), which beats MTF + RLE + Huffman by 5-19% on every block measured.

use super::bwt_cm;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// BWT block size. Matches the adaptive container's block size: larger blocks
/// sort more context together (-9% on a 3.4 MB novel vs 900 KB blocks).
pub const BWT_BLOCK_SIZE: usize = 4 * 1024 * 1024;

// ---------------------------------------------------------------------------
// BWT Forward (SA-IS suffix array, O(n) time)
// ---------------------------------------------------------------------------

/// SA-IS for byte arrays. Returns the suffix array of `text`.
///
/// Appends a sentinel (0) smaller than any data byte (shifted to 1..=256),
/// computes the suffix array, then drops the sentinel suffix, which sorts first.
fn suffix_array_sais(text: &[u8]) -> Vec<u32> {
    let n = text.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![0];
    }

    let mut t: Vec<u32> = text.iter().map(|&b| u32::from(b) + 1).collect();
    t.push(0); // sentinel
    let mut sa = sais_core(&t, 257); // alphabet = 256 values + sentinel
    debug_assert_eq!(sa[0] as usize, n);
    sa.remove(0);
    sa
}

/// Core SA-IS algorithm over an integer alphabet. `text` must end with a
/// unique smallest symbol. Buffers are released before recursing so peak
/// memory stays near 5 bytes per input symbol at the top level.
fn sais_core(text: &[u32], alphabet_size: usize) -> Vec<u32> {
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

    const EMPTY: u32 = u32::MAX;

    // 1. Classify S/L types
    let mut is_s = vec![false; n];
    is_s[n - 1] = true; // sentinel is always S-type
    for i in (0..n - 1).rev() {
        is_s[i] = text[i] < text[i + 1] || (text[i] == text[i + 1] && is_s[i + 1]);
    }

    // 2. Identify LMS positions
    let is_lms = |i: usize| -> bool { i > 0 && is_s[i] && !is_s[i - 1] };

    // 3. Bucket sizes
    let mut bucket_sizes = vec![0u32; alphabet_size];
    for &c in text {
        bucket_sizes[c as usize] += 1;
    }

    // Helper: get bucket starts (end=false) or ends (end=true)
    let get_buckets = |end: bool| -> Vec<u32> {
        let mut b = vec![0u32; alphabet_size];
        let mut sum = 0u32;
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

    // Place LMS positions (given in the order to seed) at their bucket tails.
    let seed_lms = |sa: &mut [u32], positions: &mut dyn Iterator<Item = usize>| {
        let mut tails = get_buckets(true);
        for pos in positions {
            let c = text[pos] as usize;
            tails[c] -= 1;
            sa[tails[c] as usize] = pos as u32;
        }
    };

    // Induce L-type suffixes left to right, then S-type right to left.
    let induce = |sa: &mut [u32]| {
        let mut heads = get_buckets(false);
        for i in 0..n {
            if sa[i] == EMPTY || sa[i] == 0 {
                continue;
            }
            let j = sa[i] as usize - 1;
            if !is_s[j] {
                let c = text[j] as usize;
                sa[heads[c] as usize] = j as u32;
                heads[c] += 1;
            }
        }
        let mut tails = get_buckets(true);
        for i in (0..n).rev() {
            if sa[i] == EMPTY || sa[i] == 0 {
                continue;
            }
            let j = sa[i] as usize - 1;
            if is_s[j] {
                let c = text[j] as usize;
                tails[c] -= 1;
                sa[tails[c] as usize] = j as u32;
            }
        }
    };

    // 4. Sort LMS substrings: seed LMS positions in text order and induce
    let mut sa = vec![EMPTY; n];
    seed_lms(&mut sa, &mut (0..n).rev().filter(|&i| is_lms(i)));
    induce(&mut sa);

    // 5. Name sorted LMS substrings
    let mut lms_names = vec![EMPTY; n];
    let mut name = 0u32;
    let mut prev = usize::MAX;

    for i in 0..n {
        let cur = sa[i] as usize;
        if !is_lms(cur) {
            continue;
        }
        // Compare LMS substring at cur with previous LMS substring
        let mut diff = prev == usize::MAX;
        if !diff {
            let (a, b) = (prev, cur);
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
        lms_names[cur] = name - 1;
        prev = cur;
    }
    drop(sa);

    // 6. Reduced string: LMS names in text order
    let lms_positions: Vec<u32> = (0..n).filter(|&i| is_lms(i)).map(|i| i as u32).collect();
    let reduced: Vec<u32> = lms_positions.iter().map(|&p| lms_names[p as usize]).collect();
    drop(lms_names);

    // 7. Solve reduced problem
    let reduced_sa = if (name as usize) < lms_positions.len() {
        sais_core(&reduced, name as usize)
    } else {
        // All names are unique, directly compute SA
        let mut sa_r = vec![0u32; reduced.len()];
        for (i, &r) in reduced.iter().enumerate() {
            sa_r[r as usize] = i as u32;
        }
        sa_r
    };
    drop(reduced);

    // 8. Final induction from the correctly ordered LMS suffixes
    let mut sa = vec![EMPTY; n];
    seed_lms(
        &mut sa,
        &mut reduced_sa.iter().rev().map(|&r| lms_positions[r as usize] as usize),
    );
    induce(&mut sa);
    sa
}

/// Compute the BWT of `data ++ sentinel` from its SA-IS suffix array.
///
/// Rows are the n + 1 sorted suffixes; row 0 is the sentinel suffix. The
/// sentinel itself is not emitted: the returned index is the row where it
/// would sit (the row of suffix 0), so the output has exactly n bytes.
///
/// Complexity: O(n) time and space.
fn bwt_forward(data: &[u8]) -> (Vec<u8>, u32) {
    let n = data.len();
    if n == 0 {
        return (Vec::new(), 0);
    }

    let sa = suffix_array_sais(data);

    let mut bwt = Vec::with_capacity(n);
    bwt.push(data[n - 1]); // row 0: char preceding the sentinel
    let mut index = 0u32;
    for (k, &s) in sa.iter().enumerate() {
        if s == 0 {
            index = k as u32 + 1;
        } else {
            bwt.push(data[s as usize - 1]);
        }
    }
    (bwt, index)
}

/// Inverse BWT using the LF-mapping (see `bwt_forward` for the row layout).
fn bwt_inverse(transformed: &[u8], index: u32) -> Vec<u8> {
    let n = transformed.len();
    if n == 0 {
        return Vec::new();
    }

    // First row of each byte in the sorted first column (row 0 is the sentinel).
    let mut counts = [0u32; 256];
    for &b in transformed {
        counts[b as usize] += 1;
    }
    let mut running = [0u32; 256];
    let mut sum = 1u32;
    for i in 0..256 {
        running[i] = sum;
        sum += counts[i];
    }

    // lf[j] = row reached from stored position j.
    let mut lf = vec![0u32; n];
    for (j, &b) in transformed.iter().enumerate() {
        lf[j] = running[b as usize];
        running[b as usize] += 1;
    }

    // Walk backwards from the sentinel row; rows past `index` are stored one slot earlier.
    let index = index as usize;
    let mut result = vec![0u8; n];
    let mut row = 0usize;
    for i in (0..n).rev() {
        let j = if row < index { row } else { row - 1 };
        result[i] = transformed[j];
        row = lf[j] as usize;
    }

    result
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compress data using BWT + context mixing.
///
/// Processes data in `BWT_BLOCK_SIZE` chunks. Output format:
/// ```text
/// [4B] original_length (u32 LE)
/// [4B] num_blocks (u32 LE)
/// For each block:
///   [4B] bwt_index (u32 LE)
///   [4B] block_size (u32 LE)
///   [4B] compressed_len (u32 LE)
///   [compressed_len bytes] context-mixing coded BWT output
/// ```
pub fn bwt_compress(data: &[u8]) -> Vec<u8> {
    let chunks: Vec<&[u8]> = data.chunks(BWT_BLOCK_SIZE).collect();

    let mut out = Vec::with_capacity(8 + data.len() / 2);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(chunks.len() as u32).to_le_bytes());

    for chunk in chunks {
        let (bwt_data, bwt_index) = bwt_forward(chunk);
        let coded = bwt_cm::encode(&bwt_data);
        out.extend_from_slice(&bwt_index.to_le_bytes());
        out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(&(coded.len() as u32).to_le_bytes());
        out.extend_from_slice(&coded);
    }

    out
}

/// Decompress BWT-compressed data. Stops at the first malformed block, so a
/// corrupt payload yields a short output instead of a panic.
pub fn bwt_decompress(payload: &[u8]) -> Vec<u8> {
    let read_u32 = |pos: usize| {
        payload
            .get(pos..pos.saturating_add(4))
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
    };
    let original_len = read_u32(0).unwrap_or(0);
    let num_blocks = read_u32(4).unwrap_or(0);

    let mut output = Vec::new();
    let mut pos = 8;
    for _ in 0..num_blocks {
        let (Some(bwt_index), Some(block_size), Some(compressed_len)) =
            (read_u32(pos), read_u32(pos + 4), read_u32(pos + 8))
        else {
            break;
        };
        pos += 12;
        let Some(compressed) = payload.get(pos..pos.saturating_add(compressed_len)) else {
            break;
        };
        pos += compressed_len;
        // A valid stream always has 1 <= bwt_index <= block_size <= BWT_BLOCK_SIZE
        // and never decodes past its declared length.
        if block_size > BWT_BLOCK_SIZE
            || bwt_index == 0
            || bwt_index > block_size
            || output.len() + block_size > original_len
        {
            break;
        }

        let bwt_data = bwt_cm::decode(compressed, block_size);
        output.extend_from_slice(&bwt_inverse(&bwt_data, bwt_index as u32));
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
            let sa_sais: Vec<usize> = suffix_array_sais(&text).into_iter().map(|x| x as usize).collect();
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
    fn test_decompress_never_exceeds_declared_length() {
        // Hostile payload: declares 1 byte but carries three 1000-byte block
        // headers with empty coded data, which the CM decoder expands anyway.
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&3u32.to_le_bytes());
        for _ in 0..3 {
            payload.extend_from_slice(&1u32.to_le_bytes()); // bwt_index
            payload.extend_from_slice(&1000u32.to_le_bytes()); // block_size
            payload.extend_from_slice(&0u32.to_le_bytes()); // compressed_len
        }
        assert!(bwt_decompress(&payload).len() <= 1);
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
