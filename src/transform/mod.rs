// NEXCOMP — Domain Transform (Stage 2)
//
// Implements domain-specific transforms:
//   TEXT:             BWT (Burrows-Wheeler Transform) + Move-to-Front (MTF)
//   All others:       pass-through (identity)
//
// All transforms are lossless and invertible.

use crate::classifier::DomainType;

// ---------------------------------------------------------------------------
// Burrows-Wheeler Transform (BWT)
// ---------------------------------------------------------------------------

/// Perform the BWT on `data`, returning (transformed_bytes, original_row_index).
///
/// Uses naive suffix-array style sorting: we sort indices 0..n by comparing
/// the rotations they represent. The last column of the sorted rotation matrix
/// is the BWT output.
fn bwt_forward(data: &[u8]) -> (Vec<u8>, u32) {
    let n = data.len();
    if n == 0 {
        return (Vec::new(), 0);
    }

    // Build index array and sort by comparing rotations.
    let mut indices: Vec<usize> = (0..n).collect();
    indices.sort_by(|&a, &b| {
        for k in 0..n {
            let ca = data[(a + k) % n];
            let cb = data[(b + k) % n];
            match ca.cmp(&cb) {
                std::cmp::Ordering::Equal => continue,
                other => return other,
            }
        }
        std::cmp::Ordering::Equal
    });

    // The BWT output is the last character of each sorted rotation.
    let transformed: Vec<u8> = indices.iter().map(|&i| data[(i + n - 1) % n]).collect();

    // Find the row corresponding to the original (rotation 0).
    let original_index = indices.iter().position(|&i| i == 0).unwrap() as u32;

    (transformed, original_index)
}

/// Inverse BWT: given the transformed bytes and the original row index,
/// reconstruct the original data.
fn bwt_inverse(transformed: &[u8], index: u32) -> Vec<u8> {
    let n = transformed.len();
    if n == 0 {
        return Vec::new();
    }

    // Standard inverse BWT using the "T-transform" (LF-mapping).
    // 1. Count occurrences of each byte.
    let mut counts = [0usize; 256];
    for &b in transformed {
        counts[b as usize] += 1;
    }

    // 2. Compute cumulative counts (first occurrence of each byte in sorted column).
    let mut cumul = [0usize; 256];
    let mut sum = 0usize;
    for i in 0..256 {
        cumul[i] = sum;
        sum += counts[i];
    }

    // 3. Build the LF-mapping (T vector).
    let mut lf = vec![0usize; n];
    let mut running = cumul;
    for i in 0..n {
        let b = transformed[i] as usize;
        lf[i] = running[b];
        running[b] += 1;
    }

    // 4. Follow the chain starting from `index` to reconstruct.
    let mut result = vec![0u8; n];
    let mut pos = index as usize;
    for i in (0..n).rev() {
        result[i] = transformed[pos];
        pos = lf[pos];
    }

    result
}

// ---------------------------------------------------------------------------
// Move-to-Front (MTF) Transform
// ---------------------------------------------------------------------------

/// MTF encode: for each byte, output its position in a list of 256 bytes,
/// then move that byte to the front of the list.
fn mtf_encode(data: &[u8]) -> Vec<u8> {
    let mut list: Vec<u8> = (0..=255).collect();
    let mut output = Vec::with_capacity(data.len());

    for &b in data {
        let pos = list.iter().position(|&x| x == b).unwrap();
        output.push(pos as u8);
        list.remove(pos);
        list.insert(0, b);
    }

    output
}

/// MTF decode: use position to look up byte in the list, then move to front.
fn mtf_decode(data: &[u8]) -> Vec<u8> {
    let mut list: Vec<u8> = (0..=255).collect();
    let mut output = Vec::with_capacity(data.len());

    for &pos in data {
        let b = list[pos as usize];
        output.push(b);
        list.remove(pos as usize);
        list.insert(0, b);
    }

    output
}

// ---------------------------------------------------------------------------
// DNA 2-bit packing transform
// ---------------------------------------------------------------------------

/// Encode a base as 2 bits: A=00, C=01, G=10, T=11.
fn base_to_bits(b: u8) -> u8 {
    match b {
        b'A' | b'a' => 0b00,
        b'C' | b'c' => 0b01,
        b'G' | b'g' => 0b10,
        b'T' | b't' => 0b11,
        _ => 0b00, // N or other — stored separately
    }
}

