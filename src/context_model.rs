// NEXCOMP — Order-1 Context Model for rANS Entropy Coding
//
// Uses a 256×256 frequency table (order-1 Markov model) where
// table[prev_byte][curr_byte] gives the frequency of curr_byte following prev_byte.
// Each row is normalized to sum=4096 (scale_bits=12) for use with the rANS coder.

use crate::entropy::{
    build_decode_table, build_table, RansDecodeTable, RansDecoder, RansEncoder,
    RansError, RansTable,
};

/// Order-1 context model: 256 frequency rows, one per context byte.
pub struct ContextModel {
    /// Normalized frequency tables, one per context byte (256 tables, each 256 entries).
    pub tables: Vec<RansTable>,
    /// Decode lookup tables for each context.
    pub decode_tables: Vec<RansDecodeTable>,
    /// Raw frequency counts [context][symbol] before normalization, kept for serialization.
    raw_freqs: Vec<[u32; 256]>,
}

impl ContextModel {
    /// Train the context model from the given data stream.
    ///
    /// For each consecutive pair (prev, curr), increment counts[prev][curr].
    /// Then normalize each row to sum=4096, ensuring no nonzero entry becomes 0.
    pub fn train(stream: &[u8]) -> Self {
        let mut counts = vec![[0u64; 256]; 256];

        // Build bigram frequency table
        if !stream.is_empty() {
            // First byte uses context 0
            counts[0][stream[0] as usize] += 1;
            for i in 1..stream.len() {
                let prev = stream[i - 1] as usize;
                let curr = stream[i] as usize;
                counts[prev][curr] += 1;
            }
        }

        let mut tables = Vec::with_capacity(256);
        let mut decode_tables = Vec::with_capacity(256);
        let mut raw_freqs = Vec::with_capacity(256);

        for ctx in 0..256 {
            let freq_arr = crate::ajedrez::build_freq_table_from_counts(&counts[ctx]);
            let freqs: Vec<u32> = freq_arr.to_vec();
            raw_freqs.push(freq_arr);
            let table = build_table(&freqs).expect("normalized freqs must sum to 4096");
            let dtable = build_decode_table(&table);
            tables.push(table);
            decode_tables.push(dtable);
        }

        ContextModel {
            tables,
            decode_tables,
            raw_freqs,
        }
    }

    /// Encode a data stream using order-1 rANS with per-context tables.
    ///
    /// The rANS encoder processes symbols in REVERSE order. When processing
    /// symbol at position i in reverse, the context is the byte at position i-1.
    /// First byte (position 0) uses context=0.
    pub fn encode(&self, stream: &[u8]) -> Result<Vec<u8>, RansError> {
        if stream.is_empty() {
            return Err(RansError::EmptyInput);
        }

        let mut encoder = RansEncoder::new();

        // Process in reverse order (rANS is stack-based).
        // For position i, context = stream[i-1] if i > 0, else 0.
        for i in (0..stream.len()).rev() {
            let ctx = if i > 0 { stream[i - 1] as usize } else { 0 };
            let symbol = stream[i] as usize;
            encoder.encode_symbol(&self.tables[ctx], symbol)?;
        }

        Ok(encoder.finish())
    }

    /// Decode n_symbols from compressed data using order-1 context model.
    ///
    /// This is the exact inverse of encode. Decoding proceeds forward:
    /// first symbol uses context=0, subsequent symbols use the previously
    /// decoded byte as context.
    pub fn decode(&self, data: &[u8], n_symbols: usize) -> Result<Vec<u8>, RansError> {
        if n_symbols == 0 {
            return Ok(Vec::new());
        }

        let mut decoder = RansDecoder::new(data)?;
        let mut output = Vec::with_capacity(n_symbols);

        let mut ctx: usize = 0; // first byte uses context 0
        for _ in 0..n_symbols {
            let symbol = decoder.decode_symbol(&self.decode_tables[ctx])?;
            output.push(symbol);
            ctx = symbol as usize;
        }

        Ok(output)
    }

    /// Serialize the model as raw bytes: 256 rows x 256 entries x 2 bytes (u16 LE) = 131,072 bytes.
    pub fn serialize_model(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(256 * 256 * 2);
        for ctx in 0..256 {
            for sym in 0..256 {
                let val = self.raw_freqs[ctx][sym] as u16;
                out.extend_from_slice(&val.to_le_bytes());
            }
        }
        out
    }

