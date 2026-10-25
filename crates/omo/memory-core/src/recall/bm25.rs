//! Recall BM25 ranking with CJK bigram tokenization (pin `recall/bm25.ts`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};

use unicode_general_category::{GeneralCategory, get_general_category};
use unicode_normalization::UnicodeNormalization;

use super::english_stem::stem_english_token;
use super::provider::RecallDocument;

const K1: f64 = 1.5;
const B: f64 = 0.75;

fn is_hangul(ch: char) -> bool {
    matches!(ch as u32,
        0x1100..=0x11FF | 0x3130..=0x318F | 0xA960..=0xA97F | 0xAC00..=0xD7A3 | 0xD7B0..=0xD7FF)
}

fn is_han(ch: char) -> bool {
    matches!(ch as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F)
}

fn is_kana(ch: char) -> bool {
    matches!(ch as u32, 0x3040..=0x309F | 0x30A0..=0x30FF | 0x31F0..=0x31FF)
}

fn is_prolonged_sound_mark(ch: char) -> bool {
    ch == '\u{30fc}'
}

/// True when the character is part of the CJK class the pinned tokenizer groups into runs.
pub fn is_cjk_char(ch: char) -> bool {
    is_hangul(ch) || is_han(ch) || is_kana(ch) || is_prolonged_sound_mark(ch)
}

pub fn is_han_character(token: &str) -> bool {
    let mut chars = token.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) => is_han(ch),
        _ => false,
    }
}

fn is_word_char(ch: char) -> bool {
    matches!(
        get_general_category(ch),
        GeneralCategory::LowercaseLetter
            | GeneralCategory::UppercaseLetter
            | GeneralCategory::TitlecaseLetter
            | GeneralCategory::ModifierLetter
            | GeneralCategory::OtherLetter
            | GeneralCategory::NonspacingMark
            | GeneralCategory::SpacingMark
            | GeneralCategory::EnclosingMark
            | GeneralCategory::DecimalNumber
            | GeneralCategory::LetterNumber
            | GeneralCategory::OtherNumber
    )
}

struct RecallRun {
    pieces: Vec<String>,
    han_characters: Vec<String>,
}

fn split_run(token: &str) -> RecallRun {
    let characters: Vec<char> = token.chars().collect();
    let mut pieces = vec![token.to_string()];
    if characters.len() < 2 || !characters.iter().all(|ch| is_cjk_char(*ch)) {
        return RecallRun {
            pieces,
            han_characters: Vec::new(),
        };
    }
    if characters.len() > 2 {
        for pair in characters.windows(2) {
            pieces.push(pair.iter().collect());
        }
    }
    RecallRun {
        pieces,
        han_characters: characters
            .into_iter()
            .filter(|ch| is_han(*ch))
            .map(String::from)
            .collect(),
    }
}

fn runs(text: &str) -> Vec<RecallRun> {
    let normalized: String = text.nfkc().collect::<String>().to_lowercase();
    let mut runs = Vec::new();
    let mut current = String::new();
    let mut current_is_cjk = false;
    for ch in normalized.chars() {
        let cjk = is_cjk_char(ch);
        if cjk || is_word_char(ch) {
            if !current.is_empty() && cjk != current_is_cjk {
                runs.push(split_run(&current));
                current.clear();
            }
            current.push(ch);
            current_is_cjk = cjk;
            continue;
        }
        if !current.is_empty() {
            runs.push(split_run(&current));
            current.clear();
        }
    }
    if !current.is_empty() {
        runs.push(split_run(&current));
    }
    runs
}

struct RecallTokens {
    tokens: Vec<String>,
    standalone_han_characters: usize,
}

fn tokenize(text: &str) -> RecallTokens {
    let mut tokens = Vec::new();
    let mut standalone_han_characters = 0usize;
    for run in runs(text) {
        for piece in run.pieces {
            tokens.push(piece);
        }
        standalone_han_characters += run.han_characters.len();
        for character in run.han_characters {
            tokens.push(character);
        }
    }
    RecallTokens {
        tokens,
        standalone_han_characters,
    }
}

