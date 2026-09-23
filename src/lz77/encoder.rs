// src/lz77/encoder.rs — LZ77 encoder with lazy matching and rep-offset optimization

use super::hash::HashChain;
use super::huffman::MruCache;
use super::types::*;

pub struct Lz77Encoder {
    chain: HashChain,
}

/// Statistics from encoding.
#[derive(Debug)]
pub struct EncoderStats {
    pub literals: usize,
    pub matches: usize,
    pub match_bytes: usize,
    pub match_ratio: f64,
    /// Matches that used a recently-used (rep) offset instead of a hash-chain match.
    pub rep_matches: usize,
}

impl Default for Lz77Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Lz77Encoder {
    pub fn new() -> Self {
        Self {
            chain: HashChain::new(),
        }
    }

    /// Try to find a match at `pos` using one of the cached rep-offsets.
    /// Returns Some((offset, length)) for the best rep-match, or None.
    /// This is a simple byte-by-byte comparison — no hash chain involved.
    #[inline]
    fn try_rep_match(data: &[u8], pos: usize, mru: &MruCache) -> Option<(u32, u16)> {
        let remaining = data.len() - pos;
        if remaining < MIN_MATCH {
            return None;
        }
        let max_len = remaining.min(MAX_MATCH);
        let mut best_offset: u32 = 0;
        let mut best_len: usize = MIN_MATCH - 1;

        for &cached_offset in &mru.recent {
            let co = cached_offset as usize;
            if co == 0 || co > pos {
                continue; // invalid: would reference before start of data
            }
            let ref_pos = pos - co;
            // Count matching bytes
            let mut len = 0;
            while len < max_len && data[ref_pos + len] == data[pos + len] {
                len += 1;
            }
            if len > best_len {
                best_len = len;
                best_offset = cached_offset;
                if best_len == max_len {
                    break;
                }
            }
        }

        if best_len >= MIN_MATCH {
            Some((best_offset, best_len as u16))
        } else {
            None
        }
    }

    /// Update the MRU cache with the given offset: promote if already present,
    /// or insert as new entry.
    #[inline]
    fn update_mru(mru: &mut MruCache, offset: u32) {
        if let Some(idx) = mru.find(offset) {
            mru.promote(idx);
        } else {
            mru.insert(offset);
        }
    }

    /// Merge a rep-match candidate with a hash-chain candidate.
    /// Returns (offset, length, is_rep). Prefers the rep-match if it is within
    /// 1 byte of the hash chain's best length, since rep-offsets encode cheaply.
    #[inline]
    fn merge_rep_and_hash(
        rep: Option<(u32, u16)>,
        hash: Option<(u32, u16)>,
    ) -> Option<(u32, u16, bool)> {
        match (rep, hash) {
            (Some((ro, rl)), Some((ho, hl))) => {
                if rl >= hl.saturating_sub(1) {
                    Some((ro, rl, true))
                } else {
                    Some((ho, hl, false))
                }
            }
            (Some((ro, rl)), None) => Some((ro, rl, true)),
            (None, Some((ho, hl))) => Some((ho, hl, false)),
            (None, None) => None,
        }
    }

    /// Filter unprofitable short hash-chain matches.
    #[inline]
    fn filter_hash_match(m: Option<(u32, u16, usize)>) -> Option<(u32, u16)> {
        m.and_then(|(offset, length, steps)| {
            if length == 3 {
                if offset > 4096 {
                    return None;
                }
                if offset > 1024 && steps >= 4 {
                    return None;
                }
            }
            Some((offset, length))
        })
    }

