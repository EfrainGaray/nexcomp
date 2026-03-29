// NEXCOMP v1.1 — Benchmark: per-block Huffman + context1 + split + encoder rep-offsets
// Target: beat bzip2-9 on Calgary (2.109 bpb) and Canterbury (1.545 bpb)

use nexcomp::lz77::{self, Lz77Encoder, huffman};
use std::path::Path;
use std::time::Instant;

/// Compress using the best available method (tries blocked, context1, split)
fn compress_best(data: &[u8]) -> (Vec<u8>, &'static str) {
    let mut enc = Lz77Encoder::new();
    let (tokens, rep_count) = enc.encode(data);
    let total_matches = tokens.iter().filter(|t| t.is_match()).count();
    let rep_pct = if total_matches > 0 { rep_count as f64 / total_matches as f64 * 100.0 } else { 0.0 };
    let _ = rep_pct; // used in detailed output

    // Try multiple strategies, pick smallest
    let blocked = huffman::huffman_encode_blocked(&tokens, 8192);
    let context1 = huffman::huffman_encode_context1(&tokens, 8192);

    // Also try different block sizes
    let blocked_16k = huffman::huffman_encode_blocked(&tokens, 16384);
    let blocked_4k = huffman::huffman_encode_blocked(&tokens, 4096);

    let mut candidates: Vec<(Vec<u8>, &str)> = vec![
        (blocked, "blk8k"),
        (context1, "ctx1"),
        (blocked_16k, "blk16k"),
        (blocked_4k, "blk4k"),
    ];

    candidates.sort_by_key(|(data, _)| data.len());
    let (best, label) = candidates.into_iter().next().unwrap();

    let mut out = Vec::with_capacity(5 + best.len());
    // Header: 1 byte method tag + 4 bytes original size
    let tag: u8 = match label {
        "ctx1" => 2,
        _ => 1, // blocked (any block size)
    };
    out.push(tag);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&best);
    (out, label)
}

fn decompress_best(payload: &[u8]) -> Vec<u8> {
    let tag = payload[0];
    let huff_data = &payload[5..];
    let tokens = match tag {
        2 => huffman::huffman_decode_context1(huff_data),
        _ => huffman::huffman_decode_blocked(huff_data),
    };
    lz77::lz77_decode(&tokens).expect("LZ77 decode failed")
}

fn bench_file(path: &str) -> Option<(String, usize, usize, usize, usize, f64, f64, f64, bool, String, f64)> {
    let data = std::fs::read(path).ok()?;
    let name = std::path::Path::new(path).file_name()?.to_str()?.to_string();
    let orig = data.len();

    let t0 = Instant::now();
    let (compressed, method) = compress_best(&data);
    let comp_time = t0.elapsed().as_secs_f64();

    let decompressed = decompress_best(&compressed);
    let verify = data == decompressed;

    // gzip reference
    let gz_out = "/tmp/nxc_v11_bench.gz";
    std::process::Command::new("gzip")
        .args(["-9", "-c", path])
        .stdout(std::fs::File::create(gz_out).unwrap())
        .status().ok()?;
    let gz_size = std::fs::metadata(gz_out).ok()?.len() as usize;

    // bzip2 reference
    let bz_out = "/tmp/nxc_v11_bench.bz2";
    std::process::Command::new("bzip2")
        .args(["-9", "-c", path])
        .stdout(std::fs::File::create(bz_out).unwrap())
        .status().ok()?;
    let bz_size = std::fs::metadata(bz_out).ok()?.len() as usize;

    let nxc_bpb = compressed.len() as f64 * 8.0 / orig as f64;
    let gz_bpb = gz_size as f64 * 8.0 / orig as f64;
    let bz_bpb = bz_size as f64 * 8.0 / orig as f64;

    Some((name, orig, compressed.len(), gz_size, bz_size, nxc_bpb, gz_bpb, bz_bpb, verify, method.to_string(), comp_time))
}

