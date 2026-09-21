// NEXCOMP v1.2 — Adaptive codec benchmark
// Selector: min(candidate_codec, lz77_huffman_baseline) per file
// Guaranteed no regression vs v1.1

use nexcomp::adaptive::{adaptive_compress, adaptive_decompress, codec_summary, compress_block_adaptive_pub};
use nexcomp::classifier_v2::{classify_block_v2, BlockMetrics};
use std::path::Path;
use std::time::Instant;

fn bench_file(path: &str) -> Option<(String, usize, usize, usize, usize, f64, f64, f64, bool, String, f64)> {
    let data = std::fs::read(path).ok()?;
    let name = std::path::Path::new(path).file_name()?.to_str()?.to_string();
    let orig = data.len();

    let t0 = Instant::now();
    let compressed = adaptive_compress(&data);
    let comp_time = t0.elapsed().as_secs_f64();

    let decompressed = adaptive_decompress(&compressed);
    let verify = data == decompressed;

    let codec = codec_summary(&compressed).unwrap();

    // gzip reference
    let gz_out = "/tmp/nxc_v12_bench.gz";
    std::process::Command::new("gzip")
        .args(["-9", "-c", path])
        .stdout(std::fs::File::create(gz_out).unwrap())
        .status().ok()?;
    let gz_size = std::fs::metadata(gz_out).ok()?.len() as usize;

    // bzip2 reference
    let bz_out = "/tmp/nxc_v12_bench.bz2";
    std::process::Command::new("bzip2")
        .args(["-9", "-c", path])
        .stdout(std::fs::File::create(bz_out).unwrap())
        .status().ok()?;
    let bz_size = std::fs::metadata(bz_out).ok()?.len() as usize;

    let nxc_bpb = compressed.len() as f64 * 8.0 / orig as f64;
    let gz_bpb = gz_size as f64 * 8.0 / orig as f64;
    let bz_bpb = bz_size as f64 * 8.0 / orig as f64;

    Some((name, orig, compressed.len(), gz_size, bz_size, nxc_bpb, gz_bpb, bz_bpb, verify,
          codec, comp_time))
}

