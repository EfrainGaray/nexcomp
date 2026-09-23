// NEXCOMP v1.0 — Standard corpora benchmark with all improvements
// Per-block Huffman + improved lazy matching + (repeated offsets if available)

use nexcomp::lz77::{self, Lz77Encoder, huffman};
use std::path::Path;
use std::time::Instant;

fn compress_blocked(data: &[u8]) -> Vec<u8> {
    let mut enc = Lz77Encoder::new();
    let (tokens, _) = enc.encode(data);
    let huff = huffman::huffman_encode_blocked(&tokens, 8192);
    let mut out = Vec::with_capacity(4 + huff.len());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&huff);
    out
}

fn decompress_blocked(payload: &[u8]) -> Vec<u8> {
    let tokens = huffman::huffman_decode_blocked(&payload[4..]);
    lz77::lz77_decode(&tokens).expect("LZ77 decode failed")
}

fn bench_file(path: &str) -> Option<(String, usize, usize, usize, f64, f64, bool)> {
    let data = std::fs::read(path).ok()?;
    let name = std::path::Path::new(path).file_name()?.to_str()?.to_string();
    let orig = data.len();

    let t0 = Instant::now();
    let compressed = compress_blocked(&data);
    let _comp_time = t0.elapsed().as_secs_f64();

    let decompressed = decompress_blocked(&compressed);
    let verify = data == decompressed;

    // gzip reference
    let gz_out = "/tmp/nxc_std_bench.gz";
    std::process::Command::new("gzip")
        .args(["-9", "-c", path])
        .stdout(std::fs::File::create(gz_out).unwrap())
        .status().ok()?;
    let gz_size = std::fs::metadata(gz_out).ok()?.len() as usize;

    let nxc_bpb = compressed.len() as f64 * 8.0 / orig as f64;
    let gz_bpb = gz_size as f64 * 8.0 / orig as f64;

    Some((name, orig, compressed.len(), gz_size, nxc_bpb, gz_bpb, verify))
}

#[test]
fn test_calgary_benchmark() {
    let dir = "/tmp/nexcomp_corpora/calgary";
    if !Path::new(dir).exists() {
        eprintln!("Calgary corpus not found at {}, skipping", dir);
        return;
    }
    let files = ["bib", "book1", "book2", "geo", "news", "obj1", "obj2",
                 "paper1", "paper2", "pic", "progc", "progl", "progp", "trans"];

    eprintln!("\n{}", "=".repeat(95));
    eprintln!("  CALGARY CORPUS — NEXCOMP (per-block Huffman bs=8192) vs gzip-9");
    eprintln!("{}", "=".repeat(95));
    eprintln!("{:<15} {:>10} {:>10} {:>6} {:>10} {:>6} {:>7} {:>4}",
        "File", "Orig", "NXC", "bpb", "gzip-9", "bpb", "Δgzip", "OK");
    eprintln!("{}", "-".repeat(95));

    let mut tot_orig = 0usize;
    let mut tot_nxc = 0usize;
    let mut tot_gz = 0usize;
    let mut wins = 0;
    let mut total = 0;

    for f in &files {
        let path = format!("{}/{}", dir, f);
        if let Some((name, orig, nxc, gz, nxc_bpb, gz_bpb, verify)) = bench_file(&path) {
            let delta = nxc_bpb - gz_bpb;
            let marker = if delta < 0.0 { " WIN" } else { "" };
            eprintln!("{:<15} {:>10} {:>10} {:>6.3} {:>10} {:>6.3} {:>+7.3}{} {}",
                name, orig, nxc, nxc_bpb, gz, gz_bpb, delta, marker,
                if verify { "OK" } else { "FAIL" });
            assert!(verify, "Lossless FAILED for {}", name);
            tot_orig += orig;
            tot_nxc += nxc;
            tot_gz += gz;
            if delta < 0.0 { wins += 1; }
            total += 1;
        }
    }

    if tot_orig == 0 {
        eprintln!("Calgary corpus directory exists but no readable files were found, skipping");
        return;
    }

    eprintln!("{}", "-".repeat(95));
    let nxc_bpb = tot_nxc as f64 * 8.0 / tot_orig as f64;
    let gz_bpb = tot_gz as f64 * 8.0 / tot_orig as f64;
    eprintln!("{:<15} {:>10} {:>10} {:>6.3} {:>10} {:>6.3} {:>+7.3}",
        "TOTAL", tot_orig, tot_nxc, nxc_bpb, tot_gz, gz_bpb, nxc_bpb - gz_bpb);
    eprintln!("\n  Wins vs gzip-9: {}/{}", wins, total);
}

