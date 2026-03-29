// Strategy evaluation: empirical assessment of three compression improvement paths.
// RESEARCH ONLY — reads corpus files, computes metrics, prints results. No source modifications.

use nexcomp::codecs::lzma_style;
use nexcomp::transform;
use nexcomp::classifier::DomainType;

/// Shannon entropy in bits per byte (0-order).
fn shannon_entropy(data: &[u8]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    let mut counts = [0u64; 256];
    for &b in data {
        counts[b as usize] += 1;
    }
    let n = data.len() as f64;
    let mut h = 0.0f64;
    for &c in &counts {
        if c > 0 {
            let p = c as f64 / n;
            h -= p * p.log2();
        }
    }
    h
}

/// BWT forward (standalone copy for testing — avoids private fn access issue).
/// Naive O(n^2 log n) — only for small blocks.
fn bwt_forward(data: &[u8]) -> (Vec<u8>, u32) {
    let n = data.len();
    if n == 0 {
        return (Vec::new(), 0);
    }
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
    let transformed: Vec<u8> = indices.iter().map(|&i| data[(i + n - 1) % n]).collect();
    let original_index = indices.iter().position(|&i| i == 0).unwrap() as u32;
    (transformed, original_index)
}

/// MTF encode (standalone copy).
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

/// Get bzip2 compressed size for a file path (returns None if bzip2 not available).
fn bzip2_size(path: &str) -> Option<usize> {
    let out_path = "/tmp/strategy_eval_bz2.bz2";
    let status = std::process::Command::new("bzip2")
        .args(["-9", "-c", path])
        .stdout(std::fs::File::create(out_path).ok()?)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    std::fs::metadata(out_path).ok().map(|m| m.len() as usize)
}

