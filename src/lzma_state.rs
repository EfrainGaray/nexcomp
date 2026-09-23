//! LZMA state machine with 12 states and associated probability arrays.

use crate::range_coder::{Prob, PROB_INIT};

pub const NUM_STATES: usize = 12;
pub const PB: usize = 2;
pub const NUM_POS_STATES: usize = 1 << PB; // 4

// Transition tables from the LZMA specification.
const LITERAL_NEXT: [usize; NUM_STATES] = [0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 4, 5];
const MATCH_NEXT: [usize; NUM_STATES] = [7, 7, 7, 7, 7, 7, 7, 10, 10, 10, 10, 10];
const REP_NEXT: [usize; NUM_STATES] = [8, 8, 8, 8, 8, 8, 8, 11, 11, 11, 11, 11];
const SHORTREP_NEXT: [usize; NUM_STATES] = [9, 9, 9, 9, 9, 9, 9, 11, 11, 11, 11, 11];

/// Tracks the current LZMA state (0 -- 11).
pub struct LzmaState {
    pub state: usize,
}

impl Default for LzmaState {
    fn default() -> Self {
        Self::new()
    }
}

impl LzmaState {
    pub fn new() -> Self {
        Self { state: 0 }
    }

    pub fn update_literal(&mut self) {
        self.state = LITERAL_NEXT[self.state];
    }

    pub fn update_match(&mut self) {
        self.state = MATCH_NEXT[self.state];
    }

    pub fn update_rep(&mut self) {
        self.state = REP_NEXT[self.state];
    }

    pub fn update_shortrep(&mut self) {
        self.state = SHORTREP_NEXT[self.state];
    }

    /// States 0 -- 6 are "literal" states (last symbol was a literal).
    pub fn is_literal_state(&self) -> bool {
        self.state < 7
    }

    #[inline]
    pub fn pos_state(pos: usize) -> usize {
        pos & (NUM_POS_STATES - 1)
    }
}

/// All probability arrays that are indexed by the LZMA state and position state.
pub struct StateProbs {
    pub is_match: [[Prob; NUM_POS_STATES]; NUM_STATES],
    pub is_rep: [Prob; NUM_STATES],
    pub is_rep0: [Prob; NUM_STATES],
    pub is_rep0_long: [[Prob; NUM_POS_STATES]; NUM_STATES],
    pub is_rep1: [Prob; NUM_STATES],
    pub is_rep2: [Prob; NUM_STATES],
}

impl StateProbs {
    pub fn new() -> Self {
        Self {
            is_match: [[PROB_INIT; NUM_POS_STATES]; NUM_STATES],
            is_rep: [PROB_INIT; NUM_STATES],
            is_rep0: [PROB_INIT; NUM_STATES],
            is_rep0_long: [[PROB_INIT; NUM_POS_STATES]; NUM_STATES],
            is_rep1: [PROB_INIT; NUM_STATES],
            is_rep2: [PROB_INIT; NUM_STATES],
        }
    }
}

impl Default for StateProbs {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_state() {
        let s = LzmaState::new();
        assert_eq!(s.state, 0);
        assert!(s.is_literal_state());
    }

    #[test]
    fn literal_transitions() {
        for start in 0..NUM_STATES {
            let mut s = LzmaState { state: start };
            s.update_literal();
            assert_eq!(s.state, LITERAL_NEXT[start]);
        }
    }

    #[test]
    fn match_transitions() {
        for start in 0..NUM_STATES {
            let mut s = LzmaState { state: start };
            s.update_match();
            assert_eq!(s.state, MATCH_NEXT[start]);
            // After a match we must be in a non-literal state (>= 7).
            assert!(!s.is_literal_state());
        }
    }

    #[test]
    fn rep_transitions() {
        for start in 0..NUM_STATES {
            let mut s = LzmaState { state: start };
            s.update_rep();
            assert_eq!(s.state, REP_NEXT[start]);
            assert!(!s.is_literal_state());
        }
    }

    #[test]
    fn shortrep_transitions() {
        for start in 0..NUM_STATES {
            let mut s = LzmaState { state: start };
            s.update_shortrep();
            assert_eq!(s.state, SHORTREP_NEXT[start]);
            assert!(!s.is_literal_state());
        }
    }

    #[test]
    fn typical_sequence() {
        // Simulate: lit lit match lit rep shortrep lit
        let mut s = LzmaState::new();
        s.update_literal(); // 0 -> 0
        assert_eq!(s.state, 0);
        s.update_literal(); // 0 -> 0
        assert_eq!(s.state, 0);
        s.update_match(); // 0 -> 7
        assert_eq!(s.state, 7);
        assert!(!s.is_literal_state());
        s.update_literal(); // 7 -> 4
        assert_eq!(s.state, 4);
        assert!(s.is_literal_state());
        s.update_rep(); // 4 -> 8
        assert_eq!(s.state, 8);
        s.update_shortrep(); // 8 -> 11
        assert_eq!(s.state, 11);
        s.update_literal(); // 11 -> 5
        assert_eq!(s.state, 5);
        assert!(s.is_literal_state());
    }

    #[test]
    fn state_probs_initialised() {
        let sp = StateProbs::new();
        for s in 0..NUM_STATES {
            assert_eq!(sp.is_rep[s], PROB_INIT);
            assert_eq!(sp.is_rep0[s], PROB_INIT);
            assert_eq!(sp.is_rep1[s], PROB_INIT);
            assert_eq!(sp.is_rep2[s], PROB_INIT);
            for ps in 0..NUM_POS_STATES {
                assert_eq!(sp.is_match[s][ps], PROB_INIT);
                assert_eq!(sp.is_rep0_long[s][ps], PROB_INIT);
            }
        }
    }
}
