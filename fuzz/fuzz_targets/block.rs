#![no_main]
//! Every block decoder on its own: [1B codec][2B block length LE][payload].

use libfuzzer_sys::fuzz_target;
use nexcomp::adaptive::{decompress_block_adaptive, CodecId};

fuzz_target!(|data: &[u8]| {
    let [id, lo, hi, payload @ ..] = data else { return };
    let Some(codec) = CodecId::from_u8(id % 8) else { return };
    let _ = decompress_block_adaptive(codec, payload, usize::from(u16::from_le_bytes([*lo, *hi])));
});
