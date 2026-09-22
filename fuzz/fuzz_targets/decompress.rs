#![no_main]
//! Any byte string given to the container decoder: an error or the data,
//! never a panic (libfuzzer aborts even on panics the container catches).

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = nexcomp::adaptive::try_adaptive_decompress_limited(data, 1 << 16);
});