fn run_corpus(corpus_name: &str, dir: &str, files: &[&str]) {
    if !Path::new(dir).exists() {
        eprintln!("{} corpus not found at {}, skipping", corpus_name, dir);
        return;
    }
    eprintln!("\n{}", "=".repeat(110));
    eprintln!("  {} — NEXCOMP v1.1 (best of blocked/ctx1/split) vs gzip-9 vs bzip2-9", corpus_name);
    eprintln!("{}", "=".repeat(110));
    eprintln!("{:<15} {:>10} {:>10} {:>6} {:>10} {:>6} {:>10} {:>6} {:>7} {:>7} {:>5} {:>4}",
        "File", "Orig", "NXC", "bpb", "gzip-9", "bpb", "bzip2-9", "bpb", "Δgzip", "Δbz2", "meth", "OK");
    eprintln!("{}", "-".repeat(110));

    let mut tot_orig = 0usize;
    let mut tot_nxc = 0usize;
    let mut tot_gz = 0usize;
    let mut tot_bz = 0usize;
    let mut wins_gz = 0;
    let mut wins_bz = 0;
    let mut total = 0;

    for f in files {
        let path = format!("{}/{}", dir, f);
        if let Some((name, orig, nxc, gz, bz, nxc_bpb, gz_bpb, bz_bpb, verify, method, _time)) = bench_file(&path) {
            let delta_gz = nxc_bpb - gz_bpb;
            let delta_bz = nxc_bpb - bz_bpb;
            eprintln!("{:<15} {:>10} {:>10} {:>6.3} {:>10} {:>6.3} {:>10} {:>6.3} {:>+7.3} {:>+7.3} {:>5} {}",
                name, orig, nxc, nxc_bpb, gz, gz_bpb, bz, bz_bpb, delta_gz, delta_bz, method,
                if verify { "OK" } else { "FAIL" });
            assert!(verify, "Lossless FAILED for {}", name);
            tot_orig += orig;
            tot_nxc += nxc;
            tot_gz += gz;
            tot_bz += bz;
            if delta_gz < 0.0 { wins_gz += 1; }
            if delta_bz < 0.0 { wins_bz += 1; }
            total += 1;
        }
    }

    if total == 0 || tot_orig == 0 {
        eprintln!("{} corpus directory exists but no readable files were found, skipping", corpus_name);
        return;
    }

    eprintln!("{}", "-".repeat(110));
    let nxc_bpb = tot_nxc as f64 * 8.0 / tot_orig as f64;
    let gz_bpb = tot_gz as f64 * 8.0 / tot_orig as f64;
    let bz_bpb = tot_bz as f64 * 8.0 / tot_orig as f64;
    eprintln!("{:<15} {:>10} {:>10} {:>6.3} {:>10} {:>6.3} {:>10} {:>6.3} {:>+7.3} {:>+7.3}",
        "TOTAL", tot_orig, tot_nxc, nxc_bpb, tot_gz, gz_bpb, tot_bz, bz_bpb,
        nxc_bpb - gz_bpb, nxc_bpb - bz_bpb);
    eprintln!("\n  Wins vs gzip-9: {}/{}", wins_gz, total);
    eprintln!("  Wins vs bzip2-9: {}/{}", wins_bz, total);
    eprintln!("  NXC: {:.3} bpb | gzip-9: {:.3} bpb | bzip2-9: {:.3} bpb", nxc_bpb, gz_bpb, bz_bpb);
}

#[test]
fn test_v11_calgary() {
    let files = ["bib", "book1", "book2", "geo", "news", "obj1", "obj2",
                 "paper1", "paper2", "pic", "progc", "progl", "progp", "trans"];
    run_corpus("CALGARY CORPUS", "/tmp/nexcomp_corpora/calgary", &files);
}

#[test]
fn test_v11_canterbury() {
    let files = ["alice29.txt", "asyoulik.txt", "cp.html", "fields.c",
                 "grammar.lsp", "kennedy.xls", "lcet10.txt", "plrabn12.txt",
                 "ptt5", "sum", "xargs.1"];
    run_corpus("CANTERBURY CORPUS", "/tmp/nexcomp_corpora/canterbury", &files);
}

