//! Deterministic seeded PRNG for the chaos bench. No wall clock or ambient randomness anywhere:
//! every draw derives from the captured seed so a pinned seed replays an identical interleaving.
//!
//! Port of `__adversarial__/prng.ts`, which is test-only harness code, so the module is compiled
//! only for tests.
#![cfg(test)]

const FNV_OFFSET_BASIS: u32 = 0x811c_9dc5;
const FNV_PRIME: u32 = 0x0100_0193;
const U32: f64 = 4_294_967_296.0;

/// FNV-1a over the UTF-16 code units of `text` (matches JS `charCodeAt`).
pub fn hash_seed(text: &str) -> u32 {
    let mut hash = FNV_OFFSET_BASIS;
    for unit in text.encode_utf16() {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Independent per-iteration sub-stream so pinning one iteration reproduces it in isolation.
pub fn derive_seed(base: u32, iteration: u32) -> u32 {
    let mut mixed = base ^ iteration.wrapping_add(1).wrapping_mul(0x9e37_79b1);
    mixed = (mixed ^ (mixed >> 16)).wrapping_mul(0x85eb_ca6b);
    mixed = (mixed ^ (mixed >> 13)).wrapping_mul(0xc2b2_ae35);
    mixed ^ (mixed >> 16)
}

#[derive(Debug, Clone, PartialEq)]
pub struct WeightedChoice<T> {
    pub value: T,
    pub weight: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("RandomSource.pick on an empty array")]
pub struct EmptyPickError;

/// Mulberry32 generator.
#[derive(Debug, Clone)]
pub struct RandomSource {
    state: u32,
}

impl RandomSource {
    pub fn new(seed: u32) -> Self {
        Self { state: seed }
    }

    pub fn float(&mut self) -> f64 {
        self.state = self.state.wrapping_add(0x6d2b_79f5);
        let state = self.state;
        let mut t = (state ^ (state >> 15)).wrapping_mul(1 | state);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t)) ^ t;
        f64::from(t ^ (t >> 14)) / U32
    }

    pub fn int(&mut self, min_inclusive: i64, max_inclusive: i64) -> i64 {
        let span = (max_inclusive - min_inclusive + 1) as f64;
        min_inclusive + (self.float() * span).floor() as i64
    }

    pub fn bool(&mut self, probability_true: f64) -> bool {
        self.float() < probability_true
    }

    /// `bool()` with the TS default probability of 0.5.
    pub fn coin(&mut self) -> bool {
        self.bool(0.5)
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> Result<&'a T, EmptyPickError> {
        if items.is_empty() {
            return Err(EmptyPickError);
        }
        let last = (items.len() - 1) as i64;
        let index = self.int(0, last);
        usize::try_from(index)
            .ok()
            .and_then(|index| items.get(index))
            .ok_or(EmptyPickError)
    }

    pub fn weighted<'a, T>(&mut self, choices: &'a [WeightedChoice<T>]) -> Option<&'a T> {
        let total: f64 = choices.iter().map(|choice| choice.weight).sum();
        let mut threshold = self.float() * total;
        for choice in choices {
            threshold -= choice.weight;
            if threshold < 0.0 {
                return Some(&choice.value);
            }
        }
        choices.last().map(|choice| &choice.value)
    }
}

mod tests {
    use pretty_assertions::assert_eq;

    use super::{EmptyPickError, RandomSource, WeightedChoice, derive_seed, hash_seed};

    #[test]
    fn hash_seed_of_empty_text_is_offset_basis() {
        assert_eq!(hash_seed(""), 0x811c_9dc5);
        assert_eq!(hash_seed("seed"), hash_seed("seed"));
    }

    #[test]
    fn derive_seed_is_deterministic_per_iteration() {
        assert_eq!(derive_seed(7, 3), derive_seed(7, 3));
        assert!(derive_seed(7, 3) != derive_seed(7, 4));
    }

    #[test]
    fn same_seed_replays_identical_draws() {
        let mut a = RandomSource::new(42);
        let mut b = RandomSource::new(42);
        for _ in 0..16 {
            let x = a.float();
            assert!((0.0..1.0).contains(&x));
            assert_eq!(x.to_bits(), b.float().to_bits());
        }
        let n = a.int(3, 5);
        assert!((3..=5).contains(&n));
        let _ = a.coin();
        assert!(!a.bool(0.0));
    }

    #[test]
    fn pick_rejects_empty_and_returns_member() {
        let mut source = RandomSource::new(1);
        let empty: [u8; 0] = [];
        assert_eq!(source.pick(&empty), Err(EmptyPickError));
        assert_eq!(
            EmptyPickError.to_string(),
            "RandomSource.pick on an empty array"
        );
        assert_eq!(source.pick(&[9]), Ok(&9));
    }

    #[test]
    fn weighted_honours_zero_weights() {
        let mut source = RandomSource::new(5);
        let choices = [
            WeightedChoice { value: "a", weight: 0.0 },
            WeightedChoice { value: "b", weight: 1.0 },
        ];
        assert_eq!(source.weighted(&choices), Some(&"b"));
        let none: [WeightedChoice<&str>; 0] = [];
        assert_eq!(source.weighted(&none), None);
    }
}
