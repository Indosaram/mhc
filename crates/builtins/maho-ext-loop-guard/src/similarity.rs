use std::collections::BTreeMap;
pub type BigramCounts = BTreeMap<[u16; 2], u32>;
pub fn bigram_counts(text: &str) -> BigramCounts {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut counts = BigramCounts::new();
    for pair in units.windows(2) { *counts.entry([pair[0], pair[1]]).or_default() += 1; }
    counts
}
pub fn dice_similarity(a: &BigramCounts, b: &BigramCounts) -> f64 {
    let total_a: u32 = a.values().sum();
    let total_b: u32 = b.values().sum();
    if total_a == 0 && total_b == 0 { return 1.0; }
    if total_a == 0 || total_b == 0 { return 0.0; }
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let intersection: u32 = small.iter().map(|(gram, count)| (*count).min(large.get(gram).copied().unwrap_or(0))).sum();
    2.0 * f64::from(intersection) / (f64::from(total_a) + f64::from(total_b))
}
pub fn mean_adjacent_similarity(args: &[String]) -> f64 {
    if args.len() < 2 { return 1.0; }
    let grams: Vec<_> = args.iter().map(|s| bigram_counts(s)).collect();
    let (total, count) = grams.windows(2).fold((0.0, 0_u32), |(total, count), pair| (total + dice_similarity(&pair[0], &pair[1]), count + 1));
    total / f64::from(count)
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn identical_strings_score_one() { let grams = bigram_counts("abcdef"); let result = dice_similarity(&grams, &grams); assert!((result - 1.0).abs() < f64::EPSILON); }
    #[test] fn disjoint_strings_score_zero() { let a = bigram_counts("aaaa"); let b = bigram_counts("zzzz"); let result = dice_similarity(&a, &b); assert!(result.abs() < f64::EPSILON); }
    #[test] fn unicode_counts_utf16_bigrams() { let text = "😀😀"; let result = bigram_counts(text); assert_eq!(result.values().sum::<u32>(), 3); }
    #[test] fn pagination_separates_from_distinct_queries() { let pagination = ["{\"path\":\"src/app.ts\",\"offset\":1,\"limit\":200}", "{\"path\":\"src/app.ts\",\"offset\":201,\"limit\":200}", "{\"path\":\"src/app.ts\",\"offset\":401,\"limit\":200}"].map(String::from); let queries = ["{\"query\":\"tool call loop detection\"}", "{\"query\":\"typescript vitest fake timers\"}", "{\"query\":\"kubernetes pod eviction policy\"}"].map(String::from); let result = (mean_adjacent_similarity(&pagination), mean_adjacent_similarity(&queries)); assert!(result.0 >= 0.85); assert!(result.1 < 0.85); }
}
