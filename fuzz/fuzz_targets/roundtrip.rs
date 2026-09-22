#![no_main]
//! Any input survives compress then decompress unchanged.

use libfuzzer_sys::fuzz_target;
use nexcomp::adaptive::{adaptive_compress, try_adaptive_decompress};

fuzz_target!(|data: &[u8]| {
    let file = adaptive_compress(data);
    assert_eq!(try_adaptive_decompress(&file).expect("own output decodes"), data);
});