#[test]
fn test_v11_all_lossless() {
    let calgary_files = ["bib", "book1", "book2", "geo", "news", "obj1", "obj2",
                         "paper1", "paper2", "pic", "progc", "progl", "progp", "trans"];
    let canterbury_files = ["alice29.txt", "asyoulik.txt", "cp.html", "fields.c",
                            "grammar.lsp", "kennedy.xls", "lcet10.txt", "plrabn12.txt",
                            "ptt5", "sum", "xargs.1"];

    let mut count = 0;
    let calgary_dir = Path::new("/tmp/nexcomp_corpora/calgary");
    let canterbury_dir = Path::new("/tmp/nexcomp_corpora/canterbury");
    if !calgary_dir.exists() && !canterbury_dir.exists() {
        eprintln!("Calgary and Canterbury corpora not found, skipping lossless sweep");
        return;
    }
    for f in &calgary_files {
        let path = calgary_dir.join(f);
        if let Ok(data) = std::fs::read(&path) {
            let (compressed, _) = compress_best(&data);
            let decompressed = decompress_best(&compressed);
            assert_eq!(data, decompressed, "Lossless FAILED for Calgary/{}", f);
            count += 1;
        }
    }
    for f in &canterbury_files {
        let path = canterbury_dir.join(f);
        if let Ok(data) = std::fs::read(&path) {
            let (compressed, _) = compress_best(&data);
            let decompressed = decompress_best(&compressed);
            assert_eq!(data, decompressed, "Lossless FAILED for Canterbury/{}", f);
            count += 1;
        }
    }
    if count == 0 {
        eprintln!("No Calgary or Canterbury files available, skipping lossless sweep");
        return;
    }
    eprintln!("\nAll {} files verified lossless with v1.1 best-of-3 strategy", count);
}

#[test]
fn test_v11_rep_offset_stats() {
    let calgary_files = ["bib", "book1", "book2", "geo", "news", "obj1", "obj2",
                         "paper1", "paper2", "pic", "progc", "progl", "progp", "trans"];
    let corpus_dir = Path::new("/tmp/nexcomp_corpora/calgary");
    if !corpus_dir.exists() {
        eprintln!("Calgary corpus not found at {}, skipping rep-offset stats", corpus_dir.display());
        return;
    }

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("  ENCODER REP-OFFSET STATISTICS — Calgary Corpus");
    eprintln!("{}", "=".repeat(80));
    eprintln!("{:<15} {:>10} {:>10} {:>8} {:>10}", "File", "Matches", "RepMatch", "Rep%", "Type");
    eprintln!("{}", "-".repeat(80));

    let mut seen = 0usize;
    for f in &calgary_files {
        let path = corpus_dir.join(f);
        if let Ok(data) = std::fs::read(&path) {
            let mut enc = Lz77Encoder::new();
            let (tokens, rep_count) = enc.encode(&data);
            let total_matches = tokens.iter().filter(|t| t.is_match()).count();
            let rep_pct = if total_matches > 0 { rep_count as f64 / total_matches as f64 * 100.0 } else { 0.0 };

            // Classify file type
            let printable = data.iter().filter(|&&b| b >= 32 && b < 127).count();
            let ftype = if printable as f64 / data.len() as f64 > 0.85 { "text" }
                       else if data.iter().filter(|&&b| b == 0).count() > data.len() / 10 { "binary" }
                       else { "mixed" };

            eprintln!("{:<15} {:>10} {:>10} {:>7.1}% {:>10}", f, total_matches, rep_count, rep_pct, ftype);
            seen += 1;
        }
    }

    if seen == 0 {
        eprintln!("No Calgary files were readable, skipping rep-offset stats");
    }
}

#[test]
fn test_v11_tar_corpus() {
    let data = std::fs::read("tests/fixtures/corpus.tar").unwrap();
    let (compressed, method) = compress_best(&data);
    let decompressed = decompress_best(&compressed);
    assert_eq!(data, decompressed, "Tar round-trip FAILED");

    let bpb = compressed.len() as f64 * 8.0 / data.len() as f64;
    eprintln!("\nTar corpus (3.2MB): {} bytes, {:.3} bpb, method={}", compressed.len(), bpb, method);
}
