#![no_main]
//! Any byte string given to the container decoder: an error or the data,
//! never a panic (libfuzzer aborts even on panics the container catches).

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Wide enough for multi-block files and full 4 MiB blocks, which a 64 KiB
    // cap rejected before any codec ran; still bounded, so a decompression
    // bomb is a finding here and not an out-of-memory abort of the fuzzer.
    let _ = nexcomp::adaptive::try_adaptive_decompress_limited(data, 32 << 20);
});