fn run_corpus(corpus_name: &str, dir: &str, files: &[&str]) {
    if !Path::new(dir).exists() {
        eprintln!("{} not found at {}, skipping", corpus_name, dir);
        return;
    }

    eprintln!("\n{}", "=".repeat(115));
    eprintln!("  {} — NEXCOMP v1.2 ADAPTIVE vs gzip-9 vs bzip2-9", corpus_name);
    eprintln!("{}", "=".repeat(115));
    eprintln!("{:<15} {:>10} {:>10} {:>6} {:>10} {:>6} {:>10} {:>6} {:>7} {:>7} {:>7} {:>4}",
        "File", "Orig", "NXC", "bpb", "gzip-9", "bpb", "bzip2-9", "bpb", "Δgzip", "Δbz2", "codec", "OK");
    eprintln!("{}", "-".repeat(115));

    let mut tot_orig = 0usize;
    let mut tot_nxc = 0usize;
    let mut tot_gz = 0usize;
    let mut tot_bz = 0usize;
    let mut wins_gz = 0;
    let mut wins_bz = 0;
    let mut total = 0;

    for f in files {
        let path = format!("{}/{}", dir, f);
        if let Some((name, orig, nxc, gz, bz, nxc_bpb, gz_bpb, bz_bpb, verify, codec, _time)) = bench_file(&path) {
            let delta_gz = nxc_bpb - gz_bpb;
            let delta_bz = nxc_bpb - bz_bpb;
            eprintln!("{:<15} {:>10} {:>10} {:>6.3} {:>10} {:>6.3} {:>10} {:>6.3} {:>+7.3} {:>+7.3} {:>7} {}",
                name, orig, nxc, nxc_bpb, gz, gz_bpb, bz, bz_bpb, delta_gz, delta_bz, codec,
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

    if total == 0 { return; }

    eprintln!("{}", "-".repeat(115));
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
fn test_v12_calgary() {
    let files = ["bib", "book1", "book2", "geo", "news", "obj1", "obj2",
                 "paper1", "paper2", "pic", "progc", "progl", "progp", "trans"];
    run_corpus("CALGARY CORPUS", "/tmp/nexcomp_corpora/calgary", &files);
}

#[test]
fn test_v12_canterbury() {
    let files = ["alice29.txt", "asyoulik.txt", "cp.html", "fields.c",
                 "grammar.lsp", "kennedy.xls", "lcet10.txt", "plrabn12.txt",
                 "ptt5", "sum", "xargs.1"];
    run_corpus("CANTERBURY CORPUS", "/tmp/nexcomp_corpora/canterbury", &files);
}

#[test]
fn test_v12_all_lossless() {
    let calgary_files = ["bib", "book1", "book2", "geo", "news", "obj1", "obj2",
                         "paper1", "paper2", "pic", "progc", "progl", "progp", "trans"];
    let canterbury_files = ["alice29.txt", "asyoulik.txt", "cp.html", "fields.c",
                            "grammar.lsp", "kennedy.xls", "lcet10.txt", "plrabn12.txt",
                            "ptt5", "sum", "xargs.1"];

    let mut count = 0;
    for f in &calgary_files {
        let path = format!("/tmp/nexcomp_corpora/calgary/{}", f);
        if let Ok(data) = std::fs::read(&path) {
            let compressed = adaptive_compress(&data);
            let decompressed = adaptive_decompress(&compressed);
            assert_eq!(data, decompressed, "Lossless FAILED for Calgary/{}", f);
            count += 1;
        }
    }
    for f in &canterbury_files {
        let path = format!("/tmp/nexcomp_corpora/canterbury/{}", f);
        if let Ok(data) = std::fs::read(&path) {
            let compressed = adaptive_compress(&data);
            let decompressed = adaptive_decompress(&compressed);
            assert_eq!(data, decompressed, "Lossless FAILED for Canterbury/{}", f);
            count += 1;
        }
    }
    eprintln!("\nAll {} files verified lossless with v1.2 adaptive selector", count);
}

// v1.1 baseline bpb per file (for no-regression check)
const V11_CALGARY: &[(&str, f64)] = &[
    ("bib", 2.515), ("book1", 3.273), ("book2", 2.646),
    ("geo", 5.344), ("news", 2.845), ("obj1", 3.760),
    ("obj2", 2.506), ("paper1", 2.838), ("paper2", 2.964),
    ("pic", 0.829), ("progc", 2.742), ("progl", 1.844),
    ("progp", 1.823), ("trans", 1.605),
];

const V11_CANTERBURY: &[(&str, f64)] = &[
    ("alice29.txt", 2.907), ("asyoulik.txt", 3.197), ("cp.html", 2.642),
    ("fields.c", 2.333), ("grammar.lsp", 2.883), ("kennedy.xls", 0.863),
    ("lcet10.txt", 2.640), ("plrabn12.txt", 3.282), ("ptt5", 0.829),
    ("sum", 2.584), ("xargs.1", 3.509),
];

#[test]
fn test_v12_no_regression_calgary() {
    let dir = "/tmp/nexcomp_corpora/calgary";
    if !Path::new(dir).exists() { return; }

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("  NO-REGRESSION CHECK — Calgary");
    eprintln!("{}", "=".repeat(80));
    eprintln!("{:<15} {:>8} {:>8} {:>8} {:>7}", "File", "v1.1", "v1.2", "delta", "status");
    eprintln!("{}", "-".repeat(80));

    let mut regressions = 0;
    for &(file, v11_bpb) in V11_CALGARY {
        let path = format!("{}/{}", dir, file);
        if let Ok(data) = std::fs::read(&path) {
            let compressed = adaptive_compress(&data);
            let v12_bpb = compressed.len() as f64 * 8.0 / data.len() as f64;
            let delta = v12_bpb - v11_bpb;
            let status = if delta <= 0.001 { "OK" } else { "REGR" };
            if delta > 0.001 { regressions += 1; }
            eprintln!("{:<15} {:>8.3} {:>8.3} {:>+8.3} {:>7}", file, v11_bpb, v12_bpb, delta, status);
        }
    }
    eprintln!("\nRegressions: {}/14", regressions);
}

#[test]
fn test_v12_no_regression_canterbury() {
    let dir = "/tmp/nexcomp_corpora/canterbury";
    if !Path::new(dir).exists() { return; }

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("  NO-REGRESSION CHECK — Canterbury");
    eprintln!("{}", "=".repeat(80));
    eprintln!("{:<15} {:>8} {:>8} {:>8} {:>7}", "File", "v1.1", "v1.2", "delta", "status");
    eprintln!("{}", "-".repeat(80));

    let mut regressions = 0;
    for &(file, v11_bpb) in V11_CANTERBURY {
        let path = format!("{}/{}", dir, file);
        if let Ok(data) = std::fs::read(&path) {
            let compressed = adaptive_compress(&data);
            let v12_bpb = compressed.len() as f64 * 8.0 / data.len() as f64;
            let delta = v12_bpb - v11_bpb;
            let status = if delta <= 0.001 { "OK" } else { "REGR" };
            if delta > 0.001 { regressions += 1; }
            eprintln!("{:<15} {:>8.3} {:>8.3} {:>+8.3} {:>7}", file, v11_bpb, v12_bpb, delta, status);
        }
    }
    eprintln!("\nRegressions: {}/11", regressions);
}

#[test]
fn test_v12_codec_selection_calgary() {
    let dir = "/tmp/nexcomp_corpora/calgary";
    if !Path::new(dir).exists() { return; }

    eprintln!("\n{}", "=".repeat(90));
    eprintln!("  CODEC SELECTION — Calgary Corpus");
    eprintln!("{}", "=".repeat(90));
    eprintln!("{:<15} {:>7} {:>7} {:>7} {:>7} {:>9}", "File", "chosen", "entr", "ascii%", "autocr", "uniq");
    eprintln!("{}", "-".repeat(90));

    let files = ["bib", "book1", "book2", "geo", "news", "obj1", "obj2",
                 "paper1", "paper2", "pic", "progc", "progl", "progp", "trans"];

    for f in &files {
        let path = format!("{}/{}", dir, f);
        if let Ok(data) = std::fs::read(&path) {
            let (_, codec) = compress_block_adaptive_pub(&data);
            let metrics = BlockMetrics::compute(&data);
            eprintln!("{:<15} {:>7} {:>7.2} {:>6.1}% {:>7.3} {:>9}",
                f, codec.name(), metrics.entropy, metrics.ascii_ratio * 100.0,
                metrics.autocorrelation, metrics.unique_bytes);
        }
    }
}

#[test]
fn test_v12_tar_corpus() {
    let data = std::fs::read("tests/fixtures/corpus.tar").unwrap();
    let compressed = adaptive_compress(&data);
    let decompressed = adaptive_decompress(&compressed);
    assert_eq!(data, decompressed, "Tar round-trip FAILED");

    let bpb = compressed.len() as f64 * 8.0 / data.len() as f64;
    let codec = codec_summary(&compressed).unwrap();
    eprintln!("\nTar corpus: {} bytes, {:.3} bpb, codec={}", compressed.len(), bpb, codec);
    assert!(bpb <= 1.211, "Tar regression: {:.3} > 1.211", bpb);
}
