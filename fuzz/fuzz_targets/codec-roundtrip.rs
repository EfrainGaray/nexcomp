#![no_main]
//! Every codec, not only the one the selector would pick, must decode its own
//! output exactly: [1B codec, 8 = BCJ filter][input].

use libfuzzer_sys::fuzz_target;
use nexcomp::adaptive::{decompress_block_adaptive, encode_with, CodecId};
use nexcomp::codecs::bcj_filter::{bcj_decode, bcj_encode};

fuzz_target!(|data: &[u8]| {
    let [id, input @ ..] = data else { return };
    // The container never hands a codec an empty block.
    if input.is_empty() {
        return;
    }
    let Some(codec) = CodecId::from_u8(id % 9) else {
        assert_eq!(bcj_decode(&bcj_encode(input)), input);
        return;
    };
    let Some(payload) = encode_with(codec, input) else { return };
    let decoded = decompress_block_adaptive(codec, &payload, input.len()).expect("a codec decodes its own output");
    assert_eq!(decoded, input, "{}", codec.name());
});
