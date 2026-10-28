//! Query and content text handling for lexical recall.
//!
//! Port of mnemopi `core/synonyms.ts` (synonym groups), `util/regex.ts` (`hasCjk`,
//! `matchesWordForm`) and the tokenizer / relevance helpers of `core/beam/recall.ts`
//! (`tokenize`, `recallSynonyms`, `expandedTokens`, `expandedTokenGroups`,
//! `lexicalGroupRelevance`, `minimumRelevance`, `queryAsksCurrent`, `currentContentAdjustment`,
//! `ftsQuery`, `clipRecallContent`).
use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;

/// Words dropped from queries and content before matching (`recall.ts` `STOP_WORDS`).
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "how", "i", "in", "is", "it",
    "of", "on", "or", "that", "the", "this", "to", "was", "what", "when", "where", "who", "with",
];

/// Canonical word → its synonyms (`synonyms.ts` `SYNONYM_GROUPS`, same order).
const SYNONYM_GROUPS: &[(&str, &[&str])] = &[
    ("database", &["db", "datastore", "data_store"]),
    (
        "password",
        &["pass", "pwd", "passwd", "credential", "secret", "token"],
    ),
    ("config", &["configuration", "settings", "cfg", "setup"]),
    (
        "error",
        &[
            "bug",
            "issue",
            "fault",
            "failure",
            "crash",
            "exception",
            "traceback",
        ],
    ),
    (
        "fix",
        &["repair", "resolve", "solve", "patch", "correct", "address"],
    ),
    (
        "deploy",
        &["deployment", "release", "ship", "push", "rollout"],
    ),
    (
        "server",
        &["host", "machine", "vm", "instance", "node", "vps"],
    ),
    ("api", &["endpoint", "interface", "service"]),
    ("key", &["token", "credential", "secret", "api_key"]),
    ("user", &["account", "profile", "identity", "person"]),
    (
        "model",
        &["llm", "ai", "provider", "gpt", "claude", "gemini"],
    ),
    (
        "speed",
        &["fast", "quick", "performance", "latency", "throughput"],
    ),
    ("memory", &["recall", "remember", "storage", "retention"]),
    ("search", &["find", "lookup", "query", "retrieve", "locate"]),
    ("file", &["document", "doc", "text", "note"]),
    ("code", &["script", "program", "source", "implementation"]),
    ("test", &["verify", "check", "validate", "probe", "examine"]),
    ("backup", &["snapshot", "copy", "save", "archive"]),
    ("install", &["setup", "configure", "bootstrap", "init"]),
    ("update", &["upgrade", "refresh", "renew", "sync"]),
    (
        "delete",
        &["remove", "destroy", "purge", "clean", "wipe", "erase"],
    ),
    ("list", &["show", "display", "enumerate", "catalog"]),
    ("time", &["date", "when", "timestamp", "schedule"]),
    ("url", &["link", "address", "uri", "path"]),
    ("health", &["status", "check", "pulse", "alive", "up"]),
    ("service", &["daemon", "process", "systemd", "worker"]),
    ("port", &["socket", "bind", "listen"]),
    (
        "network",
        &["internet", "connection", "connectivity", "dns"],
    ),
    ("ssh", &["terminal", "shell", "remote", "connect"]),
    (
        "git",
        &["commit", "push", "pull", "repo", "repository", "branch"],
    ),
    ("log", &["output", "stdout", "stderr", "trace", "debug"]),
    ("cron", &["schedule", "job", "task", "timer", "periodic"]),
    ("email", &["mail", "message", "inbox", "smtp"]),
    ("image", &["picture", "photo", "screenshot", "graphic"]),
    ("browser", &["web", "page", "site", "navigate", "chrome"]),
    ("monitor", &["watch", "observe", "track", "survey"]),
    ("alert", &["notify", "notification", "warning", "ping"]),
    ("migrate", &["transfer", "move", "relocate", "port"]),
    ("compare", &["diff", "versus", "vs", "contrast"]),
    ("save", &["store", "persist", "preserve", "keep"]),
];