/// NFKC-normalized, lowercased tokens with CJK runs expanded into bigrams.
pub fn tokenize_recall_text(text: &str) -> Vec<String> {
    tokenize(text).tokens
}

/// A document's index terms and BM25 length: what the index and the persisted term store hold.
pub fn recall_document_terms(document: &RecallDocument) -> DocumentTermsInput {
    let tokens = tokenize(&format!("{}\n{}", document.description, document.body));
    // The stand-alone Han characters repeat text the run and its bigrams already count.
    DocumentTermsInput {
        terms: tokens
            .tokens
            .iter()
            .map(|token| stem_english_token(token))
            .collect(),
        length: tokens.tokens.len() - tokens.standalone_han_characters,
    }
}

/// Index and query terms: the tokens with English suffixes folded.
pub fn recall_terms(text: &str) -> Vec<String> {
    tokenize_recall_text(text)
        .into_iter()
        .map(|token| stem_english_token(&token))
        .collect()
}

/// One document's terms as the tokenizer makes them, before they are interned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentTermsInput {
    pub terms: Vec<String>,
    pub length: usize,
}

/// One document's terms outside the process dictionary: ids index a term list the holder carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDocumentTerms {
    /// Term ids, ascending.
    pub ids: Vec<u32>,
    pub counts: Vec<u32>,
    pub length: usize,
}

/// A document's terms against the live dictionary of one generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentTerms {
    pub generation: u64,
    pub ids: Vec<u32>,
    pub counts: Vec<u32>,
    pub length: usize,
}

impl DocumentTerms {
    pub fn to_stored(&self) -> StoredDocumentTerms {
        StoredDocumentTerms {
            ids: self.ids.clone(),
            counts: self.counts.clone(),
            length: self.length,
        }
    }
}

/// Documents' terms against a compact term list holding exactly the terms they use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallTermsSnapshot {
    pub terms: Vec<String>,
    pub documents: Vec<StoredDocumentTerms>,
}

/// SHA-1 of the exact text a document's terms are made from.
pub type DocumentKey = [u8; 20];

pub(crate) fn sha1_digest(data: &[u8]) -> DocumentKey {
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA1_FOR_LEGACY_USE_ONLY, data);
    let mut key = [0u8; 20];
    for (slot, byte) in key.iter_mut().zip(digest.as_ref()) {
        *slot = *byte;
    }
    key
}

/// The key of `description\nbody`, the text [`recall_document_terms`] tokenizes.
pub fn document_key(document: &RecallDocument) -> DocumentKey {
    // PERF-DIVERGENCE(recall/bm25.ts): upstream hashes a concatenated `description\nbody` string; this port feeds the same bytes to an incremental SHA-1 context, skipping the full-body copy. The digest is identical.
    let mut context = aws_lc_rs::digest::Context::new(&aws_lc_rs::digest::SHA1_FOR_LEGACY_USE_ONLY);
    context.update(document.description.as_bytes());
    context.update(b"\n");
    context.update(document.body.as_bytes());
    let digest = context.finish();
    let mut key = [0u8; 20];
    for (slot, byte) in key.iter_mut().zip(digest.as_ref()) {
        *slot = *byte;
    }
    key
}

/// Terms no live index holds still keep their ids, so the dictionary is replaced once it holds more
/// than twice the live vocabulary plus this slack; every cached document is then re-read once.
const DICTIONARY_SLACK: usize = 65_536;
const RECENT_BUILDS: usize = 8;

/// The process-wide dictionary. The vocabulary lists the dictionary's terms by id; both reset together
/// when the generation rolls over. Document terms are cached by the key of their text, which plays the
/// role of the pin's per-document-object cache across HEAD moves.
#[derive(Default)]
struct IndexState {
    generation: u64,
    dictionary: HashMap<Arc<str>, u32>,
    vocabulary: Vec<Arc<str>>,
    recent_live: Vec<usize>,
    documents: HashMap<DocumentKey, Arc<DocumentTerms>>,
}

