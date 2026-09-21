// NEXCOMP — Professional Benchmark Harness
// Compares NEXCOMP against 9 external compressors on multiple standard corpora.
// Run: cargo test --release --test professional_bench -- --ignored --nocapture

use nexcomp::adaptive::{adaptive_compress, adaptive_decompress, parse_blocks};
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

struct CompressorResult {
    name: String,
    compressed_size: usize,
    bpb: f64,
    comp_time_ms: f64,
    decomp_time_ms: f64,
    comp_speed_mbps: f64,
    decomp_speed_mbps: f64,
    lossless: bool,
}

// ---------------------------------------------------------------------------
// Competitor definitions
// ---------------------------------------------------------------------------

struct Competitor {
    name: &'static str,
    compress_args: &'static [&'static str],
    decompress_args: &'static [&'static str],
}

const COMPETITORS: &[Competitor] = &[
    Competitor { name: "gzip-1",    compress_args: &["gzip", "-1", "-c"],           decompress_args: &["gzip", "-d", "-c"] },
    Competitor { name: "gzip-9",    compress_args: &["gzip", "-9", "-c"],           decompress_args: &["gzip", "-d", "-c"] },
    Competitor { name: "bzip2-9",   compress_args: &["bzip2", "-9", "-c"],          decompress_args: &["bzip2", "-d", "-c"] },
    Competitor { name: "xz-6",      compress_args: &["xz", "-6", "-c"],             decompress_args: &["xz", "-d", "-c"] },
    Competitor { name: "xz-9e",     compress_args: &["xz", "-9e", "-c"],            decompress_args: &["xz", "-d", "-c"] },
    Competitor { name: "zstd-3",    compress_args: &["zstd", "-3", "-c", "--no-progress"], decompress_args: &["zstd", "-d", "-c", "--no-progress"] },
    Competitor { name: "zstd-19",   compress_args: &["zstd", "-19", "-c", "--no-progress"],decompress_args: &["zstd", "-d", "-c", "--no-progress"] },
    Competitor { name: "brotli-11", compress_args: &["brotli", "-q", "11", "-c"],   decompress_args: &["brotli", "-d", "-c"] },
    Competitor { name: "lz4-9",     compress_args: &["lz4", "-9", "-c"],            decompress_args: &["lz4", "-d", "-c"] },
];

// ---------------------------------------------------------------------------
// Corpora
// ---------------------------------------------------------------------------

const CALGARY_FILES: &[&str] = &[
    "bib", "book1", "book2", "geo", "news", "obj1", "obj2",
    "paper1", "paper2", "pic", "progc", "progl", "progp", "trans",
];

const CANTERBURY_FILES: &[&str] = &[
    "alice29.txt", "asyoulik.txt", "cp.html", "fields.c", "grammar.lsp",
    "kennedy.xls", "lcet10.txt", "plrabn12.txt", "ptt5", "sum", "xargs.1",
];

const SILESIA_FILES: &[&str] = &[
    "dickens", "mozilla", "mr", "nci", "ooffice", "osdb",
    "reymont", "samba", "sao", "webster", "xml", "x-ray",
];

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

