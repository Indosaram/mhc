//! Port of senpi packages/ai/src/utils/hash.ts.

use super::text::to_radix_36;

/// Fast deterministic hash to shorten long strings (cyrb53 over UTF-16 code units).
pub fn short_hash(input: &str) -> String {
    let mut h1: u32 = 0xdead_beef;
    let mut h2: u32 = 0x41c6_ce57;
    for ch in input.encode_utf16() {
        let ch = u32::from(ch);
        h1 = (h1 ^ ch).wrapping_mul(2_654_435_761);
        h2 = (h2 ^ ch).wrapping_mul(1_597_334_677);
    }
    h1 = (h1 ^ (h1 >> 16)).wrapping_mul(2_246_822_507) ^ (h2 ^ (h2 >> 13)).wrapping_mul(3_266_489_909);
    h2 = (h2 ^ (h2 >> 16)).wrapping_mul(2_246_822_507) ^ (h1 ^ (h1 >> 13)).wrapping_mul(3_266_489_909);
    format!("{}{}", to_radix_36(u64::from(h2)), to_radix_36(u64::from(h1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_senpi_values() {
        assert_eq!(short_hash(""), "k4n83c7h0j2b");
        assert_eq!(short_hash("hello"), "1h6qa0qrowduu");
    }
}