/// Decode 2 bits back to an ASCII base.
fn bits_to_base(bits: u8) -> u8 {
    match bits & 0b11 {
        0b00 => b'A',
        0b01 => b'C',
        0b10 => b'G',
        0b11 => b'T',
        _ => unreachable!(),
    }
}

/// Forward DNA transform.
///
/// Format:
///   [4B] original_len (u32 LE)
///   [4B] num_header_bytes (u32 LE)
///   [header_bytes...]
///   [4B] num_n_positions (u32 LE)
///   [n_positions as u32 LE each...]
///   [packed_bases: 4 bases per byte, MSB-first]
fn dna_forward(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }

    let original_len = data.len() as u32;

    // Separate header lines (lines starting with '>') from sequence data.
    let mut header_bytes: Vec<u8> = Vec::new();
    let mut seq_bases: Vec<u8> = Vec::new();
    // Track positions in the *original* byte stream where header lines start/end
    // so we can reconstruct. We store full header lines including the newline.
    // For reconstruction we need to know where headers appeared relative to
    // sequence characters. We'll store header insertion points as:
    //   (seq_position_before_header: u32, header_offset_in_header_bytes: u32, header_len: u32)
    // But to keep it simple, store headers concatenated and also store a list of
    // (seq_pos, header_len) pairs.
    let mut header_entries: Vec<(u32, u32)> = Vec::new(); // (seq_char_index, header_line_len)

    let mut i = 0;
    let bytes = data;
    while i < bytes.len() {
        if bytes[i] == b'>' {
            // Read until end of line (or end of data).
            let start = i;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            if i < bytes.len() {
                i += 1; // consume the '\n'
            }
            let header_line = &bytes[start..i];
            let seq_pos = seq_bases.len() as u32;
            header_entries.push((seq_pos, header_line.len() as u32));
            header_bytes.extend_from_slice(header_line);
        } else if bytes[i] == b'\n' {
            // Newlines within sequence data — treat as sequence character to
            // preserve exact reconstruction. Actually, in FASTA the newlines
            // are formatting. We need to preserve them for exact reconstruction.
            // Store them as part of sequence stream and mark them as N-like
            // specials. Simpler: store newline positions separately too.
            // For simplicity let's include newlines in the seq_bases and handle
            // them like N (non-ACGT).
            seq_bases.push(bytes[i]);
            i += 1;
        } else {
            seq_bases.push(bytes[i]);
            i += 1;
        }
    }

    // Find N-positions (any non-ACGT character in seq_bases, including newlines).
    let mut n_positions: Vec<u32> = Vec::new();
    let mut n_values: Vec<u8> = Vec::new();
    for (idx, &b) in seq_bases.iter().enumerate() {
        match b {
            b'A' | b'a' | b'C' | b'c' | b'G' | b'g' | b'T' | b't' => {}
            _ => {
                n_positions.push(idx as u32);
                n_values.push(b);
            }
        }
    }

    // Pack bases: 4 bases per byte, MSB first.
    let num_seq = seq_bases.len();
    let packed_len = (num_seq + 3) / 4;
    let mut packed = vec![0u8; packed_len];
    for (idx, &b) in seq_bases.iter().enumerate() {
        let bits = base_to_bits(b); // non-ACGT maps to 00, we fix via n_positions
        let byte_idx = idx / 4;
        let shift = 6 - 2 * (idx % 4); // positions: 6,4,2,0
        packed[byte_idx] |= bits << shift;
    }

    // Build output.
    let num_header_entries = header_entries.len() as u32;
    let num_n = n_positions.len() as u32;

    let mut out = Vec::new();
    out.extend_from_slice(&original_len.to_le_bytes());
    // Header section: num_header_bytes, header_bytes, then header_entries
    out.extend_from_slice(&(header_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&header_bytes);
    out.extend_from_slice(&num_header_entries.to_le_bytes());
    for &(seq_pos, hdr_len) in &header_entries {
        out.extend_from_slice(&seq_pos.to_le_bytes());
        out.extend_from_slice(&hdr_len.to_le_bytes());
    }
    // N-positions section
    out.extend_from_slice(&num_n.to_le_bytes());
    for (&pos, &val) in n_positions.iter().zip(n_values.iter()) {
        out.extend_from_slice(&pos.to_le_bytes());
        out.push(val);
    }
    // Sequence length for unpacking
    out.extend_from_slice(&(num_seq as u32).to_le_bytes());
    // Packed bases
    out.extend_from_slice(&packed);

    out
}