    /// Deserialize a model from raw bytes (131,072 bytes = 256 x 256 x u16 LE).
    /// Rebuilds the RansTables and decode tables from the stored frequencies.
    pub fn deserialize_model(data: &[u8]) -> Result<Self, RansError> {
        if data.len() < 256 * 256 * 2 {
            return Err(RansError::DecodeUnderflow);
        }

        let mut tables = Vec::with_capacity(256);
        let mut decode_tables = Vec::with_capacity(256);
        let mut raw_freqs = Vec::with_capacity(256);

        let mut pos = 0;
        for _ctx in 0..256 {
            let mut freqs = [0u32; 256];
            for sym in 0..256 {
                let lo = data[pos] as u16;
                let hi = data[pos + 1] as u16;
                freqs[sym] = (lo | (hi << 8)) as u32;
                pos += 2;
            }

            raw_freqs.push(freqs);
            let table = build_table(freqs.as_ref())?;
            let dtable = build_decode_table(&table);
            tables.push(table);
            decode_tables.push(dtable);
        }

        Ok(ContextModel {
            tables,
            decode_tables,
            raw_freqs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip_random() {
        // 64KB of pseudo-random data
        let mut data = vec![0u8; 65536];
        let mut state: u64 = 0xDEAD_BEEF_CAFE_BABE;
        for b in data.iter_mut() {
            // xorshift64
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *b = (state & 0xFF) as u8;
        }

        let model = ContextModel::train(&data);
        let encoded = model.encode(&data).expect("encode should succeed");
        let decoded = model.decode(&encoded, data.len()).expect("decode should succeed");
        assert_eq!(data, decoded, "random data round-trip must be bit-perfect");
    }

    #[test]
    fn test_roundtrip_text() {
        let text = b"The quick brown fox jumps over the lazy dog. \
                     Compression is the dual of prediction. \
                     rANS achieves near-optimal entropy coding with table-based lookups. \
                     Order-1 context models capture byte-to-byte correlations effectively. \
                     In English text, the letter 'q' is almost always followed by 'u'. \
                     This property allows context models to assign higher probability \
                     to likely successors, reducing the average code length.";

        let model = ContextModel::train(text);
        let encoded = model.encode(text).expect("encode should succeed");
        let decoded = model.decode(&encoded, text.len()).expect("decode should succeed");
        assert_eq!(
            text.as_slice(),
            decoded.as_slice(),
            "text round-trip must be bit-perfect"
        );
    }

    #[test]
    fn test_roundtrip_zeros() {
        let data = vec![0u8; 4096];
        let model = ContextModel::train(&data);
        let encoded = model.encode(&data).expect("encode should succeed");
        let decoded = model.decode(&encoded, data.len()).expect("decode should succeed");
        assert_eq!(data, decoded, "all-zero data round-trip must be bit-perfect");
    }

    #[test]
    fn test_model_reduces_size() {
        // Structured English-like text where order-1 model should outperform order-0.
        // Repeat patterns to amplify bigram correlations.
        let base = b"the the the that that this this then them there their \
                     in in into ing ion tion ation ness ment able ible \
                     he she they them their there here where when what which \
                     qu qu qu question quick quiet quite quality quantity \
                     and and and another answer any anything anywhere always";
        let mut text = Vec::new();
        for _ in 0..50 {
            text.extend_from_slice(base);
        }

        // Order-1 encode
        let model_o1 = ContextModel::train(&text);
        let encoded_o1 = model_o1.encode(&text).expect("order-1 encode");

        // Order-0 encode: use global frequency table
        let mut counts = [0u64; 256];
        for &b in &text {
            counts[b as usize] += 1;
        }
        let freqs = crate::entropy::normalize_freqs(&counts, 256);
        let table = build_table(&freqs).expect("valid table");
        let encoded_o0 = crate::entropy::rans_encode(&text, &table).expect("order-0 encode");

        assert!(
            encoded_o1.len() < encoded_o0.len(),
            "order-1 ({} bytes) should compress better than order-0 ({} bytes) on structured text",
            encoded_o1.len(),
            encoded_o0.len()
        );
    }

    #[test]
    fn test_serialize_deserialize_model() {
        let text = b"Hello world! This is a test of serialization.";
        let model = ContextModel::train(text);
        let serialized = model.serialize_model();
        assert_eq!(serialized.len(), 256 * 256 * 2, "serialized size must be 131072");

        let model2 = ContextModel::deserialize_model(&serialized).expect("deserialize ok");

        // Verify the deserialized model produces the same encoding
        let encoded1 = model.encode(text).expect("encode with original");
        let encoded2 = model2.encode(text).expect("encode with deserialized");
        assert_eq!(encoded1, encoded2, "serialized model must produce identical encoding");

        let decoded = model2.decode(&encoded2, text.len()).expect("decode ok");
        assert_eq!(text.as_slice(), decoded.as_slice());
    }
}
