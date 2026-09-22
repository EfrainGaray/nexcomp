//! Predictor-as-context experiment (docs/research/predictor-context-experiment.md).
//!
//! Every experiment is `#[ignore]`; run one with:
//!   NEXCOMP_CORPORA_DIR=~/corpora cargo test --release \
//!     --test predictor_context_experiment -- --ignored --nocapture --test-threads=1 <name>

use nexcomp::adaptive::{adaptive_compress, adaptive_decompress, parse_blocks, BLOCK_SIZE};
use nexcomp::codecs::stride_cm::{
    self, ALL_MODELS, MODEL_BITS, MODEL_COLUMN, MODEL_DELTA, MODEL_LINEAR, MODEL_PLANE,
};
use rayon::prelude::*;
use std::time::Instant;

const CONTAINER_HEADER: usize = 16;
const CONTAINER_BLOCK: usize = 14;

fn lcg(state: &mut u64) -> u64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    *state >> 33
}

struct Run {
    bytes: usize,
    blocks: Vec<usize>,
    enc_s: f64,
    dec_s: f64,
    lossless: bool,
}

fn run_nexcomp(data: &[u8]) -> Run {
    let t = Instant::now();
    let out = adaptive_compress(data);
    let enc_s = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let lossless = adaptive_decompress(&out) == data;
    let dec_s = t.elapsed().as_secs_f64();
    let blocks = parse_blocks(&out).unwrap().1.iter().map(|b| b.data.len()).collect();
    Run { bytes: out.len(), blocks, enc_s, dec_s, lossless }
}

/// The stride CM as the codec of every 4 MiB container block.
fn run_stride_cm(data: &[u8], models: u8) -> (Run, String) {
    let chunks: Vec<&[u8]> = data.chunks(BLOCK_SIZE).collect();
    let t = Instant::now();
    let coded: Vec<Vec<u8>> = chunks.par_iter().map(|c| stride_cm::encode_with(c, models)).collect();
    let enc_s = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let decoded: Vec<Option<Vec<u8>>> = coded.par_iter().map(|c| stride_cm::decode(c)).collect();
    let dec_s = t.elapsed().as_secs_f64();
    let lossless = decoded.iter().zip(&chunks).all(|(d, c)| d.as_deref() == Some(*c));
    let params = coded
        .first()
        .map_or(String::from("-"), |c| format!("s={} r={}", c[4], u16::from_le_bytes([c[5], c[6]])));
    let blocks: Vec<usize> = coded.iter().map(Vec::len).collect();
    let bytes = CONTAINER_HEADER + blocks.iter().map(|b| CONTAINER_BLOCK + b).sum::<usize>();
    (Run { bytes, blocks, enc_s, dec_s, lossless }, params)
}

/// NEXCOMP with the stride CM as one more candidate per container block.
fn hybrid(nx: &Run, cm: &Run) -> (usize, usize) {
    let mut wins = 0;
    let mut bytes = CONTAINER_HEADER;
    for (a, b) in nx.blocks.iter().zip(&cm.blocks) {
        bytes += CONTAINER_BLOCK + a.min(b);
        wins += usize::from(b < a);
    }
    (bytes, wins)
}

fn mbs(len: usize, secs: f64) -> f64 {
    len as f64 / 1e6 / secs.max(1e-9)
}

fn numeric_sets() -> Vec<(String, Vec<u8>)> {
    let mut s = 5u64;
    let mut sets = vec![(
        "u32 counter, 256 KiB".to_string(),
        (0..65536u32).flat_map(|i| i.to_le_bytes()).collect::<Vec<u8>>(),
    )];
    sets.push((
        "u16 sine + noise, 256 KiB".into(),
        (0..131072u32)
            .flat_map(|i| {
                let v = 20000.0 + 10000.0 * (i as f64 / 160.0).sin() + (lcg(&mut s) % 5) as f64 - 2.0;
                (v as u16).to_le_bytes()
            })
            .collect(),
    ));
    let mut walk = 0i32;
    sets.push((
        "i32 random walk, 256 KiB".into(),
        (0..65536)
            .flat_map(|_| {
                walk += (lcg(&mut s) % 7) as i32 - 3;
                walk.to_le_bytes()
            })
            .collect(),
    ));
    let mut ts = 1_700_000_000_000u64;
    sets.push((
        "u64 timestamps +1000±8, 256 KiB".into(),
        (0..32768)
            .flat_map(|_| {
                ts += 992 + lcg(&mut s) % 17;
                ts.to_le_bytes()
            })
            .collect(),
    ));
    sets.push((
        "f32 damped oscillation, 256 KiB".into(),
        (0..65536)
            .flat_map(|i| {
                let t = i as f32 / 200.0;
                ((t * 3.0).sin() * (-t / 90.0).exp()).to_le_bytes()
            })
            .collect(),
    ));
    sets.push((
        "u8 image 512x512, smooth + noise".into(),
        (0..512 * 512)
            .map(|i| {
                let (x, y) = ((i % 512) as f64, (i / 512) as f64);
                (128.0 + 60.0 * (x / 40.0).sin() * (y / 55.0).cos() + (lcg(&mut s) % 7) as f64 - 3.0) as u8
            })
            .collect(),
    ));
    sets
}