#[test]
fn test_canterbury_benchmark() {
    let dir = "/tmp/nexcomp_corpora/canterbury";
    if !Path::new(dir).exists() {
        eprintln!("Canterbury corpus not found at {}, skipping", dir);
        return;
    }
    let files = ["alice29.txt", "asyoulik.txt", "cp.html", "fields.c",
                 "grammar.lsp", "kennedy.xls", "lcet10.txt", "plrabn12.txt",
                 "ptt5", "sum", "xargs.1"];

    eprintln!("\n{}", "=".repeat(95));
    eprintln!("  CANTERBURY CORPUS — NEXCOMP (per-block Huffman bs=8192) vs gzip-9");
    eprintln!("{}", "=".repeat(95));
    eprintln!("{:<15} {:>10} {:>10} {:>6} {:>10} {:>6} {:>7} {:>4}",
        "File", "Orig", "NXC", "bpb", "gzip-9", "bpb", "Δgzip", "OK");
    eprintln!("{}", "-".repeat(95));

    let mut tot_orig = 0usize;
    let mut tot_nxc = 0usize;
    let mut tot_gz = 0usize;
    let mut wins = 0;
    let mut total = 0;

    for f in &files {
        let path = format!("{}/{}", dir, f);
        if let Some((name, orig, nxc, gz, nxc_bpb, gz_bpb, verify)) = bench_file(&path) {
            let delta = nxc_bpb - gz_bpb;
            let marker = if delta < 0.0 { " WIN" } else { "" };
            eprintln!("{:<15} {:>10} {:>10} {:>6.3} {:>10} {:>6.3} {:>+7.3}{} {}",
                name, orig, nxc, nxc_bpb, gz, gz_bpb, delta, marker,
                if verify { "OK" } else { "FAIL" });
            assert!(verify, "Lossless FAILED for {}", name);
            tot_orig += orig;
            tot_nxc += nxc;
            tot_gz += gz;
            if delta < 0.0 { wins += 1; }
            total += 1;
        }
    }

    if tot_orig == 0 {
        eprintln!("Canterbury corpus directory exists but no readable files were found, skipping");
        return;
    }

    eprintln!("{}", "-".repeat(95));
    let nxc_bpb = tot_nxc as f64 * 8.0 / tot_orig as f64;
    let gz_bpb = tot_gz as f64 * 8.0 / tot_orig as f64;
    eprintln!("{:<15} {:>10} {:>10} {:>6.3} {:>10} {:>6.3} {:>+7.3}",
        "TOTAL", tot_orig, tot_nxc, nxc_bpb, tot_gz, gz_bpb, nxc_bpb - gz_bpb);
    eprintln!("\n  Wins vs gzip-9: {}/{}", wins, total);
}

#[test]
fn test_tar_corpus_blocked() {
    let data = std::fs::read("tests/fixtures/corpus.tar").unwrap();
    let compressed = compress_blocked(&data);
    let decompressed = decompress_blocked(&compressed);
    assert_eq!(data, decompressed, "Tar round-trip FAILED");

    let bpb = compressed.len() as f64 * 8.0 / data.len() as f64;
    eprintln!("\nTar corpus (3.2MB): {} bytes, {:.3} bpb (was 1.554 greedy global)", compressed.len(), bpb);
}