/// Word → canonical group. Built in group order, so a word listed in two groups maps to the later
/// one, as the upstream reverse map does.
fn word_to_canonical() -> &'static HashMap<&'static str, usize> {
    static MAP: OnceLock<HashMap<&'static str, usize>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map = HashMap::new();
        for (index, (canonical, synonyms)) in SYNONYM_GROUPS.iter().enumerate() {
            map.insert(*canonical, index);
            for synonym in *synonyms {
                map.insert(*synonym, index);
            }
        }
        map
    })
}

/// The word itself when it has no group, else the canonical word followed by its group.
pub fn synonyms(word: &str) -> Vec<String> {
    let lowered = word.to_lowercase();
    match word_to_canonical().get(lowered.as_str()) {
        None => vec![lowered],
        Some(&index) => {
            let (canonical, group) = SYNONYM_GROUPS[index];
            std::iter::once(canonical)
                .chain(group.iter().copied())
                .map(str::to_owned)
                .collect()
        }
    }
}

fn is_token_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// Lowercased runs of letters, digits and `_`, without stop words.
pub fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|ch: char| !is_token_char(ch))
        .filter(|token| !token.is_empty() && !STOP_WORDS.contains(token))
        .map(str::to_owned)
        .collect()
}

fn recall_synonyms(token: &str, use_synonyms: bool) -> Vec<String> {
    if !use_synonyms {
        return vec![token.to_owned()];
    }
    let mut variants = synonyms(token);
    let extra: &[&str] = match token {
        "branding" => &["positioning", "wording", "headline"],
        "preference" | "prefer" | "preferred" => &["wants", "want", "prefers"],
        _ => &[],
    };
    variants.extend(extra.iter().map(|word| (*word).to_owned()));
    variants
}

fn push_unique(out: &mut Vec<String>, seen: &mut BTreeSet<String>, value: String) {
    if seen.insert(value.clone()) {
        out.push(value);
    }
}

/// Every query token with its synonyms, deduplicated, in first-seen order.
pub fn expanded_tokens(query: &str, use_synonyms: bool) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for token in tokenize(query) {
        for variant in recall_synonyms(&token, use_synonyms) {
            for part in tokenize(&variant) {
                push_unique(&mut out, &mut seen, part);
            }
        }
    }
    out
}

/// One group per query token: the token and its synonyms.
pub fn expanded_token_groups(query: &str, use_synonyms: bool) -> Vec<Vec<String>> {
    let mut groups = Vec::new();
    for token in tokenize(query) {
        let mut seen = BTreeSet::new();
        let mut group = Vec::new();
        for variant in recall_synonyms(&token, use_synonyms) {
            for part in tokenize(&variant) {
                push_unique(&mut group, &mut seen, part);
            }
        }
        if !group.is_empty() {
            groups.push(group);
        }
    }
    groups
}

fn is_cjk_char(ch: char) -> bool {
    matches!(ch, '\u{3040}'..='\u{30ff}' | '\u{4e00}'..='\u{9fff}' | '\u{ac00}'..='\u{d7af}')
}

/// True when `text` contains Hiragana/Katakana, CJK ideographs or Hangul syllables.
pub fn has_cjk(text: &str) -> bool {
    text.chars().any(is_cjk_char)
}

const WORD_FORM_ENDINGS: &[&str] = &["s", "es", "d", "ed", "ing", "er", "ers", "ment", "ments"];
const SILENT_E_ENDINGS: &[&str] = &["ing", "ion", "ions"];
const FINAL_Y_ENDINGS: &[&str] = &["ies", "ied"];

