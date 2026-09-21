// src/lz77/types.rs — Token types and constants for LZ77 encoder/decoder

/// Token de salida del LZ77 encoder.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    /// Byte literal — no encontró match
    Literal(u8),
    /// Match encontrado en la ventana
    /// offset: distancia hacia atrás en bytes (1..=MAX_WINDOW)
    /// length: longitud del match (MIN_MATCH..=MAX_MATCH)
    Match { offset: u32, length: u16 },
}

/// Parámetros del LZ77.
pub const MAX_WINDOW: usize = 4 * 1024 * 1024; // 4MB sliding window
pub const MAX_MATCH: usize = 258;
pub const MIN_MATCH: usize = 3;
pub const HASH_BITS: usize = 18;
pub const HASH_SIZE: usize = 1 << HASH_BITS;
pub const HASH_MASK: usize = HASH_SIZE - 1;
pub const MAX_CHAIN: usize = 256; // max hash chain depth

impl Token {
    /// Estimated bit cost for size estimation.
    /// Literal:  1 flag + 8 value = 9 bits
    /// Match:    1 flag + 22 offset + 8 length = 31 bits
    pub fn bit_cost(&self) -> usize {
        match self {
            Token::Literal(_) => 9,
            Token::Match { .. } => 31,
        }
    }

    pub fn is_match(&self) -> bool {
        matches!(self, Token::Match { .. })
    }
}
