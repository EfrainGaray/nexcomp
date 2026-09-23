// NEXCOMP library — public modules for benchmarks and tests

// The codecs walk parallel arrays by position — symbol, slot, context, block —
// and the index is the meaning, so those range loops stay as they are.
#![allow(clippy::needless_range_loop)]
pub mod ajedrez;
pub mod classifier;
pub mod lz77;
pub mod crypto;
pub mod entropy;
pub mod grammar;
pub mod repair_fast;
pub mod neural;
pub mod transform;
pub mod context_model;
pub mod codecs;
pub mod selector;
pub mod range_coder;
pub mod lzma_state;
pub mod literal_coder;
pub mod classifier_v2;
pub mod adaptive;