static STATE: LazyLock<Mutex<IndexState>> = LazyLock::new(Mutex::default);

fn state() -> MutexGuard<'static, IndexState> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

impl IndexState {
    fn intern(&mut self, term: &str) -> u32 {
        if let Some(id) = self.dictionary.get(term) {
            return *id;
        }
        let id = u32::try_from(self.vocabulary.len()).unwrap_or(u32::MAX);
        let shared: Arc<str> = Arc::from(term);
        self.dictionary.insert(Arc::clone(&shared), id);
        self.vocabulary.push(shared);
        id
    }

    fn encode(&mut self, input: &DocumentTermsInput) -> DocumentTerms {
        let mut counts: BTreeMap<u32, u32> = BTreeMap::new();
        for term in &input.terms {
            let id = self.intern(term);
            *counts.entry(id).or_insert(0) += 1;
        }
        DocumentTerms {
            generation: self.generation,
            ids: counts.keys().copied().collect(),
            counts: counts.values().copied().collect(),
            length: input.length,
        }
    }

    fn current(&self, key: &DocumentKey) -> Option<Arc<DocumentTerms>> {
        self.documents
            .get(key)
            .filter(|cached| cached.generation == self.generation)
            .cloned()
    }
}

/// Compact BM25 index storage: interned term ids, two sorted arrays per document.
pub(crate) struct RecallBm25Index {
    generation: u64,
    documents: Vec<Arc<DocumentTerms>>,
    document_frequency: Vec<u32>,
    average_length: f64,
}

impl RecallBm25Index {
    /// The id of a term any indexed document holds; `None` when no document holds it.
    fn term_id(&self, term: &str) -> Option<u32> {
        let state = state();
        if state.generation != self.generation {
            return None;
        }
        let id = *state.dictionary.get(term)?;
        (self
            .document_frequency
            .get(id as usize)
            .copied()
            .unwrap_or(0)
            > 0)
        .then_some(id)
    }
}

fn term_count(terms: &DocumentTerms, id: u32) -> u32 {
    terms
        .ids
        .binary_search(&id)
        .ok()
        .and_then(|position| terms.counts.get(position).copied())
        .unwrap_or(0)
}

/// Builds the index; the count is how many documents had to be tokenized.
fn build_index(documents: &[RecallDocument]) -> (RecallBm25Index, usize) {
    let mut state = state();
    loop {
        let mut fresh = 0usize;
        let mut encoded: Vec<Arc<DocumentTerms>> = Vec::with_capacity(documents.len());
        for document in documents {
            let key = document_key(document);
            if let Some(cached) = state.current(&key) {
                encoded.push(cached);
                continue;
            }
            fresh += 1;
            let terms = Arc::new(state.encode(&recall_document_terms(document)));
            state.documents.insert(key, Arc::clone(&terms));
            encoded.push(terms);
        }
        let mut document_frequency = vec![0u32; state.vocabulary.len()];
        let mut live = 0usize;
        let mut total_length = 0usize;
        for terms in &encoded {
            total_length += terms.length;
            for id in &terms.ids {
                if let Some(slot) = document_frequency.get_mut(*id as usize) {
                    if *slot == 0 {
                        live += 1;
                    }
                    *slot += 1;
                }
            }
        }
        state.recent_live.push(live);
        if state.recent_live.len() > RECENT_BUILDS {
            state.recent_live.remove(0);
        }
        let largest = state.recent_live.iter().copied().max().unwrap_or(0);
        if state.dictionary.len() > 2 * largest + DICTIONARY_SLACK {
            state.generation += 1;
            state.dictionary = HashMap::new();
            state.vocabulary = Vec::new();
            state.recent_live.clear();
            state.documents.clear();
            continue;
        }
        let average_length = if documents.is_empty() {
            0.0
        } else {
            total_length as f64 / documents.len() as f64
        };
        return (
            RecallBm25Index {
                generation: state.generation,
                documents: encoded,
                document_frequency,
                average_length,
            },
            fresh,
        );
    }
}

