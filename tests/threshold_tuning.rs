use nexcomp::classifier_v2::BlockMetrics;
use nexcomp::codecs::bwt_codec;
use nexcomp::codecs::lzma_style;
use nexcomp::lz77::{huffman, Lz77Encoder};

fn compress_baseline(data: &[u8]) -> Vec<u8> {
    let mut enc = Lz77Encoder::new();
    let (tokens, _) = enc.encode(data);
    let bs4k = huffman::huffman_encode_blocked(&tokens, 4096);
    let bs8k = huffman::huffman_encode_blocked(&tokens, 8192);
    let bs16k = huffman::huffman_encode_blocked(&tokens, 16384);
    let mut best = bs8k;
    if bs16k.len() < best.len() {
        best = bs16k;
    }
    if bs4k.len() < best.len() {
        best = bs4k;
    }
    best
}

fn compress_with_bwt_threshold(
    data: &[u8],
    min_size_for_bwt: usize,
    entropy_cap: f64,
) -> (Vec<u8>, &'static str) {
    let metrics = BlockMetrics::compute(data);

    let baseline = compress_baseline(data);
    let lzma = lzma_style::encode_block(data);

    // BWT candidate (only if meets threshold)
    let bwt = if metrics.ascii_ratio > 0.50
        && data.len() >= min_size_for_bwt
        && metrics.entropy <= entropy_cap
    {
        Some(bwt_codec::bwt_compress(data))
    } else {
        None
    };

    let mut best = baseline;
    let mut best_name = "lz77huf";

    if lzma.len() < best.len() {
        best = lzma;
        best_name = "lzma";
    }
    if let Some(b) = bwt {
        if b.len() < best.len() {
            best = b;
            best_name = "bwt";
        }
    }

    (best, best_name)
}

/// List all Calgary corpus files present on disk.
fn calgary_files() -> Vec<(String, Vec<u8>)> {
    let names = [
        "bib", "book1", "book2", "geo", "news", "obj1", "obj2", "paper1", "paper2", "paper3",
        "paper4", "paper5", "paper6", "pic", "progc", "progl", "progp", "trans",
    ];
    let mut files = Vec::new();
    for f in &names {
        let path = format!("/tmp/nexcomp_corpora/calgary/{}", f);
        if let Ok(data) = std::fs::read(&path) {
            files.push((f.to_string(), data));
        }
    }
    files
}

#[test]
fn test_bwt_threshold_sweep() {
    let files = calgary_files();
    if files.is_empty() {
        eprintln!("SKIP: Calgary corpus not found at /tmp/nexcomp_corpora/calgary/");
        return;
    }

    eprintln!("\n=== BWT size-threshold sweep (entropy_cap=8.0, i.e. no entropy filter) ===");
    eprintln!(
        "{:>10}  {:>12}  {:>+12}",
        "threshold", "Calgary bpb", "vs bzip2"
    );

    for &threshold in &[
        0usize, 2048, 4096, 8192, 16384, 32768, 65536, 100_000, 200_000,
        usize::MAX,
    ] {
        let mut tot_orig = 0usize;
        let mut tot_comp = 0usize;

        for (_, data) in &files {
            let (compressed, _) = compress_with_bwt_threshold(data, threshold, 8.0);
            tot_orig += data.len();
            tot_comp += compressed.len();
        }

        let bpb = tot_comp as f64 * 8.0 / tot_orig as f64;
        eprintln!(
            "{:>10}  {:>12.4}  {:>+12.4}",
            threshold, bpb, bpb - 2.109
        );
    }
}

#[test]
fn test_per_file_codec_comparison() {
    let files = calgary_files();
    if files.is_empty() {
        return;
    }

    eprintln!("\n=== Per-file codec comparison (BWT threshold=0, no entropy cap) ===");
    eprintln!(
        "{:<8} {:>8} {:>8} {:>8} {:>8} {:>6} {:>7} {:>8}  winner  bwt_saves",
        "file", "size", "lz77bpb", "lzma_bpb", "bwt_bpb", "ascii", "entropy", "best_bpb"
    );

    let mut grand_orig = 0usize;
    let mut grand_best = 0usize;

    for (name, data) in &files {
        let metrics = BlockMetrics::compute(data);

        let baseline = compress_baseline(data);
        let lz77_bpb = baseline.len() as f64 * 8.0 / data.len() as f64;

        let lzma = lzma_style::encode_block(data);
        let lzma_bpb = lzma.len() as f64 * 8.0 / data.len() as f64;

        let bwt = bwt_codec::bwt_compress(data);
        let bwt_bpb = bwt.len() as f64 * 8.0 / data.len() as f64;

        let mut best_bpb = lz77_bpb;
        let mut winner = "lz77huf";
        let mut best_size = baseline.len();

        if lzma_bpb < best_bpb {
            best_bpb = lzma_bpb;
            winner = "lzma";
            best_size = lzma.len();
        }
        if bwt_bpb < best_bpb {
            best_bpb = bwt_bpb;
            winner = "bwt";
            best_size = bwt.len();
        }

        // How much does BWT save vs LZMA (negative = BWT worse)
        let bwt_saves = lzma_bpb - bwt_bpb;

        grand_orig += data.len();
        grand_best += best_size;

        eprintln!(
            "{:<8} {:>8} {:>8.3} {:>8.3} {:>8.3} {:>6.2} {:>7.2} {:>8.3}  {:<7} {:>+.3}",
            name,
            data.len(),
            lz77_bpb,
            lzma_bpb,
            bwt_bpb,
            metrics.ascii_ratio,
            metrics.entropy,
            best_bpb,
            winner,
            bwt_saves,
        );
    }

    let grand_bpb = grand_best as f64 * 8.0 / grand_orig as f64;
    eprintln!(
        "\nGrand total: {:.4} bpb  (vs bzip2 2.109: {:+.4})",
        grand_bpb,
        grand_bpb - 2.109
    );
}

#[test]
fn test_combined_threshold_sweep() {
    let files = calgary_files();
    if files.is_empty() {
        return;
    }

    eprintln!("\n=== Combined size+entropy sweep ===");

    let sizes = [0usize, 4096, 16384, 32768, 65536];
    let caps = [5.0, 5.5, 6.0, 7.0, 8.0];

    let mut overall_best_bpb = f64::MAX;
    let mut overall_best_params = (0usize, 0.0f64);

    for &sz in &sizes {
        for &cap in &caps {
            let mut tot_orig = 0usize;
            let mut tot_comp = 0usize;

            for (_, data) in &files {
                let (compressed, _) = compress_with_bwt_threshold(data, sz, cap);
                tot_orig += data.len();
                tot_comp += compressed.len();
            }

            let bpb = tot_comp as f64 * 8.0 / tot_orig as f64;
            eprintln!(
                "min_size={:>6} entropy_cap={:.1}  bpb={:.4}  vs_bzip2={:+.4}",
                sz, cap, bpb, bpb - 2.109
            );

            if bpb < overall_best_bpb {
                overall_best_bpb = bpb;
                overall_best_params = (sz, cap);
            }
        }
    }

    eprintln!(
        "\nBest: min_size={}, entropy_cap={:.1} => {:.4} bpb",
        overall_best_params.0, overall_best_params.1, overall_best_bpb
    );
}