#[test]
fn strategy_evaluation() {
    eprintln!("\n{}", "=".repeat(80));
    eprintln!("  NEXCOMP Strategy Evaluation — Empirical Assessment");
    eprintln!("{}", "=".repeat(80));

    let calgary_dir = "/tmp/nexcomp_corpora/calgary";

    // =========================================================================
    // STRATEGY 1: BWT + MTF entropy (theoretical rANS floor) on text samples
    // =========================================================================
    eprintln!("\n--- STRATEGY 1: BWT+MTF entropy on text samples ---");
    eprintln!("(Theoretical best a perfect entropy coder like rANS could achieve)");
    eprintln!();

    let text_files = ["book1", "book2", "bib", "paper1", "paper2", "news"];
    for &fname in &text_files {
        let path = format!("{}/{}", calgary_dir, fname);
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("  [SKIP] {}: {}", fname, e);
                continue;
            }
        };

        let raw_entropy = shannon_entropy(&data);

        // Test with multiple BWT block sizes: 1KB, 4KB, 8KB, 16KB
        // Use first N bytes for larger block sizes to keep runtime sane
        let block_sizes = [1024usize, 4096, 8192, 16384];

        eprintln!("  {} ({} bytes, raw entropy: {:.3} bpb)", fname, data.len(), raw_entropy);

        for &bs in &block_sizes {
            // Take a sample: first min(64KB, len) bytes, process in blocks of `bs`
            let sample_limit = data.len().min(65536);
            let sample = &data[..sample_limit];

            let mut all_mtf = Vec::new();
            for chunk in sample.chunks(bs) {
                let (bwt_data, _idx) = bwt_forward(chunk);
                let mtf_data = mtf_encode(&bwt_data);
                all_mtf.extend_from_slice(&mtf_data);
            }

            let mtf_entropy = shannon_entropy(&all_mtf);

            // Also measure how many bytes are 0 or 1 (MTF concentration)
            let zeros = all_mtf.iter().filter(|&&b| b == 0).count();
            let low_vals = all_mtf.iter().filter(|&&b| b < 4).count();
            let zero_pct = zeros as f64 / all_mtf.len() as f64 * 100.0;
            let low_pct = low_vals as f64 / all_mtf.len() as f64 * 100.0;

            eprintln!("    block={:>5}: MTF entropy={:.3} bpb | zeros={:.1}% | <4={:.1}%",
                bs, mtf_entropy, zero_pct, low_pct);
        }

        // bzip2 reference
        if let Some(bz_size) = bzip2_size(&path) {
            let bz_bpb = bz_size as f64 * 8.0 / data.len() as f64;
            eprintln!("    bzip2-9:    {:.3} bpb ({} bytes)", bz_bpb, bz_size);
        }
        eprintln!();
    }

    // =========================================================================
    // STRATEGY 2: LZMA codec output quality vs LZ77+Huffman baseline
    // =========================================================================
    eprintln!("\n--- STRATEGY 2: LZMA-style codec vs LZ77+Huffman baseline ---");
    eprintln!();

    let eval_files = ["book1", "bib", "paper1", "news", "progc", "progl"];
    for &fname in &eval_files {
        let path = format!("{}/{}", calgary_dir, fname);
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("  [SKIP] {}: {}", fname, e);
                continue;
            }
        };

        let orig_size = data.len();

        // LZMA-style codec
        let lzma_compressed = lzma_style::encode_block(&data);
        let lzma_size = lzma_compressed.len();
        let lzma_bpb = lzma_size as f64 * 8.0 / orig_size as f64;

        // Verify roundtrip
        let lzma_decoded = lzma_style::decode_block(&lzma_compressed);
        let lzma_ok = match &lzma_decoded {
            Ok(d) => d == &data,
            Err(_) => false,
        };

        // LZ77+Huffman baseline (use nexcomp's LZ77 compress)
        let lz77_compressed = nexcomp::lz77::compress(&data);
        let lz77_size = lz77_compressed.len();
        let lz77_bpb = lz77_size as f64 * 8.0 / orig_size as f64;

        // BWT+MTF+rANS via apply_transform + rANS encode
        let transformed = transform::apply_transform(&data, DomainType::Text);
        let transform_bpb = transformed.len() as f64 * 8.0 / orig_size as f64;

        // bzip2 reference
        let bz_bpb = bzip2_size(&path)
            .map(|s| s as f64 * 8.0 / orig_size as f64);

        eprintln!("  {:<12} {:>8} bytes | LZ77: {:.3} bpb | LZMA: {:.3} bpb | BWT+MTF raw: {:.3} bpb | bzip2: {:.3} bpb | LZMA OK: {}",
            fname, orig_size, lz77_bpb, lzma_bpb,
            transform_bpb,
            bz_bpb.unwrap_or(0.0),
            if lzma_ok { "yes" } else { "NO" });
    }

    // =========================================================================
    // STRATEGY 3: Stride-delta entropy on geo
    // =========================================================================
    eprintln!("\n--- STRATEGY 3: Stride-delta entropy on geo ---");
    eprintln!();

    let geo_path = format!("{}/geo", calgary_dir);
    match std::fs::read(&geo_path) {
        Ok(data) => {
            let orig_size = data.len();
            let raw_entropy = shannon_entropy(&data);
            eprintln!("  geo: {} bytes, raw entropy: {:.3} bpb", orig_size, raw_entropy);

            for stride in [1, 2, 4, 8] {
                // XOR delta
                let mut xor_delta = Vec::with_capacity(data.len());
                for i in 0..data.len() {
                    if i < stride {
                        xor_delta.push(data[i]);
                    } else {
                        xor_delta.push(data[i] ^ data[i - stride]);
                    }
                }
                let xor_entropy = shannon_entropy(&xor_delta);

                // Subtraction delta
                let mut sub_delta = Vec::with_capacity(data.len());
                for i in 0..data.len() {
                    if i < stride {
                        sub_delta.push(data[i]);
                    } else {
                        sub_delta.push(data[i].wrapping_sub(data[i - stride]));
                    }
                }
                let sub_entropy = shannon_entropy(&sub_delta);

                // Count zeros in delta (indicator of how well delta works)
                let xor_zeros = xor_delta.iter().filter(|&&b| b == 0).count();
                let xor_zero_pct = xor_zeros as f64 / xor_delta.len() as f64 * 100.0;

                eprintln!("    stride={}: XOR entropy={:.3} bpb, SUB entropy={:.3} bpb | XOR zeros={:.1}%",
                    stride, xor_entropy, sub_entropy, xor_zero_pct);
            }

            // Now try BWT+MTF on the delta output to see combined potential
            eprintln!();
            eprintln!("  Combined: stride-2 XOR delta → BWT(4KB) + MTF → entropy:");
            let mut xor2_delta = Vec::with_capacity(data.len());
            for i in 0..data.len() {
                if i < 2 {
                    xor2_delta.push(data[i]);
                } else {
                    xor2_delta.push(data[i] ^ data[i - 2]);
                }
            }
            let mut all_mtf = Vec::new();
            for chunk in xor2_delta.chunks(4096) {
                let (bwt_data, _) = bwt_forward(chunk);
                let mtf_data = mtf_encode(&bwt_data);
                all_mtf.extend_from_slice(&mtf_data);
            }
            let combined_entropy = shannon_entropy(&all_mtf);
            eprintln!("    stride-2 XOR + BWT(4K) + MTF entropy: {:.3} bpb", combined_entropy);

            // bzip2 reference for geo
            if let Some(bz_size) = bzip2_size(&geo_path) {
                let bz_bpb = bz_size as f64 * 8.0 / orig_size as f64;
                eprintln!("    bzip2-9 on geo: {:.3} bpb", bz_bpb);
            }

            // Current nexcomp baseline
            let nxc_lz77 = nexcomp::lz77::compress(&data);
            let nxc_bpb = nxc_lz77.len() as f64 * 8.0 / orig_size as f64;
            eprintln!("    NEXCOMP LZ77 on geo: {:.3} bpb", nxc_bpb);
        }
        Err(e) => eprintln!("  [SKIP] geo: {}", e),
    }

    // =========================================================================
    // SUMMARY
    // =========================================================================
    eprintln!("\n{}", "=".repeat(80));
    eprintln!("  SUMMARY & RECOMMENDATIONS");
    eprintln!("{}", "=".repeat(80));
    eprintln!();
    eprintln!("  Current baseline: 2.557 bpb (Calgary avg)");
    eprintln!("  Target: beat bzip2 at 2.109 bpb (gap = 0.448 bpb)");
    eprintln!();
    eprintln!("  Strategy 1 (BWT+MTF+rANS): MTF entropy is the floor for rANS.");
    eprintln!("    If MTF entropy on 4-8KB blocks is ~1.5-2.0 bpb for text,");
    eprintln!("    this gives ~0.5-1.0 bpb improvement over current 2.557.");
    eprintln!("    Key question: does larger BWT block size help enough?");
    eprintln!();
    eprintln!("  Strategy 2 (LZMA codec): Compare LZMA bpb to LZ77+Huffman.");
    eprintln!("    If LZMA is better, it can be a drop-in replacement.");
    eprintln!();
    eprintln!("  Strategy 3 (stride-delta for geo): If entropy drops significantly,");
    eprintln!("    a domain-specific delta pre-filter for structured binary data");
    eprintln!("    can reclaim easy bpb on geo (currently worst file at 5.34 bpb).");
    eprintln!();
}