/// One index per cached corpus (the pin keys its `WeakMap` by array); other slices build per call.
/// A cached index from an older dictionary generation is rebuilt.
fn index_for(documents: &[RecallDocument]) -> Arc<RecallBm25Index> {
    let Some(derived) = super::corpus_derived::derived_for(documents) else {
        return Arc::new(build_index(documents).0);
    };
    let mut slot = derived.bm25.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(cached) = slot.as_ref()
        && cached.generation == state().generation
    {
        return Arc::clone(cached);
    }
    let (index, fresh) = build_index(documents);
    let index = Arc::new(index);
    *slot = Some(Arc::clone(&index));
    drop(slot);
    if fresh > 0 {
        let listener = derived
            .encoded_listener
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(listener) = listener {
            listener();
        }
    }
    index
}

/// Calls `listener` after an index of `documents` is built that had to tokenize at least one of them,
/// so a holder of a persisted copy knows that copy is missing documents. Only a slice a
/// `RecallCorpusCache` registered has an index to build, so another slice is ignored.
pub fn on_recall_documents_encoded(
    documents: &[RecallDocument],
    listener: Arc<dyn Fn() + Send + Sync>,
) {
    if let Some(derived) = super::corpus_derived::derived_for(documents) {
        *derived
            .encoded_listener
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(listener);
    }
}

/// What the memory report reads for these documents: their cached term arrays and the process
/// dictionary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecallTermFigures {
    /// Term id and count arrays the index keeps for these documents.
    pub term_array_bytes: usize,
    /// Terms in the process-wide dictionary.
    pub vocabulary_terms: usize,
}

/// Sums the term-array bytes of the documents already indexed in the current generation.
///
/// A document the index never tokenized contributes nothing, so reading the figures never indexes
/// one: the report must not pay for the corpus it measures.
pub fn recall_term_figures<'a>(
    documents: impl IntoIterator<Item = &'a RecallDocument>,
) -> RecallTermFigures {
    let mut term_array_bytes = 0usize;
    for document in documents {
        if let Some(terms) = RECALL_TERM_ACCESS.current(document) {
            term_array_bytes += terms.ids.len() * std::mem::size_of::<u32>()
                + terms.counts.len() * std::mem::size_of::<u32>();
        }
    }
    RecallTermFigures {
        term_array_bytes,
        vocabulary_terms: RECALL_TERM_ACCESS.vocabulary_size(),
    }
}

/// The index internals a persisted store needs to seed and snapshot document terms.
#[derive(Debug, Clone, Copy, Default)]
pub struct RecallTermAccess;

/// The process-wide access to the live dictionary.
pub const RECALL_TERM_ACCESS: RecallTermAccess = RecallTermAccess;

impl RecallTermAccess {
    pub fn current(&self, document: &RecallDocument) -> Option<Arc<DocumentTerms>> {
        let key = document_key(document);
        state().current(&key)
    }

    /// Sets `document`'s terms from a store when it has none in the current generation.
    pub fn adopt(&self, document: &RecallDocument, stored: StoredDocumentTerms) {
        let key = document_key(document);
        let mut state = state();
        if state.current(&key).is_none() {
            let terms = DocumentTerms {
                generation: state.generation,
                ids: stored.ids,
                counts: stored.counts,
                length: stored.length,
            };
            state.documents.insert(key, Arc::new(terms));
        }
    }

    pub fn intern(&self, term: &str) -> u32 {
        state().intern(term)
    }

    pub fn term(&self, id: u32) -> Option<String> {
        state()
            .vocabulary
            .get(id as usize)
            .map(|term| term.to_string())
    }

    pub fn vocabulary_size(&self) -> usize {
        state().vocabulary.len()
    }