    /// Compress data into a stream of LZ77 tokens.
    ///
    /// Uses gzip-9-style lazy matching with two-step lookahead:
    /// 1. Find best match at pos.
    /// 2. Insert pos into hash chain, then check pos+1.
    /// 3. If pos+1 is longer, insert pos+1 and also check pos+2.
    /// 4. Pick the best among {pos, pos+1, pos+2}, emitting literals to reach it.
    ///
    /// Before searching the hash chain, checks 4 recently-used offsets (MRU cache)
    /// for matches. Rep-matches are preferred when within 1 byte of the hash
    /// chain's best, since they encode much more cheaply (~2-3 bits vs ~10-20 bits).
    ///
    /// Also filters unprofitable short matches:
    /// - length=3 at offset > 4096: always emit as literals (costs more bits).
    /// - length=3 at offset > 1024: only keep if found in < 4 hash chain steps.
    ///
    /// Returns (tokens, rep_match_count).
    pub fn encode(&mut self, data: &[u8]) -> (Vec<Token>, usize) {
        let mut tokens = Vec::with_capacity(data.len() / 2);
        let mut pos: usize = 0;
        let mut mru = MruCache::new(); // initial state: [1, 2, 3, 4]
        let mut rep_match_count: usize = 0;

        while pos < data.len() {
            // Need at least 3 bytes for hashing
            if pos + 3 > data.len() {
                for &b in &data[pos..] {
                    tokens.push(Token::Literal(b));
                }
                break;
            }

            // --- Step 0: Check rep-offsets first (cheap byte comparison) ---
            let rep_match = Self::try_rep_match(data, pos, &mru);

            // --- Step 1: Find match via hash chain ---
            let hash_match = Self::filter_hash_match(
                self.chain.find_best_match_with_steps(data, pos),
            );

            // --- Merge: prefer rep-match if within 1 byte of hash best ---
            let current_match = Self::merge_rep_and_hash(rep_match, hash_match);

            match current_match {
                Some((offset, length, is_rep)) => {
                    // Insert current position into hash chain
                    let h = HashChain::hash3(data[pos], data[pos + 1], data[pos + 2]);
                    self.chain.insert(h, pos as u32);

                    // --- Two-step lazy matching (gzip-9 strategy) ---
                    // Check pos+1 (with both rep and hash)
                    let next1 = if pos + 1 + 3 <= data.len() {
                        let rep1 = Self::try_rep_match(data, pos + 1, &mru);
                        let hash1 = Self::filter_hash_match(
                            self.chain.find_best_match_with_steps(data, pos + 1),
                        );
                        Self::merge_rep_and_hash(rep1, hash1)
                    } else {
                        None
                    };

                    let use_pos1 = next1.is_some_and(|(_, nl, _)| nl > length);

                    if use_pos1 {
                        // pos+1 is longer — also check pos+2
                        let h1 = HashChain::hash3(data[pos + 1], data[pos + 2], data[pos + 3]);
                        self.chain.insert(h1, (pos + 1) as u32);

                        let next1_len = next1.unwrap().1;

                        let next2 = if pos + 2 + 3 <= data.len() {
                            let rep2 = Self::try_rep_match(data, pos + 2, &mru);
                            let hash2 = Self::filter_hash_match(
                                self.chain.find_best_match_with_steps(data, pos + 2),
                            );
                            Self::merge_rep_and_hash(rep2, hash2)
                        } else {
                            None
                        };

                        let use_pos2 = next2.is_some_and(|(_, nl, _)| nl > next1_len);

                        if use_pos2 {
                            tokens.push(Token::Literal(data[pos]));
                            tokens.push(Token::Literal(data[pos + 1]));
                            pos += 2;
                        } else {
                            tokens.push(Token::Literal(data[pos]));
                            pos += 1;
                        }
                    } else {
                        // Use match at current position
                        tokens.push(Token::Match { offset, length });
                        Self::update_mru(&mut mru, offset);
                        if is_rep {
                            rep_match_count += 1;
                        }
                        // Insert all positions covered by the match into hash chain
                        let match_end = (pos + length as usize).min(data.len());
                        for p in (pos + 1)..match_end {
                            if p + 3 <= data.len() {
                                let mh = HashChain::hash3(data[p], data[p + 1], data[p + 2]);
                                self.chain.insert(mh, p as u32);
                            }
                        }
                        pos = match_end;
                    }
                }
                None => {
                    // No match found: emit literal
                    let h = HashChain::hash3(data[pos], data[pos + 1], data[pos + 2]);
                    self.chain.insert(h, pos as u32);
                    tokens.push(Token::Literal(data[pos]));
                    pos += 1;
                }
            }
        }

        (tokens, rep_match_count)
    }

    /// Compute statistics from a token stream (with rep-match count from encode).
    pub fn stats_with_reps(tokens: &[Token], rep_matches: usize) -> EncoderStats {
        let literals = tokens.iter().filter(|t| matches!(t, Token::Literal(_))).count();
        let matches = tokens.iter().filter(|t| t.is_match()).count();
        let match_bytes: usize = tokens
            .iter()
            .map(|t| {
                if let Token::Match { length, .. } = t {
                    *length as usize
                } else {
                    0
                }
            })
            .sum();
        let total_input = literals + match_bytes;

        EncoderStats {
            literals,
            matches,
            match_bytes,
            match_ratio: match_bytes as f64 / total_input.max(1) as f64,
            rep_matches,
        }
    }

    /// Compute statistics from a token stream (rep_matches defaults to 0).
    pub fn stats(tokens: &[Token]) -> EncoderStats {
        Self::stats_with_reps(tokens, 0)
    }
}
