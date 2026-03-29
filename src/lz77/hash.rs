// src/lz77/hash.rs — Hash chain for O(n) match search in LZ77

use super::types::*;

pub struct HashChain {
    /// head[hash] = most recent position with this hash, u32::MAX if empty
    pub head: Vec<u32>,
    /// prev[pos % MAX_WINDOW] = previous position with same hash (linked list)
    pub prev: Vec<u32>,
}

impl HashChain {
    pub fn new() -> Self {
        Self {
            head: vec![u32::MAX; HASH_SIZE],
            prev: vec![u32::MAX; MAX_WINDOW],
        }
    }

    /// Hash of 3 consecutive bytes using multiplicative hashing.
    /// Constant 0x1E35A7BD gives good distribution for byte triples.
    #[inline]
    pub fn hash3(a: u8, b: u8, c: u8) -> usize {
        let val = (a as u32) << 16 | (b as u32) << 8 | c as u32;
        // Multiplicative hash: multiply by prime, take top HASH_BITS bits
        ((val.wrapping_mul(0x1E35A7BD)) >> (32 - HASH_BITS)) as usize & HASH_MASK
    }

    /// Insert position `pos` with hash `h` into the chain.
    #[inline]
    pub fn insert(&mut self, h: usize, pos: u32) {
        let slot = pos as usize % MAX_WINDOW;
        self.prev[slot] = self.head[h];
        self.head[h] = pos;
    }

    /// Find the longest match starting at `current` in `data`.
    /// Returns Some((offset, length)) if match >= MIN_MATCH, else None.
    /// Follows hash chain up to MAX_CHAIN entries for speed.
    pub fn find_best_match(
        &self,
        data: &[u8],
        current: usize,
    ) -> Option<(u32, u16)> {
        if current + 3 > data.len() {
            return None;
        }

        let h = Self::hash3(data[current], data[current + 1], data[current + 2]);
        let mut candidate = self.head[h];
        let mut best_len: usize = MIN_MATCH - 1;
        let mut best_offset: u32 = 0;
        let max_len = MAX_MATCH.min(data.len() - current);
        let mut chain_count = 0;

        while candidate != u32::MAX && chain_count < MAX_CHAIN {
            let cand = candidate as usize;

            // Must be before current and within window
            if cand >= current {
                candidate = self.prev[cand % MAX_WINDOW];
                chain_count += 1;
                continue;
            }
            let distance = current - cand;
            if distance > MAX_WINDOW {
                break; // beyond window, all further entries are too old
            }

            // Quick check: compare byte at best_len position first (pruning)
            // Guard against out-of-bounds for both candidate and current positions
            let cand_max_len = max_len.min(data.len() - cand);
            if best_len < cand_max_len
                && data[cand + best_len] == data[current + best_len]
            {
                // Count matching bytes from the start
                let mut len = 0;
                while len < cand_max_len && data[cand + len] == data[current + len] {
                    len += 1;
                }

                if len > best_len {
                    best_len = len;
                    best_offset = distance as u32;
                    if best_len == max_len {
                        break; // can't do better
                    }
                }
            }

            candidate = self.prev[cand % MAX_WINDOW];
            chain_count += 1;
        }

        if best_len >= MIN_MATCH {
            Some((best_offset, best_len as u16))
        } else {
            None
        }
    }

    /// Like `find_best_match` but also returns the chain step at which the
    /// best match was found (0-based). Used by the encoder to judge whether
    /// short matches at large offsets are worth keeping.
    pub fn find_best_match_with_steps(
        &self,
        data: &[u8],
        current: usize,
    ) -> Option<(u32, u16, usize)> {
        if current + 3 > data.len() {
            return None;
        }

        let h = Self::hash3(data[current], data[current + 1], data[current + 2]);
        let mut candidate = self.head[h];
        let mut best_len: usize = MIN_MATCH - 1;
        let mut best_offset: u32 = 0;
        let mut best_steps: usize = 0;
        let max_len = MAX_MATCH.min(data.len() - current);
        let mut chain_count = 0;

        while candidate != u32::MAX && chain_count < MAX_CHAIN {
            let cand = candidate as usize;

            if cand >= current {
                candidate = self.prev[cand % MAX_WINDOW];
                chain_count += 1;
                continue;
            }
            let distance = current - cand;
            if distance > MAX_WINDOW {
                break;
            }

            let cand_max_len = max_len.min(data.len() - cand);
            if best_len < cand_max_len
                && data[cand + best_len] == data[current + best_len]
            {
                let mut len = 0;
                while len < cand_max_len && data[cand + len] == data[current + len] {
                    len += 1;
                }

                if len > best_len {
                    best_len = len;
                    best_offset = distance as u32;
                    best_steps = chain_count;
                    if best_len == max_len {
                        break;
                    }
                }
            }

            candidate = self.prev[cand % MAX_WINDOW];
            chain_count += 1;
        }

        if best_len >= MIN_MATCH {
            Some((best_offset, best_len as u16, best_steps))
        } else {
            None
        }
    }
}
