//! Inputs cargo-fuzz found. Each one must come back as an error, never a panic.

use nexcomp::adaptive::{parse_blocks, CodecId};
use nexcomp::codecs::lzma_style;

/// The top distance slot decodes `base + extra + 1` right at `u32::MAX`, which
/// overflowed the 1-based conversion; the distance must simply be rejected.
#[test]
fn lzma_distance_at_the_top_slot_is_rejected() {
    let file = include_bytes!("fixtures/fuzz/decompress-distance-overflow.bin");
    let (_, blocks) = parse_blocks(file).expect("the crash input parses as a container");
    let block = blocks.iter().find(|b| b.codec == CodecId::LzmaStyle).expect("an LZMA block");
    assert!(lzma_style::decode_block(block.data).is_err());
}
