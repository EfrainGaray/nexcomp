#![no_main]
//! Every block decoder on its own: [1B codec][4B block length LE][payload].
//! The length is what the container would declare, so it has to reach the
//! 4 MiB a block can hold, not the 64 KiB two bytes allow.

use libfuzzer_sys::fuzz_target;
use nexcomp::adaptive::{decompress_block_adaptive, CodecId};

fuzz_target!(|data: &[u8]| {
    let [id, a, b, c, d, payload @ ..] = data else { return };
    let Some(codec) = CodecId::from_u8(id % 8) else { return };
    let block_len = u32::from_le_bytes([*a, *b, *c, *d]) as usize % ((4 << 20) + 1);
    let _ = decompress_block_adaptive(codec, payload, block_len);
});