/// Weak match between two forms of one word (`facts`/`fact`, `caching`/`cache`,
/// `stories`/`story`); the shorter token needs at least four characters. Not a stemmer.
pub fn matches_word_form(left: &str, right: &str) -> bool {
    let (shorter, longer) = if left.chars().count() <= right.chars().count() {
        (left, right)
    } else {
        (right, left)
    };
    let short_len = shorter.chars().count();
    if short_len < 4 || longer.chars().count() == short_len {
        return false;
    }
    if let Some(rest) = longer.strip_prefix(shorter)
        && WORD_FORM_ENDINGS.contains(&rest)
    {
        return true;
    }
    let mut stem_chars = shorter.chars();
    let last = stem_chars.next_back();
    let stem = stem_chars.as_str();
    let Some(ending) = longer.strip_prefix(stem) else {
        return false;
    };
    match last {
        Some('e') => SILENT_E_ENDINGS.contains(&ending),
        Some('y') => FINAL_Y_ENDINGS.contains(&ending),
        _ => false,
    }
}

fn clamp01(value: f64) -> f64 {
    value.clamp(0.0, 1.0)
}

/// How well `content` covers the query groups: whole-token matches count 1, other word forms 0.5;
/// a one-group query scores 0.7 plus 0.1 per extra occurrence (max 1.0), or 0.35 for a word form.
/// CJK query tokens match as substrings because CJK text need not separate words.
pub fn lexical_group_relevance(groups: &[Vec<String>], content: &str) -> f64 {
    if groups.is_empty() {
        return 0.0;
    }
    let tokens = tokenize(content);
    let mut content_tokens: BTreeSet<&str> = tokens.iter().map(String::as_str).collect();
    for token in &tokens {
        if token.contains('_') {
            content_tokens.extend(token.split('_').filter(|part| !part.is_empty()));
        }
    }
    let cjk_content = groups
        .iter()
        .any(|group| group.iter().any(|token| has_cjk(token)))
        .then(|| content.to_lowercase());
    let mut matched = 0.0;
    let mut partial = 0.0;
    for group in groups {
        let whole = group.iter().any(|token| {
            content_tokens.contains(token.as_str())
                || (has_cjk(token)
                    && cjk_content
                        .as_deref()
                        .is_some_and(|text| text.contains(token.as_str())))
        });
        if whole {
            matched += 1.0;
        } else if group.iter().any(|token| {
            content_tokens
                .iter()
                .any(|content_token| matches_word_form(token, content_token))
        }) {
            partial += 1.0;
        }
    }
    if groups.len() != 1 {
        return (matched + partial * 0.5) / groups.len() as f64;
    }
    if matched == 0.0 {
        return if partial > 0.0 { 0.35 } else { 0.0 };
    }
    let Some(token) = groups[0].first() else {
        return 0.0;
    };
    let count = match (&cjk_content, has_cjk(token)) {
        (Some(text), true) => text.matches(token.as_str()).count(),
        _ => tokens
            .iter()
            .filter(|content_token| *content_token == token)
            .count(),
    };
    clamp01(0.7 + (count.saturating_sub(1).min(3)) as f64 * 0.1)
}

/// The lowest lexical relevance a candidate needs for its query length.
pub fn minimum_relevance(token_count: usize) -> f64 {
    match token_count {
        0 | 1 => 0.08,
        2 => 0.18,
        3 => 0.34,
        _ => 0.22,
    }
}

fn contains_word(text: &str, words: &[&str]) -> bool {
    text.to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .any(|word| words.contains(&word))
}

/// The query asks about the present (`now`, `current`, `latest`, ...).
pub fn query_asks_current(query: &str) -> bool {
    contains_word(
        query,
        &[
            "now",
            "current",
            "currently",
            "latest",
            "recent",
            "today",
            "active",
            "present",
        ],
    )
}

/// For a present-tense query: favour content that says it is current, demote content that says it
/// is old.
pub fn current_content_adjustment(content: &str, current_sensitive: bool) -> f64 {
    if !current_sensitive {
        return 1.0;
    }
    let mut factor = 1.0;
    if contains_word(
        content,
        &["current", "currently", "latest", "now", "active", "present"],
    ) {
        factor *= 1.35;
    }
    if contains_word(
        content,
        &[
            "was",
            "previous",
            "previously",
            "legacy",
            "old",
            "stale",
            "former",
            "deprecated",
        ],
    ) {
        factor *= 0.72;
    }
    factor
}

