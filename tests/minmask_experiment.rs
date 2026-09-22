//! MinMask research experiments (docs/research/minmask-syndrome-experiment.md).
//!
//! Every experiment is `#[ignore]`; run one with:
//!   NEXCOMP_CORPORA_DIR=~/corpora cargo test --release \
//!     --test minmask_experiment -- --ignored --nocapture --test-threads=1 <name>
//! Corpora layout: <dir>/calgary, <dir>/canterbury, <dir>/silesia, <dir>/enwik8.

use nexcomp::adaptive::{adaptive_compress, adaptive_decompress, parse_blocks, BLOCK_SIZE};
use nexcomp::codecs::minmask::bits::{BitReader, BitVec, BitWriter};
use nexcomp::codecs::minmask::masks::Repr;
use nexcomp::codecs::minmask::residual::{self, Residual, Stats};
use nexcomp::codecs::minmask::{self, enumerative, BlockReport, Options, Search};
use std::collections::BTreeMap;
use std::time::Instant;

const CONTAINER_HEADER: usize = 16;
const CONTAINER_BLOCK: usize = 14;

fn lcg(state: &mut u64) -> u64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    *state >> 33
}

fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut s = seed;
    (0..len).map(|_| lcg(&mut s) as u8).collect()
}

fn flip_bits(data: &mut [u8], flips: usize, s: &mut u64) {
    let mut done = 0;
    let mut seen = std::collections::HashSet::new();
    while done < flips {
        let p = (lcg(s) % (8 * data.len() as u64)) as usize;
        if seen.insert(p) {
            data[p / 8] ^= 1 << (7 - p % 8);
            done += 1;
        }
    }
}

/// External compressor size, if the tool is installed.
fn external(tool: &str, args: &[&str], data: &[u8]) -> Option<usize> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new(tool).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    let input = data.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let out = child.wait_with_output().ok()?;
    writer.join().ok()?.ok()?;
    out.status.success().then_some(out.stdout.len())
}

/// MinMask used as the codec of every 4 MiB container block.
struct MinMaskRun {
    bytes: usize,
    block_payloads: Vec<usize>,
    reports: Vec<minmask::Report>,
    enc_s: f64,
    dec_s: f64,
    lossless: bool,
}

fn run_minmask(data: &[u8], opts: &Options) -> MinMaskRun {
    let t = Instant::now();
    let encoded: Vec<(Vec<u8>, minmask::Report)> = data.chunks(BLOCK_SIZE).map(|c| minmask::compress_report(c, opts)).collect();
    let enc_s = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let mut restored = Vec::with_capacity(data.len());
    let mut lossless = true;
    for (out, _) in &encoded {
        match minmask::decompress(out) {
            Ok(block) => restored.extend_from_slice(&block),
            Err(_) => lossless = false,
        }
    }
    let dec_s = t.elapsed().as_secs_f64();
    lossless &= restored == data;
    let block_payloads: Vec<usize> = encoded.iter().map(|(o, _)| o.len()).collect();
    MinMaskRun {
        bytes: CONTAINER_HEADER + block_payloads.iter().map(|p| CONTAINER_BLOCK + p).sum::<usize>(),
        block_payloads,
        reports: encoded.into_iter().map(|(_, r)| r).collect(),
        enc_s,
        dec_s,
        lossless,
    }
}

struct NexcompRun {
    bytes: usize,
    block_payloads: Vec<usize>,
    enc_s: f64,
    dec_s: f64,
    lossless: bool,
}

fn run_nexcomp(data: &[u8]) -> NexcompRun {
    let t = Instant::now();
    let out = adaptive_compress(data);
    let enc_s = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let lossless = adaptive_decompress(&out) == data;
    let dec_s = t.elapsed().as_secs_f64();
    let block_payloads = parse_blocks(&out).unwrap().1.iter().map(|b| b.data.len()).collect();
    NexcompRun { bytes: out.len(), block_payloads, enc_s, dec_s, lossless }
}

/// NEXCOMP with MinMask as one more candidate per container block.
fn hybrid_bytes(nx: &NexcompRun, mm: &MinMaskRun) -> usize {
    CONTAINER_HEADER
        + nx.block_payloads.iter().zip(&mm.block_payloads).map(|(a, b)| CONTAINER_BLOCK + a.min(b)).sum::<usize>()
}

