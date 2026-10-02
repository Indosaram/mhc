use serde::{Deserialize, Serialize};

pub const FUZZY_SEARCH_MAX_DEPTH: usize = 12;
pub const FUZZY_SEARCH_MAX_VISITED_ENTRIES: usize = 50_000;
pub const FUZZY_SEARCH_RESULT_LIMIT: usize = 50;
#[derive(Clone, Debug)]
pub struct FuzzyFileEntry {
    pub root: String,
    pub path: String,
    pub match_type: String,
    pub file_name: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FuzzyFileSearchResult {
    pub root: String,
    pub path: String,
    pub match_type: String,
    pub file_name: String,
    pub score: usize,
    pub indices: Vec<usize>,
}
fn score_subsequence(candidate: &str, query: &str) -> Option<(usize, Vec<usize>)> {
    let mut normalized = String::new(); let mut source_indices = Vec::new();
    for (index, character) in candidate.chars().enumerate() {
        let lower = character.to_lowercase().collect::<String>();
        source_indices.extend(std::iter::repeat_n(index, lower.len())); normalized.push_str(&lower);
    }
    let query = query.to_lowercase(); let contiguous = normalized.find(&query); let mut cursor = contiguous.unwrap_or(0); let mut indices = Vec::new();
    for character in query.chars() {
        let found = if contiguous.is_some() { cursor } else { cursor + normalized[cursor..].find(character)? };
        let source = *source_indices.get(found)?;
        if indices.last() != Some(&source) { indices.push(source); }
        cursor = found + character.len_utf8();
    }
    let characters = candidate.chars().collect::<Vec<_>>(); let mut score = indices.len() * 10;
    for (position, current) in indices.iter().copied().enumerate() {
        if position > 0 && current == indices[position - 1] + 1 { score += 20; }
        if current == 0 || matches!(characters.get(current - 1), Some('/' | '-' | '_' | '.' | ' ')) { score += 5; }
    }
    Some((score, indices))
}
pub fn rank_fuzzy_file_entries(query: &str, entries: &[FuzzyFileEntry]) -> Vec<FuzzyFileSearchResult> {
    if query.is_empty() { return Vec::new(); }
    let mut results = entries.iter().filter_map(|entry| {
        let filename = score_subsequence(&entry.file_name, query);
        let filename_match = filename.is_some();
        let (score, mut indices) = filename.or_else(|| score_subsequence(&entry.path, query))?;
        if filename_match { let offset = entry.path.chars().count().saturating_sub(entry.file_name.chars().count()); for index in &mut indices { *index += offset; } }
        Some(FuzzyFileSearchResult { root:entry.root.clone(), path:entry.path.clone(), match_type:entry.match_type.clone(), file_name:entry.file_name.clone(), score:score + if filename_match { 1_000_000 } else { 0 }, indices })
    }).collect::<Vec<_>>();
    results.sort_by(|left, right| right.score.cmp(&left.score).then_with(|| left.path.encode_utf16().cmp(right.path.encode_utf16())));
    results.truncate(FUZZY_SEARCH_RESULT_LIMIT); results
}
