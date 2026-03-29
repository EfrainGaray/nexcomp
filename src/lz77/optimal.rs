// LZ77 Optimal Parsing — shortest-path DP over the match graph
//
// Reference: Storer & Szymanski, "Data Compression via Textual Substitution",
//            JACM 29(4):928-951, 1982.
//            Zopfli (Google, 2013) squeeze.c — iterative price model.
//
// Algorithm:
//   1. Build match graph: at each position i, find all matches (len, dist)
//   2. Model as DAG: node i → i+1 (literal, cost = lit_price(byte))
//                    node i → i+len (match, cost = len_price + dist_price + extra_bits)
//   3. Forward DP: cost[i] = min cost to encode positions 0..i
//   4. Backward trace: reconstruct optimal token sequence
//
// Price model: -log2(freq[sym] / total) bits per symbol, computed from
//              Huffman code lengths of the current iteration.
//
// Single-pass optimal parsing is ~5-10x slower than greedy, not 80x like Zopfli.

use super::hash::HashChain;
use super::huffman;
use super::types::*;

/// Cost in fixed-point bits (scaled by 256 for precision without floats).
type Cost = u64;
const COST_INF: Cost = u64::MAX / 2;
const COST_SCALE: u64 = 256; // 1 bit = 256 cost units

/// Precomputed price model for estimating token costs.
struct PriceModel {
    /// Cost of each litlen symbol (0-285) in scaled bits
    litlen_price: [Cost; huffman::LITLEN_SYMBOLS],
    /// Cost of each distance code (0-43) in scaled bits
    dist_price: [Cost; huffman::DIST_SYMBOLS],
}

impl PriceModel {
    /// Build initial price model from assumed uniform-ish distribution.
    /// Literals cost ~8 bits, short matches ~10 bits, long matches ~15 bits.
    fn initial() -> Self {
        let mut litlen_price = [8 * COST_SCALE; huffman::LITLEN_SYMBOLS];
        // Length codes: base cost increases with length code
        for i in 257..huffman::LITLEN_SYMBOLS {
            let idx = i - 257;
            let (_, extra) = huffman::LENGTH_TABLE[idx];
            litlen_price[i] = (7 + extra as u64) * COST_SCALE;
        }
        let mut dist_price = [0; huffman::DIST_SYMBOLS];
        for i in 0..huffman::NORMAL_DIST_SYMBOLS {
            let (_, extra) = huffman::DIST_TABLE[i];
            dist_price[i] = (5 + extra as u64) * COST_SCALE;
        }
        // Rep-match codes (44-47): cheap, ~2 bits for the Huffman code + 0 extra bits
        for i in huffman::NORMAL_DIST_SYMBOLS..huffman::DIST_SYMBOLS {
            dist_price[i] = 2 * COST_SCALE;
        }
        PriceModel { litlen_price, dist_price }
    }

    /// Build price model from actual Huffman code lengths (iteration > 0).
    fn from_code_lengths(litlen_codes: &[huffman::HuffCode], dist_codes: &[huffman::HuffCode]) -> Self {
        let mut litlen_price = [COST_INF; huffman::LITLEN_SYMBOLS];
        for (i, code) in litlen_codes.iter().enumerate().take(huffman::LITLEN_SYMBOLS) {
            if code.len > 0 {
                litlen_price[i] = code.len as Cost * COST_SCALE;
            }
        }
        let mut dist_price = [COST_INF; huffman::DIST_SYMBOLS];
        for (i, code) in dist_codes.iter().enumerate().take(huffman::DIST_SYMBOLS) {
            if code.len > 0 {
                dist_price[i] = code.len as Cost * COST_SCALE;
            }
        }
        PriceModel { litlen_price, dist_price }
    }

    /// Cost of encoding a literal byte.
    #[inline]
    fn literal_cost(&self, byte: u8) -> Cost {
        self.litlen_price[byte as usize]
    }

    /// Cost of encoding a match (length, offset) including extra bits.
    #[inline]
    fn match_cost(&self, length: u16, offset: u32) -> Cost {
        let (len_code, len_extra, _) = huffman::encode_length(length);
        let (dist_code, dist_extra, _) = huffman::encode_offset(offset);
        let len_cost = self.litlen_price[len_code as usize];
        let dist_cost = self.dist_price[dist_code as usize];
        if len_cost >= COST_INF || dist_cost >= COST_INF {
            return COST_INF;
        }
        len_cost + len_extra as Cost * COST_SCALE
            + dist_cost + dist_extra as Cost * COST_SCALE
    }
}

/// Find all matches at position `pos` using the hash chain.
/// Returns matches sorted by length (shortest first).
fn find_all_matches(chain: &HashChain, data: &[u8], pos: usize) -> Vec<(u32, u16)> {
    if pos + 3 > data.len() {
        return Vec::new();
    }

    let h = HashChain::hash3(data[pos], data[pos + 1], data[pos + 2]);
    let mut candidate = chain.head_at(h);
    let max_len = MAX_MATCH.min(data.len() - pos);
    let mut matches = Vec::new();
    let mut best_len = MIN_MATCH - 1;
    let mut chain_count = 0;

    while candidate != u32::MAX && chain_count < MAX_CHAIN {
        let cand = candidate as usize;
        if cand >= pos {
            candidate = chain.prev_at(cand);
            chain_count += 1;
            continue;
        }
        let distance = pos - cand;
        if distance > MAX_WINDOW {
            break;
        }

        // Count match length
        let cand_max = max_len.min(data.len() - cand);
        let mut len = 0;
        while len < cand_max && data[cand + len] == data[pos + len] {
            len += 1;
        }

        if len >= MIN_MATCH && len > best_len {
            // Record all lengths from MIN_MATCH to len for this offset
            // (shorter matches at closer distances may be cheaper)
            matches.push((distance as u32, len as u16));
            best_len = len;
            if best_len == max_len {
                break;
            }
        }

        candidate = chain.prev_at(cand);
        chain_count += 1;
    }

    matches
}