fn is_available(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn mb(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn speed_mbps(bytes: usize, ms: f64) -> f64 {
    if ms < 0.001 { return 0.0; }
    mb(bytes) / (ms / 1000.0)
}

// ---------------------------------------------------------------------------
// NEXCOMP benchmark
// ---------------------------------------------------------------------------

fn bench_nexcomp(data: &[u8]) -> CompressorResult {
    let t0 = Instant::now();
    let compressed = adaptive_compress(data);
    let comp_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let t0 = Instant::now();
    let decompressed = adaptive_decompress(&compressed);
    let decomp_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let lossless = data == decompressed.as_slice();
    let codec = parse_blocks(&compressed).unwrap().1[0].codec;

    CompressorResult {
        name: format!("NEXCOMP({})", codec.name()),
        compressed_size: compressed.len(),
        bpb: compressed.len() as f64 * 8.0 / data.len() as f64,
        comp_time_ms: comp_ms,
        decomp_time_ms: decomp_ms,
        comp_speed_mbps: speed_mbps(data.len(), comp_ms),
        decomp_speed_mbps: speed_mbps(data.len(), decomp_ms),
        lossless,
    }
}

// ---------------------------------------------------------------------------
// External compressor benchmark
// ---------------------------------------------------------------------------

fn bench_external(
    data: &[u8],
    name: &str,
    comp_args: &[&str],
    decomp_args: &[&str],
) -> Option<CompressorResult> {
    // Sanitize name for file paths
    let safe = name.replace(['/', '\\', ' '], "_");
    let input_path = format!("/tmp/nexcomp_bench_input_{}", safe);
    let comp_path = format!("/tmp/nexcomp_bench_{}.out", safe);
    let decomp_path = format!("/tmp/nexcomp_bench_{}.dec", safe);

    std::fs::write(&input_path, data).ok()?;

    // Compress: cmd [flags] -c inputfile > outputfile
    let t0 = Instant::now();
    let status = Command::new(comp_args[0])
        .args(&comp_args[1..])
        .arg(&input_path)
        .stdout(std::fs::File::create(&comp_path).ok()?)
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    let comp_ms = t0.elapsed().as_secs_f64() * 1000.0;
    if !status.success() {
        let _ = std::fs::remove_file(&input_path);
        let _ = std::fs::remove_file(&comp_path);
        return None;
    }

    let comp_size = std::fs::metadata(&comp_path).ok()?.len() as usize;

    // Decompress: cmd -d -c compfile > decompfile
    let t0 = Instant::now();
    let _status = Command::new(decomp_args[0])
        .args(&decomp_args[1..])
        .arg(&comp_path)
        .stdout(std::fs::File::create(&decomp_path).ok()?)
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    let decomp_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let decompressed = std::fs::read(&decomp_path).ok()?;
    let lossless = data == decompressed.as_slice();

    // Cleanup
    let _ = std::fs::remove_file(&input_path);
    let _ = std::fs::remove_file(&comp_path);
    let _ = std::fs::remove_file(&decomp_path);

    Some(CompressorResult {
        name: name.to_string(),
        compressed_size: comp_size,
        bpb: comp_size as f64 * 8.0 / data.len() as f64,
        comp_time_ms: comp_ms,
        decomp_time_ms: decomp_ms,
        comp_speed_mbps: speed_mbps(data.len(), comp_ms),
        decomp_speed_mbps: speed_mbps(data.len(), decomp_ms),
        lossless,
    })
}

// ---------------------------------------------------------------------------
// Per-file results for the detail table
// ---------------------------------------------------------------------------

struct FileRow {
    filename: String,
    orig_size: usize,
    results: Vec<Option<CompressorResult>>, // index 0 = NEXCOMP, rest = competitors in order
}

// ---------------------------------------------------------------------------
// CSV writer
// ---------------------------------------------------------------------------

struct CsvWriter {
    file: std::fs::File,
}

impl CsvWriter {
    fn open(path: &str) -> Self {
        let mut f = std::fs::File::create(path).expect("cannot create CSV");
        writeln!(
            f,
            "corpus,file,orig_bytes,compressor,compressed_bytes,bpb,comp_ratio_pct,comp_ms,decomp_ms,comp_mbps,decomp_mbps,lossless"
        )
        .unwrap();
        CsvWriter { file: f }
    }

    fn write_row(&mut self, corpus: &str, filename: &str, orig: usize, r: &CompressorResult) {
        let ratio = (1.0 - r.compressed_size as f64 / orig as f64) * 100.0;
        writeln!(
            self.file,
            "{},{},{},{},{},{:.3},{:.1},{:.2},{:.2},{:.2},{:.2},{}",
            corpus,
            filename,
            orig,
            r.name,
            r.compressed_size,
            r.bpb,
            ratio,
            r.comp_time_ms,
            r.decomp_time_ms,
            r.comp_speed_mbps,
            r.decomp_speed_mbps,
            r.lossless,
        )
        .unwrap();
    }
}

// ---------------------------------------------------------------------------
// Corpus runner
// ---------------------------------------------------------------------------

fn detect_available_competitors() -> Vec<usize> {
    let mut available = Vec::new();
    for (i, c) in COMPETITORS.iter().enumerate() {
        if is_available(c.compress_args[0]) {
            available.push(i);
        }
    }
    available
}

fn run_corpus(corpus_name: &str, dir: &str, files: &[&str]) {
    if !Path::new(dir).exists() {
        eprintln!("[SKIP] {} not found at {}", corpus_name, dir);
        return;
    }

    let available = detect_available_competitors();
    let avail_names: Vec<&str> = available.iter().map(|&i| COMPETITORS[i].name).collect();

    eprintln!("\n{}", "=".repeat(130));
    eprintln!(
        "  {} CORPUS -- Professional Benchmark (NEXCOMP v0.1)",
        corpus_name.to_uppercase()
    );
    eprintln!("{}", "=".repeat(130));
    eprintln!(
        "  Available competitors: {} ({} of {})",
        avail_names.join(", "),
        available.len(),
        COMPETITORS.len()
    );
    eprintln!("{}", "-".repeat(130));

    let mut csv = CsvWriter::open("/tmp/nexcomp_results.csv");
    let mut file_rows: Vec<FileRow> = Vec::new();

    // Aggregate accumulators
    let mut tot_orig: usize = 0;
    let mut tot_nexcomp: usize = 0;
    let mut tot_comp: Vec<usize> = vec![0; available.len()];
    let mut nexcomp_lossless = 0usize;
    let mut comp_lossless: Vec<usize> = vec![0; available.len()];
    let mut file_count = 0usize;

    for filename in files {
        let path = format!("{}/{}", dir, filename);
        if !Path::new(&path).exists() {
            eprintln!("  [SKIP] {} not found", path);
            continue;
        }
        let data = std::fs::read(&path).expect("read file");
        if data.is_empty() {
            continue;
        }

        file_count += 1;
        tot_orig += data.len();

        // Benchmark NEXCOMP
        let nxc = bench_nexcomp(&data);
        csv.write_row(corpus_name, filename, data.len(), &nxc);
        tot_nexcomp += nxc.compressed_size;
        if nxc.lossless {
            nexcomp_lossless += 1;
        }
        assert!(nxc.lossless, "NEXCOMP lossless FAILED for {}", filename);

        // Benchmark competitors
        let mut comp_results: Vec<Option<CompressorResult>> = Vec::new();
        for (j, &ci) in available.iter().enumerate() {
            let c = &COMPETITORS[ci];
            let r = bench_external(&data, c.name, c.compress_args, c.decompress_args);
            if let Some(ref res) = r {
                csv.write_row(corpus_name, filename, data.len(), res);
                tot_comp[j] += res.compressed_size;
                if res.lossless {
                    comp_lossless[j] += 1;
                }
            }
            comp_results.push(r);
        }

        file_rows.push(FileRow {
            filename: filename.to_string(),
            orig_size: data.len(),
            results: {
                let mut v: Vec<Option<CompressorResult>> = Vec::new();
                v.push(Some(nxc));
                v.extend(comp_results);
                v
            },
        });
    }

    if file_count == 0 {
        eprintln!("  No files found.");
        return;
    }

    // -----------------------------------------------------------------------
    // Summary table
    // -----------------------------------------------------------------------
    eprintln!("\n{}", "=".repeat(130));
    eprintln!(
        "  {} -- SUMMARY ({} files, {} bytes original)",
        corpus_name.to_uppercase(),
        file_count,
        tot_orig
    );
    eprintln!("{}", "=".repeat(130));
    eprintln!(
        "{:<14} {:>10} {:>8} {:>8} {:>11} {:>12} {:>10}",
        "Compressor", "Comp.Ratio", "bpb", "Avg bpb", "Comp MB/s", "Decomp MB/s", "Lossless"
    );
    eprintln!("{}", "-".repeat(130));

    // NEXCOMP summary
    {
        let ratio = (1.0 - tot_nexcomp as f64 / tot_orig as f64) * 100.0;
        let bpb = tot_nexcomp as f64 * 8.0 / tot_orig as f64;
        // Average speeds across files
        let (mut avg_comp, mut avg_decomp, mut cnt) = (0.0f64, 0.0f64, 0usize);
        for row in &file_rows {
            if let Some(ref r) = row.results[0] {
                avg_comp += r.comp_speed_mbps;
                avg_decomp += r.decomp_speed_mbps;
                cnt += 1;
            }
        }
        if cnt > 0 {
            avg_comp /= cnt as f64;
            avg_decomp /= cnt as f64;
        }
        eprintln!(
            "{:<14} {:>9.1}% {:>8.3} {:>8.3} {:>10.1} {:>11.1} {:>5}/{:<4}",
            "NEXCOMP", ratio, bpb, bpb, avg_comp, avg_decomp, nexcomp_lossless, file_count
        );
    }

    // Competitor summaries
    for (j, &ci) in available.iter().enumerate() {
        let c = &COMPETITORS[ci];
        if tot_comp[j] == 0 {
            continue;
        }
        let ratio = (1.0 - tot_comp[j] as f64 / tot_orig as f64) * 100.0;
        let bpb = tot_comp[j] as f64 * 8.0 / tot_orig as f64;
        let (mut avg_comp, mut avg_decomp, mut cnt) = (0.0f64, 0.0f64, 0usize);
        for row in &file_rows {
            if let Some(ref r) = row.results[j + 1] {
                avg_comp += r.comp_speed_mbps;
                avg_decomp += r.decomp_speed_mbps;
                cnt += 1;
            }
        }
        if cnt > 0 {
            avg_comp /= cnt as f64;
            avg_decomp /= cnt as f64;
        }
        eprintln!(
            "{:<14} {:>9.1}% {:>8.3} {:>8.3} {:>10.1} {:>11.1} {:>5}/{:<4}",
            c.name, ratio, bpb, bpb, avg_comp, avg_decomp, comp_lossless[j], file_count
        );
    }

    // -----------------------------------------------------------------------
    // Per-file detail table
    // -----------------------------------------------------------------------
    eprintln!("\n{}", "=".repeat(130));
    eprintln!(
        "  {} -- PER-FILE DETAIL (bpb)",
        corpus_name.to_uppercase()
    );
    eprintln!("{}", "=".repeat(130));

    // Header
    let mut hdr = format!("{:<15} {:>10}", "File", "Orig");
    hdr.push_str(&format!(" {:>14}", "NEXCOMP"));
    for &ci in &available {
        hdr.push_str(&format!(" {:>10}", COMPETITORS[ci].name));
    }
    hdr.push_str(&format!("  {:<10}", "Best"));
    eprintln!("{}", hdr);
    eprintln!("{}", "-".repeat(hdr.len().max(130)));

    for row in &file_rows {
        let mut line = format!("{:<15} {:>10}", row.filename, row.orig_size);

        // Find best bpb across all
        let mut best_bpb = f64::MAX;
        let mut best_name = String::new();

        // NEXCOMP
        if let Some(ref r) = row.results[0] {
            let codec_name = r.name.clone();
            line.push_str(&format!(" {:>6.3} {:<6}", r.bpb, codec_name.split('(').nth(1).map(|s| s.trim_end_matches(')')).unwrap_or("")));
            if r.bpb < best_bpb {
                best_bpb = r.bpb;
                best_name = "NEXCOMP".to_string();
            }
        } else {
            line.push_str(&format!(" {:>14}", "N/A"));
        }

        // Competitors
        for (j, &ci) in available.iter().enumerate() {
            if let Some(ref r) = row.results[j + 1] {
                line.push_str(&format!(" {:>10.3}", r.bpb));
                if r.bpb < best_bpb {
                    best_bpb = r.bpb;
                    best_name = COMPETITORS[ci].name.to_string();
                }
            } else {
                line.push_str(&format!(" {:>10}", "N/A"));
            }
        }

        line.push_str(&format!("  {:<10}", best_name));
        eprintln!("{}", line);
    }

    // Totals row
    {
        let mut line = format!("{:<15} {:>10}", "TOTAL", tot_orig);
        let nxc_bpb = tot_nexcomp as f64 * 8.0 / tot_orig as f64;
        line.push_str(&format!(" {:>6.3} {:<6}", nxc_bpb, ""));
        let mut best_total_bpb = nxc_bpb;
        let mut best_total_name = "NEXCOMP".to_string();
        for (j, &ci) in available.iter().enumerate() {
            if tot_comp[j] > 0 {
                let bpb = tot_comp[j] as f64 * 8.0 / tot_orig as f64;
                line.push_str(&format!(" {:>10.3}", bpb));
                if bpb < best_total_bpb {
                    best_total_bpb = bpb;
                    best_total_name = COMPETITORS[ci].name.to_string();
                }
            } else {
                line.push_str(&format!(" {:>10}", "N/A"));
            }
        }
        line.push_str(&format!("  {:<10}", best_total_name));
        eprintln!("{}", "-".repeat(line.len().max(130)));
        eprintln!("{}", line);
    }

    eprintln!("\n  CSV results written to /tmp/nexcomp_results.csv");
}

// ---------------------------------------------------------------------------
// Single-file benchmark (for enwik8 etc.)
// ---------------------------------------------------------------------------

fn run_single_file(label: &str, path: &str) {
    if !Path::new(path).exists() {
        eprintln!("[SKIP] {} not found at {}", label, path);
        return;
    }

    let data = std::fs::read(path).expect("read file");
    let filename = Path::new(path)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let available = detect_available_competitors();

    eprintln!("\n{}", "=".repeat(100));
    eprintln!(
        "  {} -- {} ({} bytes)",
        label.to_uppercase(),
        filename,
        data.len()
    );
    eprintln!("{}", "=".repeat(100));
    eprintln!(
        "{:<14} {:>12} {:>8} {:>8} {:>11} {:>12} {:>8}",
        "Compressor", "Compressed", "Ratio%", "bpb", "Comp MB/s", "Decomp MB/s", "Lossless"
    );
    eprintln!("{}", "-".repeat(100));

    let mut csv = CsvWriter::open("/tmp/nexcomp_results.csv");

    // NEXCOMP
    let nxc = bench_nexcomp(&data);
    csv.write_row(label, filename, data.len(), &nxc);
    let ratio = (1.0 - nxc.compressed_size as f64 / data.len() as f64) * 100.0;
    eprintln!(
        "{:<14} {:>12} {:>7.1}% {:>8.3} {:>10.1} {:>11.1} {:>8}",
        nxc.name,
        nxc.compressed_size,
        ratio,
        nxc.bpb,
        nxc.comp_speed_mbps,
        nxc.decomp_speed_mbps,
        if nxc.lossless { "OK" } else { "FAIL" }
    );
    assert!(nxc.lossless, "NEXCOMP lossless FAILED for {}", filename);

    // Competitors
    for &ci in &available {
        let c = &COMPETITORS[ci];
        if let Some(r) = bench_external(&data, c.name, c.compress_args, c.decompress_args) {
            csv.write_row(label, filename, data.len(), &r);
            let ratio = (1.0 - r.compressed_size as f64 / data.len() as f64) * 100.0;
            eprintln!(
                "{:<14} {:>12} {:>7.1}% {:>8.3} {:>10.1} {:>11.1} {:>8}",
                r.name,
                r.compressed_size,
                ratio,
                r.bpb,
                r.comp_speed_mbps,
                r.decomp_speed_mbps,
                if r.lossless { "OK" } else { "FAIL" }
            );
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

/// Quick sanity check -- NEXCOMP only, no external competitors.
/// Runs as part of normal test suite (not ignored).
#[test]
fn bench_quick_calgary() {
    let dir = "/tmp/nexcomp_corpora/calgary";
    if !Path::new(dir).exists() {
        eprintln!("[SKIP] Calgary corpus not found at {}", dir);
        return;
    }

    eprintln!("\n  NEXCOMP Quick Benchmark (Calgary corpus, NEXCOMP only)");
    eprintln!("{}", "-".repeat(70));
    eprintln!(
        "{:<15} {:>10} {:>10} {:>8} {:>6}",
        "File", "Orig", "Compressed", "bpb", "OK"
    );

    let mut total_orig = 0usize;
    let mut total_comp = 0usize;
    let mut all_ok = true;

    for filename in CALGARY_FILES {
        let path = format!("{}/{}", dir, filename);
        if !Path::new(&path).exists() {
            continue;
        }
        let data = std::fs::read(&path).unwrap();
        let r = bench_nexcomp(&data);
        total_orig += data.len();
        total_comp += r.compressed_size;
        if !r.lossless {
            all_ok = false;
        }
        eprintln!(
            "{:<15} {:>10} {:>10} {:>8.3} {:>6}",
            filename,
            data.len(),
            r.compressed_size,
            r.bpb,
            if r.lossless { "OK" } else { "FAIL" }
        );
        assert!(r.lossless, "Lossless FAILED for {}", filename);
    }

    if total_orig > 0 {
        let bpb = total_comp as f64 * 8.0 / total_orig as f64;
        let ratio = (1.0 - total_comp as f64 / total_orig as f64) * 100.0;
        eprintln!("{}", "-".repeat(70));
        eprintln!(
            "{:<15} {:>10} {:>10} {:>8.3}",
            "TOTAL", total_orig, total_comp, bpb
        );
        eprintln!("  Compression ratio: {:.1}%", ratio);
    }

    assert!(all_ok, "Some files failed lossless verification");
}

/// Full Calgary corpus benchmark against all available competitors.
#[test]
#[ignore]
fn bench_calgary_full() {
    run_corpus("Calgary", "/tmp/nexcomp_corpora/calgary", CALGARY_FILES);
}

/// Full Canterbury corpus benchmark against all available competitors.
#[test]
#[ignore]
fn bench_canterbury_full() {
    run_corpus(
        "Canterbury",
        "/tmp/nexcomp_corpora/canterbury",
        CANTERBURY_FILES,
    );
}

/// Full Silesia corpus benchmark against all available competitors.
#[test]
#[ignore]
fn bench_silesia_full() {
    run_corpus("Silesia", "/tmp/nexcomp_corpora/silesia", SILESIA_FILES);
}

/// enwik8 single-file benchmark.
#[test]
#[ignore]
fn bench_enwik8() {
    run_single_file("enwik8", "/tmp/nexcomp_corpora/enwik8/enwik8");
}

/// Run all corpora in sequence.
#[test]
#[ignore]
fn bench_all_corpora() {
    run_corpus("Calgary", "/tmp/nexcomp_corpora/calgary", CALGARY_FILES);
    run_corpus(
        "Canterbury",
        "/tmp/nexcomp_corpora/canterbury",
        CANTERBURY_FILES,
    );
    run_corpus("Silesia", "/tmp/nexcomp_corpora/silesia", SILESIA_FILES);
    run_single_file("enwik8", "/tmp/nexcomp_corpora/enwik8/enwik8");
}