/// Aggregate accounting over all blocks of a run.
#[derive(Default)]
struct Summary {
    n_bits: usize,
    mask_bits: usize,
    flag_bits: usize,
    header_bits: usize,
    residual_bits: usize,
    weight: usize,
    floor_bits: f64,
    blocks: usize,
    candidates: usize,
    family: BTreeMap<&'static str, usize>,
    codec: BTreeMap<&'static str, usize>,
    configs: Vec<String>,
}

impl Summary {
    fn of(run: &MinMaskRun) -> Self {
        let mut s = Summary::default();
        for r in &run.reports {
            s.header_bits += 8 * 5;
            let cfg = format!("{}/{}", if r.repr == Repr::Linear { "lin" } else { "planes" }, r.block_bits);
            if !s.configs.contains(&cfg) {
                s.configs.push(cfg);
            }
            for b in &r.blocks {
                s.add(b);
            }
        }
        s
    }

    fn add(&mut self, b: &BlockReport) {
        self.n_bits += b.n;
        self.mask_bits += b.mask_bits;
        self.flag_bits += 1 + residual::ID_BITS as usize;
        self.residual_bits += b.residual_bits;
        self.weight += b.weight;
        self.floor_bits += b.sparse_floor;
        self.blocks += 1;
        self.candidates += b.candidates;
        *self.family.entry(b.family).or_default() += b.n;
        *self.codec.entry(b.residual.name()).or_default() += b.n;
    }

    fn top(map: &BTreeMap<&'static str, usize>) -> String {
        let total: usize = map.values().sum::<usize>().max(1);
        let mut v: Vec<_> = map.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        v.iter().take(2).map(|(k, n)| format!("{k} {:.0}%", 100.0 * **n as f64 / total as f64)).collect::<Vec<_>>().join(", ")
    }
}

fn bpb(bytes: usize, len: usize) -> f64 {
    if len == 0 { 0.0 } else { bytes as f64 * 8.0 / len as f64 }
}

// ---------------------------------------------------------------------------
// Experiment B: residual codecs on controlled sparse residuals
// ---------------------------------------------------------------------------

fn codec_bits(codec: Residual, e: &BitVec) -> Option<usize> {
    let st = Stats::of(e);
    let cost = residual::cost(codec, e, &st)?;
    let mut w = BitWriter::new();
    residual::encode(codec, e, &st, &mut w);
    assert_eq!(w.bits(), cost, "{} cost mismatch", codec.name());
    let bytes = w.finish();
    assert_eq!(residual::decode(codec, e.len, &mut BitReader::new(&bytes)).as_ref(), Some(e), "{} roundtrip", codec.name());
    Some(cost)
}