/// Optimal LZ77 parser using forward DP + backward trace.
///
/// Iterates `n_iterations` times:
///   Iteration 0: use initial price model (approximate)
///   Iteration 1+: use Huffman code lengths from previous iteration's tokens
pub fn optimal_parse(data: &[u8], n_iterations: usize) -> Vec<Token> {
    if data.is_empty() {
        return Vec::new();
    }

    let n = data.len();
    let n_iters = n_iterations.max(1);
    let mut tokens = Vec::new();

    for iter in 0..n_iters {
        let price = if iter == 0 {
            PriceModel::initial()
        } else {
            // Build price model from previous iteration's tokens
            let _huff_data = huffman::huffman_encode(&tokens);
            // Re-extract code lengths by re-running frequency counting + tree building
            let mut litlen_freq = vec![0u32; huffman::LITLEN_SYMBOLS];
            let mut dist_freq = vec![0u32; huffman::DIST_SYMBOLS];
            for t in &tokens {
                match t {
                    Token::Literal(b) => litlen_freq[*b as usize] += 1,
                    Token::Match { offset, length } => {
                        let (lc, _, _) = huffman::encode_length(*length);
                        litlen_freq[lc as usize] += 1;
                        let (dc, _, _) = huffman::encode_offset(*offset);
                        dist_freq[dc as usize] += 1;
                    }
                }
            }
            let litlen_codes = huffman::build_huffman_codes(&litlen_freq, huffman::LITLEN_SYMBOLS);
            let dist_codes = huffman::build_huffman_codes(&dist_freq, huffman::DIST_SYMBOLS);
            PriceModel::from_code_lengths(&litlen_codes, &dist_codes)
        };

        // Build hash chain
        let mut chain = HashChain::new();

        // Forward DP: cost[i] = minimum cost to encode data[0..i]
        // prev[i] = (token_type, back_pointer) for backtracking
        let mut cost = vec![COST_INF; n + 1];
        // Store: for each position, the token that leads to it optimally
        // (0 = literal from i-1, or match_length > 0 from i-match_length)
        let mut prev_len = vec![0u32; n + 1]; // 0 = literal, >0 = match length
        let mut prev_dist = vec![0u32; n + 1]; // offset for matches

        cost[0] = 0;

        for i in 0..n {
            if cost[i] >= COST_INF {
                continue;
            }

            // Insert position into hash chain for future lookups
            if i + 3 <= n {
                let h = HashChain::hash3(data[i], data[i + 1], data[i + 2]);
                chain.insert(h, i as u32);
            }

            // Option 1: Literal
            let lit_cost = cost[i] + price.literal_cost(data[i]);
            if lit_cost < cost[i + 1] {
                cost[i + 1] = lit_cost;
                prev_len[i + 1] = 0; // literal
                prev_dist[i + 1] = 0;
            }

            // Option 2: All possible matches
            let matches = find_all_matches(&chain, data, i);
            for &(offset, max_match_len) in &matches {
                // Try all lengths from MIN_MATCH to max_match_len
                // But for efficiency, only try MIN_MATCH, some intermediates, and max
                let lengths_to_try: Vec<u16> = if max_match_len <= 10 {
                    (MIN_MATCH as u16..=max_match_len).collect()
                } else {
                    let mut v = vec![MIN_MATCH as u16];
                    // Try a few intermediate lengths
                    for l in [4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 258] {
                        if l > MIN_MATCH as u16 && l < max_match_len {
                            v.push(l);
                        }
                    }
                    v.push(max_match_len);
                    v
                };

                for &mlen in &lengths_to_try {
                    let end = i + mlen as usize;
                    if end > n {
                        break;
                    }
                    let mcost = cost[i] + price.match_cost(mlen, offset);
                    if mcost < cost[end] {
                        cost[end] = mcost;
                        prev_len[end] = mlen as u32;
                        prev_dist[end] = offset;
                    }
                }
            }
        }

        // Backward trace: reconstruct optimal token sequence
        tokens.clear();
        let mut pos = n;
        while pos > 0 {
            let len = prev_len[pos];
            if len == 0 {
                // Literal
                tokens.push(Token::Literal(data[pos - 1]));
                pos -= 1;
            } else {
                // Match
                let offset = prev_dist[pos];
                tokens.push(Token::Match {
                    offset,
                    length: len as u16,
                });
                pos -= len as usize;
            }
        }
        tokens.reverse();
    }

    tokens
}

// Expose head/prev accessors on HashChain for optimal parser
impl HashChain {
    pub fn head_at(&self, h: usize) -> u32 {
        self.head[h]
    }
    pub fn prev_at(&self, pos: usize) -> u32 {
        self.prev[pos % MAX_WINDOW]
    }
}