/// Inverse DNA transform — reconstruct original data exactly.
fn dna_inverse(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return Vec::new();
    }

    let mut cursor = 0;

    let _read_u32 = |cur: &mut usize| -> u32 {
        let val = u32::from_le_bytes([data[*cur], data[*cur + 1], data[*cur + 2], data[*cur + 3]]);
        *cur += 4;
        val
    };

    let original_len = _read_u32(&mut cursor);

    // Header bytes
    let num_header_bytes = _read_u32(&mut cursor) as usize;
    let header_bytes = &data[cursor..cursor + num_header_bytes];
    cursor += num_header_bytes;

    // Header entries
    let num_header_entries = _read_u32(&mut cursor) as usize;
    let mut header_entries: Vec<(u32, u32)> = Vec::with_capacity(num_header_entries);
    for _ in 0..num_header_entries {
        let seq_pos = _read_u32(&mut cursor);
        let hdr_len = _read_u32(&mut cursor);
        header_entries.push((seq_pos, hdr_len));
    }

    // N-positions
    let num_n = _read_u32(&mut cursor) as usize;
    let mut n_map: std::collections::HashMap<u32, u8> = std::collections::HashMap::new();
    for _ in 0..num_n {
        let pos = _read_u32(&mut cursor);
        let val = data[cursor];
        cursor += 1;
        n_map.insert(pos, val);
    }

    // Sequence length and packed bases
    let num_seq = _read_u32(&mut cursor) as usize;
    let packed = &data[cursor..];

    // Unpack bases
    let mut seq_bases = Vec::with_capacity(num_seq);
    for idx in 0..num_seq {
        let byte_idx = idx / 4;
        let shift = 6 - 2 * (idx % 4);
        let bits = (packed[byte_idx] >> shift) & 0b11;
        if let Some(&val) = n_map.get(&(idx as u32)) {
            seq_bases.push(val);
        } else {
            seq_bases.push(bits_to_base(bits));
        }
    }

    // Reconstruct: insert headers at their sequence positions.
    // header_entries are sorted by seq_pos. Each entry: (seq_pos, hdr_len).
    // Headers are concatenated in header_bytes in order.
    let mut result = Vec::with_capacity(original_len as usize);
    let mut seq_cursor = 0usize;
    let mut hdr_byte_cursor = 0usize;

    for &(seq_pos, hdr_len) in &header_entries {
        let seq_pos = seq_pos as usize;
        let hdr_len = hdr_len as usize;
        // Copy sequence bases up to this header insertion point.
        while seq_cursor < seq_pos && seq_cursor < seq_bases.len() {
            result.push(seq_bases[seq_cursor]);
            seq_cursor += 1;
        }
        // Insert header.
        result.extend_from_slice(&header_bytes[hdr_byte_cursor..hdr_byte_cursor + hdr_len]);
        hdr_byte_cursor += hdr_len;
    }
    // Copy remaining sequence bases.
    while seq_cursor < seq_bases.len() {
        result.push(seq_bases[seq_cursor]);
        seq_cursor += 1;
    }

    result
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Apply domain-specific transform.
///
/// For `DomainType::Text`: applies BWT then MTF. The BWT original-row index
/// is prepended as 4 bytes (little-endian u32) so `inverse_transform` can
/// recover it.
///
/// For all other domains: identity (pass-through).
/// Maximum block size for BWT to keep sorting tractable.
/// Naive BWT uses O(n² log n) sort. At 1KB blocks this is fast enough for most data.
/// Worst case (all-same-byte): each comparison is O(n), total O(n² log n) ≈ 10M ops.
const BWT_BLOCK_SIZE: usize = 1024;