#[test]
#[ignore]
fn experiment_b_sparse_residual_codecs() {
    for n in [255usize, 1023, 8191] {
        println!("\n### Experiment B: n = {n}, mean serialized bits over 200 random residuals per t\n");
        println!("| t | log2 C(n,t) | enum rank | bch syndrome | enum total | bch total | positions | rice | rle | arith | rans | raw | enum excess | bch excess |");
        println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|");
        let mut s = 1234u64;
        for t in [1usize, 2, 4, 8, 16, 32, 64, 128] {
            if t > n / 2 {
                continue;
            }
            let trials = 200;
            let mut sums: BTreeMap<Residual, (usize, usize)> = BTreeMap::new();
            for _ in 0..trials {
                let mut e = BitVec::zeros(n);
                let mut placed = 0;
                while placed < t {
                    let p = (lcg(&mut s) % n as u64) as usize;
                    if !e.get(p) {
                        e.set(p, true);
                        placed += 1;
                    }
                }
                for codec in residual::ALL {
                    if let Some(b) = codec_bits(codec, &e) {
                        let entry = sums.entry(codec).or_default();
                        entry.0 += b;
                        entry.1 += 1;
                    }
                }
            }
            let mean = |c: Residual| sums.get(&c).filter(|v| v.1 == trials).map(|v| v.0 as f64 / trials as f64);
            let fmt = |v: Option<f64>| v.map_or("n/a".to_string(), |x| format!("{x:.1}"));
            let floor = enumerative::log2_binom(n, t);
            let enum_rank = enumerative::rank_width(n, t) as f64;
            let bch_syn = nexcomp::codecs::minmask::bch::for_len(n).and_then(|c| c.syndrome_bits(t)).map(|b| b as f64);
            println!(
                "| {t} | {floor:.1} | {enum_rank:.0} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {:+.1} | {} |",
                fmt(bch_syn),
                fmt(mean(Residual::Enum)),
                fmt(mean(Residual::Bch)),
                fmt(mean(Residual::SparsePos)),
                fmt(mean(Residual::Rice)),
                fmt(mean(Residual::Rle)),
                fmt(mean(Residual::Arith)),
                fmt(mean(Residual::Rans)),
                fmt(mean(Residual::Raw)),
                enum_rank - floor,
                bch_syn.map_or("n/a".into(), |b| format!("{:+.1}", b - floor)),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Synthetic datasets A, C, D, E, F
// ---------------------------------------------------------------------------

fn prbs15(len: usize, seed: u16) -> Vec<u8> {
    let mut bits = Vec::with_capacity(8 * len);
    for k in 0..8 * len {
        let b = if k < 15 { (seed >> k) & 1 == 1 } else { bits[k - 15] ^ bits[k - 1] };
        bits.push(b);
    }
    bits.chunks(8).map(|c| c.iter().fold(0u8, |a, &b| (a << 1) | u8::from(b))).collect()
}

fn synthetic() -> Vec<(&'static str, String, Vec<u8>)> {
    let mut out: Vec<(&'static str, String, Vec<u8>)> = Vec::new();
    let mut s = 77u64;
    // A: exact predictors
    out.push(("A", "period-8-bit pattern 64 KiB".into(), vec![0xCA; 65536]));
    out.push(("A", "PRBS15 stream 64 KiB".into(), prbs15(65536, 0x2F1A)));
    out.push(("A", "bytes 3i mod 256, 64 KiB".into(), (0..65536u32).map(|i| (3 * i) as u8).collect()));
    // C: random
    out.push(("C", "random 64 KiB".into(), random_bytes(65536, 1)));
    out.push(("C", "random 1 MiB".into(), random_bytes(1 << 20, 2)));
    // D: repeating random periods
    for period in [3usize, 7, 64, 1000, 4096] {
        let base = random_bytes(period, period as u64);
        out.push(("D", format!("period {period} B, 256 KiB"), base.iter().copied().cycle().take(262144).collect()));
    }
    // E: slowly changing numeric data
    out.push(("E", "u32 counter, 256 KiB".into(), (0..65536u32).flat_map(|i| i.to_le_bytes()).collect()));
    out.push((
        "E",
        "u16 sine + noise, 256 KiB".into(),
        (0..131072u32)
            .flat_map(|i| {
                let v = 20000.0 + 10000.0 * (i as f64 / 160.0).sin() + (lcg(&mut s) % 5) as f64 - 2.0;
                (v as u16).to_le_bytes()
            })
            .collect(),
    ));
    let mut walk = 0i32;
    out.push((
        "E",
        "i32 random walk, 256 KiB".into(),
        (0..65536)
            .flat_map(|_| {
                walk += (lcg(&mut s) % 7) as i32 - 3;
                walk.to_le_bytes()
            })
            .collect(),
    ));
    let mut ts = 1_700_000_000_000u64;
    out.push((
        "E",
        "u64 timestamps +1000±8, 256 KiB".into(),
        (0..32768)
            .flat_map(|_| {
                ts += 992 + lcg(&mut s) % 17;
                ts.to_le_bytes()
            })
            .collect(),
    ));
    // F: previous block with sparse flips
    for block in [1024usize, 4096] {
        for flips in [0usize, 1, 4, 16, 64] {
            let base = random_bytes(block, block as u64);
            let mut data = Vec::with_capacity(64 * block);
            for _ in 0..64 {
                let mut copy = base.clone();
                flip_bits(&mut copy, flips, &mut s);
                data.extend_from_slice(&copy);
            }
            out.push(("F", format!("{block} B block x64, {flips} flips/copy"), data));
        }
    }
    out
}

#[test]
#[ignore]
fn experiment_synthetic() {
    println!("\n### Synthetic experiments A, C, D, E, F (sizes in bytes, container framing included)\n");
    println!("| exp | dataset | orig | NEXCOMP | MinMask fast | MinMask exh | xz -9e | exh vs NEXCOMP | best family | residual | weight / bits | mask+flag bits | lossless |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    for (exp, name, data) in synthetic() {
        let nx = run_nexcomp(&data);
        let fast = run_minmask(&data, &Options::fast());
        let exh = run_minmask(&data, &Options::exhaustive());
        let sum = Summary::of(&exh);
        let xz = external("xz", &["-9e", "-c"], &data).map_or("n/a".into(), |v| v.to_string());
        println!(
            "| {exp} | {name} | {} | {} | {} | {} | {xz} | {:+.1}% | {} | {} | {} / {} | {} | {} |",
            data.len(),
            nx.bytes,
            fast.bytes,
            exh.bytes,
            100.0 * (exh.bytes as f64 / nx.bytes as f64 - 1.0),
            Summary::top(&sum.family),
            Summary::top(&sum.codec),
            sum.weight,
            sum.n_bits,
            sum.mask_bits + sum.flag_bits,
            nx.lossless && fast.lossless && exh.lossless,
        );
    }
}

// ---------------------------------------------------------------------------
// Standard corpora
// ---------------------------------------------------------------------------

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

fn corpus_table(corpus: &str, exhaustive_sample: Option<usize>) {
    let files = corpus_files(corpus);
    if files.is_empty() {
        println!("\n{corpus}: no files found, skipped");
        return;
    }
    println!("\n### {corpus}\n");
    println!("| file | orig | NEXCOMP | bpb | MinMask fast | bpb | hybrid | xz -9e | bzip2 -9 | config | best family | residual | weight/bits | mask+flag bits | residual bits | overhead bits | gain vs NEXCOMP | enc MB/s nx / mm | dec MB/s nx / mm | lossless |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    let (mut t_orig, mut t_nx, mut t_mm, mut t_hy) = (0usize, 0usize, 0usize, 0usize);
    let mut exh_rows = Vec::new();
    for (name, data) in &files {
        let nx = run_nexcomp(data);
        let mm = run_minmask(data, &Options::fast());
        let sum = Summary::of(&mm);
        let hy = hybrid_bytes(&nx, &mm);
        let xz = external("xz", &["-9e", "-c"], data).map_or("n/a".into(), |v| v.to_string());
        let bz = external("bzip2", &["-9", "-c"], data).map_or("n/a".into(), |v| v.to_string());
        let mbs = |secs: f64| data.len() as f64 / 1e6 / secs.max(1e-9);
        println!(
            "| {name} | {} | {} | {:.3} | {} | {:.3} | {hy} | {xz} | {bz} | {} | {} | {} | {}/{} | {} | {} | {} | {:+.1}% | {:.2} / {:.2} | {:.1} / {:.1} | {} |",
            data.len(),
            nx.bytes,
            bpb(nx.bytes, data.len()),
            mm.bytes,
            bpb(mm.bytes, data.len()),
            sum.configs.join(" "),
            Summary::top(&sum.family),
            Summary::top(&sum.codec),
            sum.weight,
            sum.n_bits,
            sum.mask_bits + sum.flag_bits,
            sum.residual_bits,
            sum.mask_bits + sum.flag_bits + sum.header_bits + 8 * (CONTAINER_HEADER + CONTAINER_BLOCK * mm.block_payloads.len()),
            100.0 * (1.0 - mm.bytes as f64 / nx.bytes as f64),
            mbs(nx.enc_s),
            mbs(mm.enc_s),
            mbs(nx.dec_s),
            mbs(mm.dec_s),
            nx.lossless && mm.lossless,
        );
        t_orig += data.len();
        t_nx += nx.bytes;
        t_mm += mm.bytes;
        t_hy += hy;
        if let Some(limit) = exhaustive_sample {
            let sample = &data[..data.len().min(limit)];
            let nx_s = run_nexcomp(sample);
            let exh = run_minmask(sample, &Options::exhaustive());
            let s = Summary::of(&exh);
            exh_rows.push(format!(
                "| {name} | {} | {} | {} | {:+.1}% | {} | {} | {} | {:.1} | {:.3} | {} |",
                sample.len(),
                nx_s.bytes,
                exh.bytes,
                100.0 * (exh.bytes as f64 / nx_s.bytes as f64 - 1.0),
                s.configs.join(" "),
                Summary::top(&s.family),
                Summary::top(&s.codec),
                s.candidates as f64 / s.blocks.max(1) as f64,
                sample.len() as f64 / 1e6 / exh.enc_s.max(1e-9),
                exh.lossless,
            ));
        }
    }
    println!(
        "| **total** | {t_orig} | {t_nx} | {:.3} | {t_mm} | {:.3} | {t_hy} | | | | | | | | | | {:+.1}% | | | |",
        bpb(t_nx, t_orig),
        bpb(t_mm, t_orig),
        100.0 * (1.0 - t_mm as f64 / t_nx as f64)
    );
    println!("\nHybrid (NEXCOMP + MinMask as a per-block candidate) saves {} bytes of {t_nx}.", t_nx - t_hy);
    if !exh_rows.is_empty() {
        println!("\n#### {corpus}: MinMask exhaustive{}\n", exhaustive_sample.filter(|&l| l < usize::MAX).map_or(String::new(), |l| format!(" on the first {} KiB of each file", l / 1024)));
        println!("| file | bytes | NEXCOMP | MinMask exh | exh vs NEXCOMP | config | best family | residual | candidates/block | enc MB/s | lossless |");
        println!("|---|---|---|---|---|---|---|---|---|---|---|");
        for row in exh_rows {
            println!("{row}");
        }
    }
}

#[test]
#[ignore]
fn corpus_calgary_canterbury() {
    corpus_table("calgary", Some(usize::MAX));
    corpus_table("canterbury", Some(usize::MAX));
}

#[test]
#[ignore]
fn corpus_silesia_enwik8() {
    corpus_table("silesia", Some(256 * 1024));
    corpus_table("enwik8", Some(256 * 1024));
}

// ---------------------------------------------------------------------------
// Ablation
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn ablation() {
    let mut sets: Vec<(String, Vec<u8>)> = corpus_files("calgary");
    let synth = synthetic();
    for (exp, name, data) in synth {
        if name.contains("u32 counter") || name.contains("1024 B block x64, 4 flips") || name.contains("period 1000") || name.contains("PRBS15") {
            sets.push((format!("{exp}: {name}"), data));
        }
    }
    let base = Options {
        reprs: vec![Repr::Linear, Repr::Planes],
        block_ids: vec![2, 8, 9],
        residuals: residual::ALL.to_vec(),
        search: Search::Fast,
    };
    let variants: Vec<(&str, Options)> = vec![
        ("masks + raw residual", Options { residuals: vec![Residual::Raw], ..base.clone() }),
        ("masks + sparse positions", Options { residuals: vec![Residual::SparsePos], ..base.clone() }),
        ("masks + Rice", Options { residuals: vec![Residual::Rice], ..base.clone() }),
        ("masks + enumerative", Options { residuals: vec![Residual::Enum], ..base.clone() }),
        ("masks + BCH", Options { residuals: vec![Residual::Bch], ..base.clone() }),
        ("masks + arith", Options { residuals: vec![Residual::Arith], ..base.clone() }),
        ("linear layout only", Options { reprs: vec![Repr::Linear], ..base.clone() }),
        ("bit-plane layout only", Options { reprs: vec![Repr::Planes], ..base.clone() }),
        ("all residual codecs, FAST search", base.clone()),
        ("EXHAUSTIVE mask search", Options { search: Search::Exhaustive, ..base.clone() }),
    ];
    println!("\n### Ablation (layouts linear+planes, blocks 1023 b / 1 KiB / 4 KiB unless stated; raw is always the fallback)\n");
    let mut header = String::from("| variant | Calgary total |");
    let mut sep = String::from("|---|---|");
    let synth_names: Vec<&String> = sets.iter().map(|(n, _)| n).filter(|n| n.contains(':')).collect();
    for n in &synth_names {
        header.push_str(&format!(" {n} |"));
        sep.push_str("---|");
    }
    println!("{header}\n{sep}");
    for (label, opts) in variants {
        let mut calgary = 0usize;
        let mut synth_cols = Vec::new();
        for (name, data) in &sets {
            let run = run_minmask(data, &opts);
            assert!(run.lossless, "{label} {name}");
            if name.contains(':') {
                synth_cols.push(run.bytes);
            } else {
                calgary += run.bytes;
            }
        }
        println!("| {label} | {calgary} | {} |", synth_cols.iter().map(|b| b.to_string()).collect::<Vec<_>>().join(" | "));
    }
    let nexcomp_calgary: usize = sets.iter().filter(|(n, _)| !n.contains(':')).map(|(_, d)| run_nexcomp(d).bytes).sum();
    let nexcomp_synth: Vec<String> = sets.iter().filter(|(n, _)| n.contains(':')).map(|(_, d)| run_nexcomp(d).bytes.to_string()).collect();
    println!("| NEXCOMP (reference) | {nexcomp_calgary} | {} |", nexcomp_synth.join(" | "));
}