    /// The dictionary generation; ids of a different generation mean other terms.
    pub fn generation(&self) -> u64 {
        state().generation
    }
}

/// Terms a caller adds to a search, by their distance from the query's own words.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecallQueryExpansions {
    pub synonyms: Option<Vec<String>>,
    pub keywords: Option<Vec<String>>,
    pub related: Option<Vec<String>>,
    pub note_line: Option<String>,
}

/// Weight of an added term; the query's own terms weigh 1.0.
pub const RECALL_EXPANSION_WEIGHTS_SYNONYMS: f64 = 0.75;
pub const RECALL_EXPANSION_WEIGHTS_KEYWORDS: f64 = 0.75;
pub const RECALL_EXPANSION_WEIGHTS_RELATED: f64 = 0.4;
pub const RECALL_EXPANSION_WEIGHTS_NOTE_LINE: f64 = 0.4;

/// The weight of each added term; a query term is left out and a term in several tiers keeps its highest.
pub fn recall_expansion_weights(
    expansions: Option<&RecallQueryExpansions>,
    query_terms: &BTreeSet<String>,
) -> BTreeMap<String, f64> {
    let mut weights = BTreeMap::new();
    let Some(expansions) = expansions else {
        return weights;
    };
    let tiers: [(f64, &Option<Vec<String>>); 3] = [
        (RECALL_EXPANSION_WEIGHTS_SYNONYMS, &expansions.synonyms),
        (RECALL_EXPANSION_WEIGHTS_KEYWORDS, &expansions.keywords),
        (RECALL_EXPANSION_WEIGHTS_RELATED, &expansions.related),
    ];
    let note_line = expansions
        .note_line
        .as_ref()
        .map(|line| vec![line.clone()])
        .unwrap_or_default();
    for (weight, texts) in tiers
        .into_iter()
        .map(|(weight, texts)| (weight, texts.clone().unwrap_or_default()))
        .chain([(RECALL_EXPANSION_WEIGHTS_NOTE_LINE, note_line)])
    {
        for text in texts {
            for term in recall_terms(&text) {
                if !query_terms.contains(&term)
                    && weight > weights.get(&term).copied().unwrap_or(0.0)
                {
                    weights.insert(term, weight);
                }
            }
        }
    }
    weights
}

#[derive(Debug, Clone, PartialEq)]
pub struct RankedRecallDocument {
    pub document: RecallDocument,
    pub score: f64,
    pub full_match: bool,
}

fn query_units(queries: &[String]) -> Vec<String> {
    let mut units: Vec<String> = Vec::new();
    for query in queries {
        for run in runs(query) {
            for piece in run.pieces {
                let term = stem_english_token(&piece);
                if !units.contains(&term) {
                    units.push(term);
                }
            }
        }
    }
    units
}

/// Documents sharing at least one token with the queries, best first; ties break on path.
pub fn rank_recall_documents_bm25(
    documents: &[RecallDocument],
    queries: &[String],
    expansions: Option<&RecallQueryExpansions>,
) -> Vec<RankedRecallDocument> {
    rank_recall_positions_bm25(documents, queries, expansions)
        .into_iter()
        .map(|entry| RankedRecallDocument {
            document: documents[entry.position].clone(),
            score: entry.score,
            full_match: entry.full_match,
        })
        .collect()
}

/// One ranked document by its position in the ranked slice, so callers clone only what they keep.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RankedRecallPosition {
    pub position: usize,
    pub score: f64,
    pub full_match: bool,
}