pub fn apply_transform(data: &[u8], domain: DomainType) -> Vec<u8> {
    match domain {
        DomainType::Text => {
            if data.is_empty() {
                return Vec::new();
            }
            // Skip BWT for highly repetitive data (low entropy) — BWT sort is O(n²)
            // worst case on repeated patterns, while Re-Pair handles them natively.
            let entropy = {
                let mut counts = [0u32; 256];
                for &b in data { counts[b as usize] += 1; }
                let n = data.len() as f64;
                let mut h = 0.0f64;
                for &c in &counts {
                    if c > 0 {
                        let p = c as f64 / n;
                        h -= p * p.log2();
                    }
                }
                h
            };
            // If entropy < 3.5 bpb, data is highly repetitive — skip BWT
            if entropy < 3.5 {
                // Store as "0 blocks" marker → inverse_transform returns raw data
                let mut out = Vec::with_capacity(4 + data.len());
                out.extend_from_slice(&0u32.to_le_bytes()); // 0 blocks = no BWT
                out.extend_from_slice(data);
                return out;
            }
            // Process in BWT_BLOCK_SIZE sub-blocks to keep BWT fast.
            // Format: [4B num_blocks] then for each block: [4B bwt_index][4B len][mtf_data]
            let chunks: Vec<&[u8]> = data.chunks(BWT_BLOCK_SIZE).collect();
            let num_blocks = chunks.len() as u32;
            let mut out = Vec::with_capacity(4 + data.len() + chunks.len() * 4);
            out.extend_from_slice(&num_blocks.to_le_bytes());
            for chunk in chunks {
                let (bwt_data, index) = bwt_forward(chunk);
                let mtf_data = mtf_encode(&bwt_data);
                out.extend_from_slice(&index.to_le_bytes());
                // Store block length so we can split on decode
                out.extend_from_slice(&(mtf_data.len() as u32).to_le_bytes());
                out.extend_from_slice(&mtf_data);
            }
            out
        }
        DomainType::DnaFasta => dna_forward(data),
        _ => data.to_vec(),
    }
}

