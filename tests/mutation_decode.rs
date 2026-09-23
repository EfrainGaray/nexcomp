//! Deterministic mutation harness (external audit, P0 item 3): every block
//! decoder and the container must answer corrupt input with an error, never a
//! panic or an allocation beyond a fixed bound. It runs in every `cargo test`;
//! the cargo-fuzz targets in `fuzz/` explore further.
//!
//! `NEXCOMP_MUTATIONS=<n>` sets the mutations per base payload (default 300).

use nexcomp::adaptive::{
    adaptive_compress, decompress_block_adaptive, encode_with, try_adaptive_decompress_limited, CodecId,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

struct Tracking;

static LARGEST: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call forwards unchanged to the system allocator; only the
// requested size is recorded.
unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LARGEST.fetch_max(layout.size(), Ordering::Relaxed);
        System.alloc(layout)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        LARGEST.fetch_max(layout.size(), Ordering::Relaxed);
        System.alloc_zeroed(layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LARGEST.fetch_max(new_size, Ordering::Relaxed);
        System.realloc(ptr, layout, new_size)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
}

#[global_allocator]
static ALLOCATOR: Tracking = Tracking;

/// Largest single allocation any decode below may make.
const ALLOC_LIMIT: usize = 64 << 20;

/// The allocator is global and tests in a binary run in parallel, so a test
/// that measures allocations has to be the only one allocating.
static MEASURING: Mutex<()> = Mutex::new(());

fn measuring() -> MutexGuard<'static, ()> {
    MEASURING.lock().unwrap_or_else(|e| e.into_inner())
}

fn mutations() -> usize {
    std::env::var("NEXCOMP_MUTATIONS").ok().and_then(|v| v.parse().ok()).unwrap_or(300)
}

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// One random corruption of `base`.
fn mutate(base: &[u8], rng: &mut Lcg, expected_len: usize) -> Vec<u8> {
    let mut m = base.to_vec();
    let at = rng.below(m.len());
    match rng.below(7) {
        0 => m[at] ^= 1 << rng.below(8),
        1 => m[at] = rng.next() as u8,
        2 => m.truncate(at),
        3 => m.insert(at, rng.next() as u8),
        4 => {
            m.remove(at);
        }
        5 => {
            let interesting = [0, 1, 0x7FFF_FFFF, u32::MAX, expected_len as u32 + 1, base.len() as u32];
            let v = interesting[rng.below(interesting.len())].to_le_bytes();
            for (i, b) in v.iter().enumerate() {
                if let Some(slot) = m.get_mut(at + i) {
                    *slot = *b;
                }
            }
        }
        _ => {
            for _ in 0..1 + rng.below(8) {
                let at = rng.below(m.len());
                m[at] = rng.next() as u8;
            }
        }
    }
    m
}

fn text(len: usize) -> Vec<u8> {
    let words = ["the ", "compressor ", "block ", "of ", "and ", "model ", "context ", "a ", "mixing\n"];
    let mut rng = Lcg(7);
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        out.extend_from_slice(words[rng.below(words.len())].as_bytes());
    }
    out.truncate(len);
    out
}

fn counters(len: usize) -> Vec<u8> {
    (0..len as u32 / 4).flat_map(|i| (1000 + 3 * i).to_le_bytes()).collect()
}

fn runs(len: usize) -> Vec<u8> {
    let mut rng = Lcg(11);
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let byte = rng.next() as u8 & 0x7;
        out.extend(std::iter::repeat_n(byte, 1 + rng.below(40)));
    }
    out.truncate(len);
    out
}

/// x86-looking code, so the writer applies the BCJ filter.
fn exe(len: usize) -> Vec<u8> {
    (0..len as u32 / 9).flat_map(|i| [[0xE8].as_slice(), &(i * 16).to_le_bytes(), &[0x55, 0x48, 0x89, 0xE5]].concat()).collect()
}

