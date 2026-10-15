use serde::{Deserialize, Serialize};
use std::{path::{Path, PathBuf}, sync::{Arc, atomic::{AtomicBool, Ordering}}};
use super::gitignore::{IgnoreScope, add_gitignore_scope, is_ignored};

/// One pending directory-walk entry: path, depth, inherited ignore scopes, and the entry it came from.
type WalkFrame = (PathBuf, usize, Arc<Vec<IgnoreScope>>, Option<tokio::fs::DirEntry>);

pub const FUZZY_SEARCH_MAX_DEPTH: usize = 12;
pub const FUZZY_SEARCH_MAX_VISITED_ENTRIES: usize = 50_000;
pub const FUZZY_SEARCH_RESULT_LIMIT: usize = 50;
#[derive(Clone)]
pub struct FuzzyTraversalOptions {
    pub cancelled: Arc<AtomicBool>,
    pub max_depth: usize,
    pub max_visited_entries: usize,
}
impl Default for FuzzyTraversalOptions {
    fn default() -> Self {
        Self { cancelled: Arc::new(AtomicBool::new(false)), max_depth: FUZZY_SEARCH_MAX_DEPTH, max_visited_entries: FUZZY_SEARCH_MAX_VISITED_ENTRIES }
    }
}

pub async fn collect_fuzzy_file_entries(roots: &[String], options: &FuzzyTraversalOptions) -> Vec<FuzzyFileEntry> {
    let mut entries = Vec::new();
    let mut visited = 0;
    for root in roots {
        if options.cancelled.load(Ordering::Acquire) || visited >= options.max_visited_entries { break; }
        let Ok(absolute_root) = std::path::absolute(Path::new(root)) else { continue; };
        let Ok(metadata) = tokio::fs::symlink_metadata(&absolute_root).await else { continue; };
        if !metadata.is_dir() || metadata.is_symlink() { continue; }
        let mut stack: Vec<WalkFrame> = vec![(absolute_root.clone(), 0, Arc::default(), None)];
        while let Some((path, depth, scopes, entry)) = stack.pop() {
            if options.cancelled.load(Ordering::Acquire) || visited >= options.max_visited_entries { break; }
            if let Some(entry) = entry {
                visited += 1;
                let name = entry.file_name().to_string_lossy().into_owned();
                if name == ".git" || name == "node_modules" { continue; }
                let Ok(kind) = entry.file_type().await else { continue; };
                if kind.is_symlink() || (!kind.is_dir() && !kind.is_file()) { continue; }
                if is_ignored(&scopes, &path, kind.is_dir()) { continue; }
                let Ok(relative) = path.strip_prefix(&absolute_root) else { continue; };
                entries.push(FuzzyFileEntry { root:root.clone(), path:relative.to_string_lossy().into_owned(), match_type:if kind.is_dir() { "directory" } else { "file" }.into(), file_name:name });
                if !kind.is_dir() || depth >= options.max_depth {
                    if visited % 256 == 0 { tokio::task::yield_now().await; }
                    continue;
                }
            }
            // Children share their parent's scopes; a copy is made only when this directory adds one.
            let added = add_gitignore_scope(Vec::new(), &path).await;
            let scopes = if added.is_empty() { scopes } else { let mut extended = Vec::clone(&scopes); extended.extend(added); Arc::new(extended) };
            let Ok(mut directory) = tokio::fs::read_dir(&path).await else { continue; };
            let mut children = Vec::new();
            while let Ok(Some(child)) = directory.next_entry().await { children.push(child); }
            children.sort_by(|left, right| left.file_name().to_string_lossy().encode_utf16().cmp(right.file_name().to_string_lossy().encode_utf16()));
            for child in children.into_iter().rev() {
                stack.push((child.path(), depth + 1, scopes.clone(), Some(child)));
            }
            if visited > 0 && visited % 256 == 0 { tokio::task::yield_now().await; }
        }
    }
    entries
}
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
/// `query` is already lowercased by the caller.
fn score_subsequence(candidate: &str, query: &str) -> Option<(usize, Vec<usize>)> {
    // ASCII lowercasing keeps byte offsets, which are then also char indices.
    let ascii = candidate.is_ascii();
    let mut source_indices = Vec::new();
    let normalized = if ascii { candidate.to_ascii_lowercase() } else {
        let mut normalized = String::new();
        for (index, character) in candidate.chars().enumerate() {
            let lower = character.to_lowercase().collect::<String>();
            source_indices.extend(std::iter::repeat_n(index, lower.len())); normalized.push_str(&lower);
        }
        normalized
    };
    let contiguous = normalized.find(query); let mut cursor = contiguous.unwrap_or(0); let mut indices = Vec::new();
    for character in query.chars() {
        let found = if contiguous.is_some() { cursor } else { cursor + normalized[cursor..].find(character)? };
        let source = if ascii { found } else { *source_indices.get(found)? };
        if indices.last() != Some(&source) { indices.push(source); }
        cursor = found + character.len_utf8();
    }
    let characters = if ascii { Vec::new() } else { candidate.chars().collect::<Vec<_>>() };
    let boundary = |index: usize| if ascii { matches!(candidate.as_bytes().get(index), Some(b'/' | b'-' | b'_' | b'.' | b' ')) } else { matches!(characters.get(index), Some('/' | '-' | '_' | '.' | ' ')) };
    let mut score = indices.len() * 10;
    for (position, current) in indices.iter().copied().enumerate() {
        if position > 0 && current == indices[position - 1] + 1 { score += 20; }
        if current == 0 || boundary(current - 1) { score += 5; }
    }
    Some((score, indices))
}
pub fn rank_fuzzy_file_entries(query: &str, entries: &[FuzzyFileEntry]) -> Vec<FuzzyFileSearchResult> {
    if query.is_empty() { return Vec::new(); }
    let query = query.to_lowercase();
    // Rank (score, entry index, indices) first; only the kept entries are cloned into results.
    let mut ranked = entries.iter().enumerate().filter_map(|(entry_index, entry)| {
        let filename = score_subsequence(&entry.file_name, &query);
        let filename_match = filename.is_some();
        let (score, mut indices) = filename.or_else(|| score_subsequence(&entry.path, &query))?;
        if filename_match { let offset = entry.path.chars().count().saturating_sub(entry.file_name.chars().count()); for index in &mut indices { *index += offset; } }
        Some((score + if filename_match { 1_000_000 } else { 0 }, entry_index, indices))
    }).collect::<Vec<_>>();
    // Stable sort: ties on (score, path) keep walk order, as before.
    ranked.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| entries[left.1].path.encode_utf16().cmp(entries[right.1].path.encode_utf16())));
    ranked.truncate(FUZZY_SEARCH_RESULT_LIMIT);
    ranked.into_iter().map(|(score, entry_index, indices)| {
        let entry = &entries[entry_index];
        FuzzyFileSearchResult { root:entry.root.clone(), path:entry.path.clone(), match_type:entry.match_type.clone(), file_name:entry.file_name.clone(), score, indices }
    }).collect()
}