fn fts_phrase(token: &str) -> String {
    format!("\"{}\"", token.replace('"', "\"\""))
}

/// FTS5 MATCH expression: up to twelve expanded tokens as quoted phrases joined by `OR`, or the
/// whole query as one phrase when it has no tokens.
pub fn fts_query(query: &str, use_synonyms: bool) -> String {
    let tokens = expanded_tokens(query, use_synonyms);
    if tokens.is_empty() {
        return fts_phrase(query.trim());
    }
    tokens
        .iter()
        .take(12)
        .map(|token| fts_phrase(token))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// Default recall preview length in characters.
pub const RECALL_CONTENT_PREVIEW_CHARS: usize = 500;

/// `content` clipped to `limit` characters, the last one replaced by `…` when cut; returns the
/// text, whether it was cut, and the full length in characters.
pub fn clip(content: &str, limit: usize) -> (String, bool, usize) {
    let full = content.chars().count();
    if limit == 0 || full <= limit {
        return (content.to_owned(), false, full);
    }
    let head: String = content.chars().take(limit - 1).collect();
    (format!("{head}…"), true, full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_lowercases_and_drops_stop_words() {
        assert_eq!(
            tokenize("The Deploy target is stable_cluster!"),
            ["deploy", "target", "stable_cluster"]
        );
    }

    #[test]
    fn synonyms_follow_groups_and_later_groups_win() {
        assert_eq!(synonyms("DB")[0], "database");
        // `token` appears in both the password and key groups; the later group wins.
        assert_eq!(synonyms("token")[0], "key");
        assert_eq!(synonyms("zebra"), ["zebra"]);
    }

    #[test]
    fn word_forms_match_only_known_endings() {
        assert!(matches_word_form("backup", "backups"));
        assert!(matches_word_form("facts", "fact"));
        assert!(matches_word_form("caching", "cache"));
        assert!(matches_word_form("stories", "story"));
        assert!(matches_word_form("deploy", "deployment"));
        assert!(!matches_word_form("pass", "1password"));
        assert!(!matches_word_form("redis", "redistribution"));
        assert!(!matches_word_form("cat", "cats"));
    }

    #[test]
    fn relevance_counts_whole_and_partial_matches() {
        let groups = expanded_token_groups("deploy target", false);
        assert!((lexical_group_relevance(&groups, "deploy target ready") - 1.0).abs() < 1e-9);
        assert!((lexical_group_relevance(&groups, "deployment notes") - 0.25).abs() < 1e-9);
        let single = expanded_token_groups("cluster", false);
        assert!((lexical_group_relevance(&single, "cluster cluster cluster") - 0.9).abs() < 1e-9);
        assert!((lexical_group_relevance(&single, "clusters") - 0.35).abs() < 1e-9);
    }

    #[test]
    fn cjk_tokens_match_inside_text() {
        let groups = expanded_token_groups("배포", false);
        assert!(lexical_group_relevance(&groups, "금요일에배포한다") > 0.6);
    }

    #[test]
    fn fts_query_quotes_tokens() {
        assert_eq!(fts_query("say \"hi\"", false), "\"say\" OR \"hi\"");
        assert_eq!(fts_query("the", false), "\"the\"");
    }

    #[test]
    fn clip_marks_truncation() {
        assert_eq!(clip("abcdef", 4), ("abc…".to_owned(), true, 6));
        assert_eq!(clip("abc", 4), ("abc".to_owned(), false, 3));
    }

    #[test]
    fn current_queries_adjust_content() {
        assert!(query_asks_current("what is the current target"));
        assert!(current_content_adjustment("currently prod", true) > 1.0);
        assert!(current_content_adjustment("the old target", true) < 1.0);
    }
}