#[test]
#[ignore]
fn numeric_synthetic() {
    println!("\n### Numeric synthetic sets (bytes, container framing included)\n");
    println!("| dataset | orig | NEXCOMP | stride CM | params | hybrid | hybrid vs NEXCOMP | lossless |");
    println!("|---|---|---|---|---|---|---|---|");
    for (name, data) in numeric_sets() {
        let nx = run_nexcomp(&data);
        let (cm, params) = run_stride_cm(&data, ALL_MODELS);
        let (hy, _) = hybrid(&nx, &cm);
        println!(
            "| {name} | {} | {} | {} | {params} | {hy} | {:+.1}% | {} |",
            data.len(),
            nx.bytes,
            cm.bytes,
            100.0 * (hy as f64 / nx.bytes as f64 - 1.0),
            nx.lossless && cm.lossless
        );
    }
}

fn corpus_files(corpus: &str) -> Vec<(String, Vec<u8>)> {
    let dir = std::env::var("NEXCOMP_CORPORA_DIR")
        .unwrap_or_else(|_| format!("{}/corpora", std::env::var("HOME").unwrap_or_default()));
    let path = std::path::Path::new(&dir).join(corpus);
    let mut names: Vec<_> = std::fs::read_dir(&path)
        .map(|rd| rd.filter_map(|e| e.ok()).filter(|e| e.path().is_file()).map(|e| e.path()).collect())
        .unwrap_or_default();
    names.sort();
    names
        .into_iter()
        .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read(&p).unwrap()))
        .collect()
}

fn corpus_table(corpus: &str) {
    let files = corpus_files(corpus);
    if files.is_empty() {
        println!("\n{corpus}: no files found, skipped");
        return;
    }
    println!("\n### {corpus}\n");
    println!("| file | orig | NEXCOMP | bpb | stride CM | bpb | params | hybrid | bpb | blocks won | hybrid vs NEXCOMP | CM enc / dec MB/s | lossless |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    let (mut t_orig, mut t_nx, mut t_cm, mut t_hy, mut t_blocks, mut t_won) = (0, 0, 0, 0, 0, 0);
    for (name, data) in &files {
        let nx = run_nexcomp(data);
        let (cm, params) = run_stride_cm(data, ALL_MODELS);
        let (hy, won) = hybrid(&nx, &cm);
        let bpb = |b: usize| b as f64 * 8.0 / data.len().max(1) as f64;
        println!(
            "| {name} | {} | {} | {:.3} | {} | {:.3} | {params} | {hy} | {:.3} | {won}/{} | {:+.2}% | {:.2} / {:.2} | {} |",
            data.len(),
            nx.bytes,
            bpb(nx.bytes),
            cm.bytes,
            bpb(cm.bytes),
            bpb(hy),
            nx.blocks.len(),
            100.0 * (hy as f64 / nx.bytes as f64 - 1.0),
            mbs(data.len(), cm.enc_s),
            mbs(data.len(), cm.dec_s),
            nx.lossless && cm.lossless,
        );
        t_orig += data.len();
        t_nx += nx.bytes;
        t_cm += cm.bytes;
        t_hy += hy;
        t_blocks += nx.blocks.len();
        t_won += won;
    }
    println!(
        "| **total** | {t_orig} | {t_nx} | {:.3} | {t_cm} | {:.3} | | {t_hy} | {:.3} | {t_won}/{t_blocks} | {:+.2}% | | |",
        t_nx as f64 * 8.0 / t_orig as f64,
        t_cm as f64 * 8.0 / t_orig as f64,
        t_hy as f64 * 8.0 / t_orig as f64,
        100.0 * (t_hy as f64 / t_nx as f64 - 1.0)
    );
}

#[test]
#[ignore]
fn corpora() {
    for corpus in ["calgary", "canterbury", "silesia", "enwik8"] {
        corpus_table(corpus);
    }
}

#[test]
#[ignore]
fn ablation() {
    let mut sets = numeric_sets();
    for (corpus, file) in [
        ("silesia", "mr"),
        ("silesia", "x-ray"),
        ("silesia", "sao"),
        ("silesia", "osdb"),
        ("calgary", "geo"),
        ("calgary", "pic"),
        ("canterbury", "kennedy.xls"),
        ("calgary", "book1"),
    ] {
        if let Some((_, data)) = corpus_files(corpus).into_iter().find(|(n, _)| n == file) {
            sets.push((format!("{file} (first 1 MiB)"), data[..data.len().min(1 << 20)].to_vec()));
        }
    }
    let variants: [(&str, u8); 6] = [
        ("order-0..6 only", 0),
        ("+ column value context", MODEL_COLUMN),
        ("+ linear, delta, plane value contexts", MODEL_COLUMN | MODEL_LINEAR | MODEL_DELTA | MODEL_PLANE),
        ("+ expected-bit models (all)", ALL_MODELS),
        ("expected-bit models without plane", ALL_MODELS & !MODEL_PLANE),
        ("expected-bit linear only", MODEL_LINEAR | MODEL_BITS),
    ];
    println!("\n### Ablation (bytes per set, container framing included)\n");
    let mut header = String::from("| variant | total |");
    let mut sep = String::from("|---|---|");
    for (name, _) in &sets {
        header.push_str(&format!(" {name} |"));
        sep.push_str("---|");
    }
    println!("{header}\n{sep}");
    for (label, models) in variants {
        let sizes: Vec<usize> = sets
            .iter()
            .map(|(name, data)| {
                let (run, _) = run_stride_cm(data, models);
                assert!(run.lossless, "{label} {name}");
                run.bytes
            })
            .collect();
        println!(
            "| {label} | {} | {} |",
            sizes.iter().sum::<usize>(),
            sizes.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(" | ")
        );
    }
    let nx: Vec<usize> = sets.iter().map(|(_, d)| run_nexcomp(d).bytes).collect();
    println!(
        "| NEXCOMP (reference) | {} | {} |",
        nx.iter().sum::<usize>(),
        nx.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(" | ")
    );
}