/// [`rank_recall_documents_bm25`] without cloning documents: the same order, by slice position.
pub(crate) fn rank_recall_positions_bm25(
    documents: &[RecallDocument],
    queries: &[String],
    expansions: Option<&RecallQueryExpansions>,
) -> Vec<RankedRecallPosition> {
    let mut query_terms: Vec<String> = Vec::new();
    // PERF-DIVERGENCE(recall/bm25.ts): upstream dedupes query terms with a linear includes scan; this port also tracks a HashSet so dedupe is O(1) per term while insertion order stays identical.
    let mut seen_terms: std::collections::HashSet<String> = std::collections::HashSet::new();
    for term in queries.iter().flat_map(|query| recall_terms(query)) {
        if seen_terms.insert(term.clone()) {
            query_terms.push(term);
        }
    }
    if query_terms.is_empty() || documents.is_empty() {
        return Vec::new();
    }

    let index = index_for(documents);
    let average_length = if index.average_length > 0.0 {
        index.average_length
    } else {
        1.0
    };
    let document_count = documents.len() as f64;
    // PERF-DIVERGENCE(recall/bm25.ts): upstream recomputes idf per (document, term); this port computes it once per query term before the document loop using the identical f64 expression, so scores are bit-identical.
    let idf_of = |id: Option<u32>| -> f64 {
        let Some(id) = id else {
            return 0.0;
        };
        let document_frequency = index
            .document_frequency
            .get(id as usize)
            .copied()
            .unwrap_or(0) as f64;
        (1.0 + (document_count - document_frequency + 0.5) / (document_frequency + 0.5)).ln()
    };
    let term_score = |id: Option<u32>, idf: f64, terms: &DocumentTerms| -> f64 {
        let Some(id) = id else {
            return 0.0;
        };
        let frequency = term_count(terms, id);
        if frequency == 0 {
            return 0.0;
        }
        let frequency = f64::from(frequency);
        (idf * frequency * (K1 + 1.0))
            / (frequency + K1 * (1.0 - B + B * terms.length as f64 / average_length))
    };

    let query_set: BTreeSet<String> = query_terms.iter().cloned().collect();
    let added: Vec<(Option<u32>, f64, f64)> = recall_expansion_weights(expansions, &query_set)
        .into_iter()
        .map(|(term, weight)| {
            let id = index.term_id(&term);
            (id, weight, idf_of(id))
        })
        .collect();
    let query_ids: Vec<(Option<u32>, f64)> = query_terms
        .iter()
        .map(|term| {
            let id = index.term_id(term);
            (id, idf_of(id))
        })
        .collect();
    let unit_ids: Vec<Option<u32>> = if added.is_empty() {
        Vec::new()
    } else {
        query_units(queries)
            .iter()
            .map(|unit| index.term_id(unit))
            .collect()
    };

    let mut ranked: Vec<RankedRecallPosition> = Vec::new();
    let mut full_matches: Vec<RankedRecallPosition> = Vec::new();
    for position in 0..documents.len() {
        let Some(terms) = index.documents.get(position) else {
            continue;
        };
        let mut score: f64 = 0.0;
        for (id, idf) in &query_ids {
            score += term_score(*id, *idf, terms);
        }
        if !added.is_empty() {
            if unit_ids
                .iter()
                .all(|id| id.is_some_and(|id| term_count(terms, id) > 0))
            {
                full_matches.push(RankedRecallPosition {
                    position,
                    score,
                    full_match: true,
                });
                continue;
            }
            for (id, weight, idf) in &added {
                score += weight * term_score(*id, *idf, terms);
            }
        }
        if score > 0.0 {
            ranked.push(RankedRecallPosition {
                position,
                score,
                full_match: false,
            });
        }
    }

    let by_score = |left: &RankedRecallPosition, right: &RankedRecallPosition| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                documents[left.position]
                    .path
                    .cmp(&documents[right.position].path)
            })
    };
    ranked.sort_by(by_score);
    if full_matches.is_empty() {
        return ranked;
    }
    let lift = ranked.first().map(|entry| entry.score).unwrap_or(0.0);
    full_matches.sort_by(by_score);
    for entry in &mut full_matches {
        entry.score += lift;
    }
    full_matches.extend(ranked);
    full_matches
}

#[cfg(test)]
#[path = "bm25_tests.rs"]
mod tests;