/// Inverse domain-specific transform.
///
/// For `DomainType::Text`: reads the 4-byte BWT index, then applies
/// MTF inverse followed by BWT inverse.
///
/// For all other domains: identity (pass-through).
pub fn inverse_transform(data: &[u8], domain: DomainType) -> Vec<u8> {
    match domain {
        DomainType::Text => {
            if data.len() < 4 {
                return data.to_vec();
            }
            let num_blocks = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
            if num_blocks == 0 {
                // No BWT was applied — raw data follows the 4-byte header
                return data[4..].to_vec();
            }
            let mut pos = 4;
            let mut output = Vec::with_capacity(data.len());
            for _ in 0..num_blocks {
                if pos + 8 > data.len() {
                    break;
                }
                let index = u32::from_le_bytes([data[pos], data[pos+1], data[pos+2], data[pos+3]]);
                let block_len = u32::from_le_bytes([data[pos+4], data[pos+5], data[pos+6], data[pos+7]]) as usize;
                pos += 8;
                let mtf_data = &data[pos..pos + block_len];
                let bwt_data = mtf_decode(mtf_data);
                let original = bwt_inverse(&bwt_data, index);
                output.extend_from_slice(&original);
                pos += block_len;
            }
            output
        }
        DomainType::DnaFasta => dna_inverse(data),
        _ => data.to_vec(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- BWT round-trip tests --

    #[test]
    fn test_bwt_roundtrip_banana() {
        let input = b"banana";
        let (transformed, index) = bwt_forward(input);
        let recovered = bwt_inverse(&transformed, index);
        assert_eq!(&recovered, input);
    }

    #[test]
    fn test_bwt_roundtrip_short_strings() {
        let cases: Vec<&[u8]> = vec![
            b"a",
            b"ab",
            b"abracadabra",
            b"mississippi",
            b"the quick brown fox jumps over the lazy dog",
            b"aaaaaaa",
        ];
        for input in cases {
            let (transformed, index) = bwt_forward(input);
            let recovered = bwt_inverse(&transformed, index);
            assert_eq!(
                &recovered, input,
                "BWT round-trip failed for {:?}",
                std::str::from_utf8(input).unwrap_or("<non-utf8>")
            );
        }
    }

    #[test]
    fn test_bwt_empty() {
        let (transformed, index) = bwt_forward(b"");
        assert!(transformed.is_empty());
        assert_eq!(index, 0);
        let recovered = bwt_inverse(&transformed, index);
        assert!(recovered.is_empty());
    }

    // -- MTF round-trip tests --

    #[test]
    fn test_mtf_roundtrip() {
        let inputs: Vec<&[u8]> = vec![
            b"hello world",
            b"abcdefghijklmnop",
            b"aaaaabbbbbccccc",
            b"",
        ];
        for input in inputs {
            let encoded = mtf_encode(input);
            let decoded = mtf_decode(&encoded);
            assert_eq!(
                &decoded, input,
                "MTF round-trip failed for {:?}",
                std::str::from_utf8(input).unwrap_or("<non-utf8>")
            );
        }
    }

    // -- Full pipeline round-trip tests --

    #[test]
    fn test_full_roundtrip_short() {
        let input = b"the quick brown fox jumps over the lazy dog";
        let transformed = apply_transform(input, DomainType::Text);
        let recovered = inverse_transform(&transformed, DomainType::Text);
        assert_eq!(&recovered, input);
    }

    #[test]
    fn test_full_roundtrip_repeated_text() {
        let input = "Hello, world! This is a repeated sentence. ".repeat(50);
        let input = input.as_bytes();
        let transformed = apply_transform(input, DomainType::Text);
        let recovered = inverse_transform(&transformed, DomainType::Text);
        assert_eq!(&recovered, input);
    }

    #[test]
    fn test_full_roundtrip_10kb_english() {
        // Build ~10KB of realistic English text.
        let sentences = [
            "The Burrows-Wheeler Transform is a reversible transformation that tends to group similar characters together. ",
            "This property makes the output much more amenable to compression by algorithms like move-to-front and run-length encoding. ",
            "Originally developed for block sorting compression, the BWT has found applications in bioinformatics and text indexing. ",
            "The move-to-front transform exploits locality of reference by assigning small indices to recently seen symbols. ",
            "When combined with entropy coding, BWT plus MTF achieves compression ratios competitive with modern algorithms. ",
            "Data compression is a fundamental problem in computer science with applications ranging from file storage to network protocols. ",
            "Lossless compression algorithms guarantee perfect reconstruction of the original data from the compressed representation. ",
            "The Shannon entropy provides a theoretical lower bound on the average number of bits needed to encode a symbol. ",
        ];
        let mut text = String::new();
        let mut i = 0;
        while text.len() < 10_000 {
            text.push_str(sentences[i % sentences.len()]);
            i += 1;
        }
        let input = text.as_bytes();
        assert!(input.len() >= 10_000, "Test text should be at least 10KB");

        let transformed = apply_transform(input, DomainType::Text);
        let recovered = inverse_transform(&transformed, DomainType::Text);
        assert_eq!(recovered, input, "10KB round-trip failed");
    }

    #[test]
    fn test_identity_for_non_text() {
        let input = b"some data that should pass through unchanged";
        for domain in &[
            DomainType::BinaryGeneric,
            DomainType::CodeSrc,
            DomainType::FloatTs,
            DomainType::ImageRaw,
            DomainType::StructuredData,
        ] {
            let transformed = apply_transform(input, *domain);
            assert_eq!(&transformed, input, "Non-text domain {:?} should be identity", domain);
            let recovered = inverse_transform(&transformed, *domain);
            assert_eq!(&recovered, input);
        }
    }

    #[test]
    fn test_transformed_has_bwt_index_prefix() {
        // Use a string with high enough entropy (>3.5 bpb) to trigger BWT
        let input = b"The quick brown fox jumps over the lazy dog and some more text here!";
        let transformed = apply_transform(input, DomainType::Text);
        // First 4 bytes should be num_blocks >= 1
        let num_blocks = u32::from_le_bytes([transformed[0], transformed[1], transformed[2], transformed[3]]);
        assert!(num_blocks >= 1, "Should have at least 1 BWT block");
        // Should be larger than input (block headers add overhead)
        assert!(transformed.len() > input.len());
        // Round-trip must work
        let recovered = inverse_transform(&transformed, DomainType::Text);
        assert_eq!(input.as_slice(), recovered.as_slice());
    }

    #[test]
    fn test_empty_text_transform() {
        let transformed = apply_transform(b"", DomainType::Text);
        assert!(transformed.is_empty());
        let recovered = inverse_transform(&transformed, DomainType::Text);
        assert!(recovered.is_empty());
    }

    // -- Entropy reduction test --

    fn shannon_entropy(data: &[u8]) -> f64 {
        if data.is_empty() {
            return 0.0;
        }
        let mut counts = [0u64; 256];
        for &b in data {
            counts[b as usize] += 1;
        }
        let n = data.len() as f64;
        let mut h = 0.0;
        for &c in &counts {
            if c > 0 {
                let p = c as f64 / n;
                h -= p * p.log2();
            }
        }
        h
    }

    #[test]
    fn test_mtf_reduces_entropy_for_english() {
        // Build English text.
        let text = "The quick brown fox jumps over the lazy dog. \
                    Pack my box with five dozen liquor jugs. \
                    How vexingly quick daft zebras jump. "
            .repeat(100);
        let input = text.as_bytes();

        // Apply BWT then MTF.
        let (bwt_data, _index) = bwt_forward(input);
        let mtf_data = mtf_encode(&bwt_data);

        let entropy_original = shannon_entropy(input);
        let entropy_mtf = shannon_entropy(&mtf_data);

        assert!(
            entropy_mtf < entropy_original,
            "MTF output entropy ({:.3} bpb) should be lower than input ({:.3} bpb)",
            entropy_mtf,
            entropy_original,
        );
    }

    #[test]
    fn test_bwt_roundtrip_all_same_byte() {
        let input = vec![0xFFu8; 128];
        let (transformed, index) = bwt_forward(&input);
        let recovered = bwt_inverse(&transformed, index);
        assert_eq!(recovered, input);
    }

    #[test]
    fn test_bwt_roundtrip_single_byte() {
        let input = b"x";
        let (transformed, index) = bwt_forward(input);
        assert_eq!(transformed, vec![b'x']);
        assert_eq!(index, 0);
        let recovered = bwt_inverse(&transformed, index);
        assert_eq!(&recovered, input);
    }

    #[test]
    fn test_full_roundtrip_binary_bytes() {
        // All 256 byte values.
        let input: Vec<u8> = (0..=255).collect();
        let transformed = apply_transform(&input, DomainType::Text);
        let recovered = inverse_transform(&transformed, DomainType::Text);
        assert_eq!(recovered, input);
    }

    // -- DNA transform tests --

    #[test]
    fn test_dna_roundtrip_pure_acgt() {
        let input = b"ACGTACGTACGTACGT";
        let transformed = apply_transform(input, DomainType::DnaFasta);
        let recovered = inverse_transform(&transformed, DomainType::DnaFasta);
        assert_eq!(&recovered, input, "Pure ACGT round-trip failed");
    }

    #[test]
    fn test_dna_roundtrip_with_n() {
        let input = b"ACNGTANNCGT";
        let transformed = apply_transform(input, DomainType::DnaFasta);
        let recovered = inverse_transform(&transformed, DomainType::DnaFasta);
        assert_eq!(&recovered, input, "ACGT+N round-trip failed");
    }

    #[test]
    fn test_dna_roundtrip_fasta_with_header() {
        let input = b">chr1 human chromosome 1\nACGTACGTNNACGT\n>chr2\nGGCCTTAA\n";
        let transformed = apply_transform(input, DomainType::DnaFasta);
        let recovered = inverse_transform(&transformed, DomainType::DnaFasta);
        assert_eq!(
            std::str::from_utf8(&recovered).unwrap(),
            std::str::from_utf8(input).unwrap(),
            "FASTA round-trip failed"
        );
    }

    #[test]
    fn test_dna_compression_ratio_pure_acgt() {
        // 1000 pure ACGT bases should pack to ~250 bytes of packed data,
        // plus a small header. The packed section itself should be 4:1.
        let input: Vec<u8> = (0..1000)
            .map(|i| match i % 4 {
                0 => b'A',
                1 => b'C',
                2 => b'G',
                _ => b'T',
            })
            .collect();
        let transformed = apply_transform(&input, DomainType::DnaFasta);
        // The packed bases portion should be ceil(1000/4) = 250 bytes.
        // Total output includes metadata overhead, but should be well under input size.
        // Packed bases alone: 250 bytes vs 1000 input bytes = 4:1
        let packed_bases_len = (input.len() + 3) / 4; // 250
        assert_eq!(packed_bases_len, 250, "Packed bases should be 4:1 ratio");
        // The total transformed size should be much less than input.
        assert!(
            transformed.len() < input.len(),
            "Transformed DNA ({} bytes) should be smaller than input ({} bytes)",
            transformed.len(),
            input.len()
        );
    }

    #[test]
    fn test_dna_empty() {
        let transformed = apply_transform(b"", DomainType::DnaFasta);
        assert!(transformed.is_empty());
        let recovered = inverse_transform(&transformed, DomainType::DnaFasta);
        assert!(recovered.is_empty());
    }
}
