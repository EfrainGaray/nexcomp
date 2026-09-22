//! Malformed NX13 blocks must be rejected before any codec allocates or loops
//! beyond what the block itself could produce (external audit F-01).
//!
//! A tracking allocator records the largest single allocation request; every
//! hostile case below declares a 1-byte block, so no request may come close
//! to the limit.

use nexcomp::adaptive::{adaptive_compress, try_adaptive_decompress, try_adaptive_decompress_limited, CodecId};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

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

/// Largest single allocation a 1-byte hostile block may cause.
const LIMIT: usize = 64 << 20;

/// An NX13 file holding one block of `block_len` declared bytes.
fn container(file_len: u64, block_len: u32, codec: u8, payload: &[u8]) -> Vec<u8> {
    let mut c = b"NX13".to_vec();
    c.extend_from_slice(&file_len.to_le_bytes());
    c.extend_from_slice(&1u32.to_le_bytes());
    c.push(codec);
    c.push(0);
    c.extend_from_slice(&block_len.to_le_bytes());
    c.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    c.extend_from_slice(&0u32.to_le_bytes()); // CRC-32 (never reached)
    c.extend_from_slice(payload);
    c
}

fn le(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

fn hostile_cases() -> Vec<(&'static str, Vec<u8>)> {
    let big = u32::MAX;
    let one_block = |codec: CodecId, payload: Vec<u8>| container(1, 1, codec as u8, &payload);
    let mut rle_mode0 = b"RHF1".to_vec();
    rle_mode0.push(0);
    rle_mode0.extend_from_slice(&le(big));
    rle_mode0.push(0);
    rle_mode0.extend_from_slice(&[0x11; 128]);
    let mut rle_mode1 = b"RHF1".to_vec();
    rle_mode1.push(0);
    rle_mode1.extend_from_slice(&le(1));
    rle_mode1.push(1);
    rle_mode1.extend_from_slice(&le(big));
    rle_mode1.extend_from_slice(&1u16.to_le_bytes());
    rle_mode1.extend_from_slice(&[7, 1]);
    rle_mode1.extend_from_slice(&[0x11; 128]);
    let mut delta = vec![0u8, 1];
    delta.extend_from_slice(&le(1));
    delta.extend_from_slice(&le(big));
    delta.extend_from_slice(&[0u8; 1024]);
    delta.extend_from_slice(&le(0));
    vec![
        // The audit's proof of concept: the baseline declares u32::MAX tokens.
        ("baseline n_tokens", one_block(CodecId::Lz77Huffman, [le(big), le(0)].concat())),
        ("baseline block tokens", one_block(CodecId::Lz77Huffman, [le(1), le(1), le(big), le(0)].concat())),
        ("lzma orig_len", one_block(CodecId::LzmaStyle, [le(big).to_vec(), vec![0; 8]].concat())),
        ("ppm orig_len", one_block(CodecId::Ppm, [le(big).to_vec(), vec![0; 8]].concat())),
        ("rle mode 0 orig_len", one_block(CodecId::RleHuffman, [rle_mode0, vec![0xFF; 8]].concat())),
        ("rle mode 1 runs", one_block(CodecId::RleHuffman, [rle_mode1, vec![0xFF; 8]].concat())),
        ("delta lane length", one_block(CodecId::DeltaAns, delta)),
        ("bwt orig_len", one_block(CodecId::BwtRans, [le(big), le(1)].concat())),
        ("stride-cm orig_len", one_block(CodecId::StrideCm, [le(big).to_vec(), vec![4, 0, 0, 31], vec![0; 8]].concat())),
        ("store length", one_block(CodecId::Passthrough, vec![0; 16])),
        ("block larger than 4 MiB", container(u64::from(big), big, CodecId::Passthrough as u8, &[0; 8])),
        ("block count disagrees with length", container(8 << 20, 4 << 20, CodecId::Passthrough as u8, &[0; 8])),
        ("block length disagrees with file length", container(10, 1, CodecId::Passthrough as u8, &[0])),
    ]
}

#[test]
fn hostile_blocks_are_rejected_without_large_allocations() {
    for (name, file) in hostile_cases() {
        LARGEST.store(0, Ordering::Relaxed);
        let result = std::panic::catch_unwind(|| try_adaptive_decompress(&file));
        let largest = LARGEST.load(Ordering::Relaxed);
        assert!(matches!(result, Ok(Err(_))), "{name}: must return Err");
        assert!(largest <= LIMIT, "{name}: allocated {largest} bytes");
    }
}

#[test]
fn output_limit_refuses_files_that_expand_beyond_it() {
    let data = vec![0u8; 12 << 20];
    let compressed = adaptive_compress(&data);
    assert!(compressed.len() < 4096);
    assert!(try_adaptive_decompress_limited(&compressed, 1 << 20).is_err());
    assert_eq!(try_adaptive_decompress_limited(&compressed, data.len()).unwrap(), data);
}