/// One valid payload per codec: (codec, original, payload).
fn base_payloads() -> Vec<(CodecId, Vec<u8>, Vec<u8>)> {
    use CodecId::*;
    let (text, nums, runs) = (text(8192), counters(8192), runs(8192));
    [
        (Lz77Huffman, &text),
        (LzmaStyle, &text),
        (DeltaAns, &nums),
        (RleHuffman, &runs),
        (RleHuffman, &text),
        (Passthrough, &runs),
        (BwtRans, &text),
        (Ppm, &text),
        (StrideCm, &nums),
    ]
    .into_iter()
    .map(|(codec, data)| (codec, data.clone(), encode_with(codec, data).unwrap()))
    .collect()
}

#[test]
fn corrupt_block_payloads_never_panic() {
    let _measuring = measuring();
    let n = mutations();
    let mut failures = Vec::new();
    for (codec, original, payload) in base_payloads() {
        assert_eq!(decompress_block_adaptive(codec, &payload, original.len()).unwrap(), original);
        let mut rng = Lcg(codec as u64 + 1);
        for i in 0..n {
            let m = mutate(&payload, &mut rng, original.len());
            LARGEST.store(0, Ordering::Relaxed);
            let outcome = catch_unwind(AssertUnwindSafe(|| decompress_block_adaptive(codec, &m, original.len())));
            let largest = LARGEST.load(Ordering::Relaxed);
            if outcome.is_err() || largest > ALLOC_LIMIT {
                failures.push(format!("{} mutation {i}: panic={} largest={largest}", codec.name(), outcome.is_err()));
            }
        }
    }
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn corrupt_containers_never_panic() {
    let _measuring = measuring();
    let n = mutations();
    let mut failures = Vec::new();
    // A text block, a numeric block and a BCJ block, framed by the real writer.
    for data in [text(8192), counters(8192), exe(18432)] {
        let file = adaptive_compress(&data);
        let mut rng = Lcg(data.len() as u64);
        for i in 0..n {
            let m = mutate(&file, &mut rng, data.len());
            LARGEST.store(0, Ordering::Relaxed);
            let outcome = catch_unwind(AssertUnwindSafe(|| try_adaptive_decompress_limited(&m, 1 << 20)));
            let largest = LARGEST.load(Ordering::Relaxed);
            let decoder_panic = matches!(outcome, Ok(Err(nexcomp::adaptive::AdaptiveError::DecoderPanic(_))));
            if outcome.is_err() || decoder_panic || largest > ALLOC_LIMIT {
                let desc = match &outcome {
                    Err(_) => "panic".to_string(),
                    Ok(Err(e)) => format!("{e:?}"),
                    Ok(Ok(v)) => format!("Ok({} bytes)", v.len()),
                };
                failures.push(format!("container mutation {i}: {desc} largest={largest}"));
            }
        }
    }
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

/// Seed corpora for the cargo-fuzz targets in `fuzz/`:
///   cargo test --release --test mutation_decode -- --ignored write_fuzz_seeds
#[test]
#[ignore]
fn write_fuzz_seeds() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/corpus");
    let write = |target: &str, name: &str, bytes: &[u8]| {
        let dir = root.join(target);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), bytes).unwrap();
    };
    for (name, data) in [("text", text(2048)), ("counters", counters(2048)), ("runs", runs(2048)), ("exe", exe(2048))] {
        write("decompress", name, &adaptive_compress(&data));
        write("roundtrip", name, &data);
        for id in 0..9u8 {
            write("codec-roundtrip", &format!("{name}-{id}"), &[&[id][..], &data].concat());
            let Some(payload) = CodecId::from_u8(id).and_then(|codec| encode_with(codec, &data)) else { continue };
            let len = (data.len() as u16).to_le_bytes();
            write("block", &format!("{name}-{id}"), &[&[id][..], &len, &payload].concat());
        }
    }
}
