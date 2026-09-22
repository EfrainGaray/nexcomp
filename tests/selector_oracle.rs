//! Selector oracle (external audit F-11): what the block selector's gates cost.
//!
//! For every block it compares what the selector produced with the best of all
//! codecs with and without the BCJ filter, no gates at all, and attributes each
//! loss to the codec that was denied.
//!
//!   NEXCOMP_CORPORA_DIR=~/corpora cargo test --release --test selector_oracle \
//!     -- --ignored --nocapture --test-threads=1 oracle
//!
//! `canterbury-large` (E. coli, the bible, world192) is the corpus no gate was
//! ever tuned on; `scripts/corpora.sh` downloads it.

use nexcomp::adaptive::{compress_block_adaptive_pub, encode_with, BLOCK_SIZE, CodecId};
use nexcomp::codecs::bcj_filter;
use rayon::prelude::*;
use std::time::Instant;

const ALL_CODECS: [CodecId; 8] = [
    CodecId::Lz77Huffman,
    CodecId::LzmaStyle,
    CodecId::DeltaAns,
    CodecId::RleHuffman,
    CodecId::Passthrough,
    CodecId::BwtRans,
    CodecId::Ppm,
    CodecId::StrideCm,
];

struct BlockResult {
    selected_len: usize,
    oracle_len: usize,
    oracle_codec: CodecId,
    oracle_bcj: bool,
}

fn oracle_block(block: &[u8]) -> BlockResult {
    let (selected, _) = compress_block_adaptive_pub(block);
    let filtered = bcj_filter::bcj_encode(block);
    let mut candidates: Vec<(usize, CodecId, bool)> = [(block, false), (&filtered[..], true)]
        .into_iter()
        .flat_map(|(data, bcj)| ALL_CODECS.iter().map(move |&codec| (data, bcj, codec)))
        .collect::<Vec<_>>()
        .into_par_iter()
        .filter_map(|(data, bcj, codec)| encode_with(codec, data).map(|out| (out.len(), codec, bcj)))
        .collect();
    candidates.sort_by_key(|&(len, codec, bcj)| (len, codec as u8, bcj));
    let (oracle_len, oracle_codec, oracle_bcj) = candidates[0];
    BlockResult { selected_len: selected.len(), oracle_len, oracle_codec, oracle_bcj }
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

#[test]
#[ignore]
fn oracle() {
    let corpora: Vec<String> = std::env::var("NEXCOMP_ORACLE_CORPORA")
        .unwrap_or_else(|_| "canterbury-large calgary canterbury".into())
        .split_whitespace()
        .map(str::to_string)
        .collect();
    println!("\n### Selector against an ungated oracle\n");
    println!("| corpus | file | blocks | selector | oracle | missed | denied winners |");
    println!("|---|---|---|---|---|---|---|");
    let (mut all_sel, mut all_or, mut all_blocks) = (0usize, 0usize, 0usize);
    let mut denied: Vec<(CodecId, bool, usize, usize)> = Vec::new(); // codec, bcj, blocks, bytes
    for corpus in &corpora {
        let files = corpus_files(corpus);
        if files.is_empty() {
            println!("| {corpus} | *not found* | | | | | |");
            continue;
        }
        let start = Instant::now();
        let (mut c_sel, mut c_or, mut c_blocks) = (0usize, 0usize, 0usize);
        for (name, data) in &files {
            let results: Vec<BlockResult> = data.chunks(BLOCK_SIZE).map(oracle_block).collect();
            let sel: usize = results.iter().map(|r| r.selected_len).sum();
            let or: usize = results.iter().map(|r| r.oracle_len).sum();
            let mut names = Vec::new();
            for r in &results {
                if r.oracle_len < r.selected_len {
                    let entry = denied
                        .iter_mut()
                        .find(|(c, b, _, _)| *c == r.oracle_codec && *b == r.oracle_bcj);
                    let slot = match entry {
                        Some(e) => e,
                        None => {
                            denied.push((r.oracle_codec, r.oracle_bcj, 0, 0));
                            denied.last_mut().unwrap()
                        }
                    };
                    slot.2 += 1;
                    slot.3 += r.selected_len - r.oracle_len;
                    let label = format!("{}{}", r.oracle_codec.name(), if r.oracle_bcj { "+bcj" } else { "" });
                    if !names.contains(&label) {
                        names.push(label);
                    }
                }
            }
            println!(
                "| {corpus} | {name} | {} | {sel} | {or} | {} ({:+.2}%) | {} |",
                results.len(),
                sel - or,
                100.0 * (or as f64 / sel as f64 - 1.0),
                if names.is_empty() { "-".into() } else { names.join(", ") }
            );
            c_sel += sel;
            c_or += or;
            c_blocks += results.len();
        }
        println!(
            "| **{corpus}** | **total** | {c_blocks} | {c_sel} | {c_or} | {} ({:+.2}%) | {:.0}s |",
            c_sel - c_or,
            100.0 * (c_or as f64 / c_sel as f64 - 1.0),
            start.elapsed().as_secs_f64()
        );
        all_sel += c_sel;
        all_or += c_or;
        all_blocks += c_blocks;
    }
    println!(
        "\n**All corpora:** {all_blocks} blocks, selector {all_sel}, oracle {all_or}, missed {} ({:+.3}%)\n",
        all_sel - all_or,
        100.0 * (all_or as f64 / all_sel as f64 - 1.0)
    );
    denied.sort_by_key(|&(_, _, _, bytes)| std::cmp::Reverse(bytes));
    println!("| denied winner | blocks | bytes lost |");
    println!("|---|---|---|");
    for (codec, bcj, blocks, bytes) in denied {
        println!("| {}{} | {blocks} | {bytes} |", codec.name(), if bcj { "+bcj" } else { "" });
    }
}
