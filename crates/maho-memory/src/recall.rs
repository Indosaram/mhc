//! Lexical hybrid recall over one bank.
//!
//! Port of mnemopi `core/beam/recall.ts` `recall` without the vector voice: FTS5 candidates from
//! both tiers (rank normalized to 0..1), a recent-rows fallback when FTS finds nothing, the
//! keyword/word-form relevance gate, importance and recency weighting, an optional temporal boost,
//! veracity and episodic-tier trust multipliers, present-tense adjustment, preview clipping, and
//! recall-count bookkeeping. Weights and defaults follow `config.ts`.
use crate::store::{Result, Store, now_iso, sql_err};
use crate::text::{
    RECALL_CONTENT_PREVIEW_CHARS, clip, current_content_adjustment, expanded_token_groups,
    expanded_tokens, fts_query, lexical_group_relevance, minimum_relevance, query_asks_current,
};
use rusqlite::types::Value as SqlValue;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// Recall options; `Default` is mnemopi's.
#[derive(Clone, Debug)]
pub struct RecallOptions {
    /// Search every session of the bank, not only this session plus `global` rows.
    pub ignore_session_scope: bool,
    /// 0 disables the temporal boost.
    pub temporal_weight: Option<f64>,
    /// ISO time treated as now for temporal scoring.
    pub query_time: Option<String>,
    pub temporal_halflife_hours: f64,
    pub vec_weight: f64,
    pub fts_weight: f64,
    pub importance_weight: f64,
    pub preview_chars: usize,
    pub use_synonyms: bool,
    pub update_recall_counts: bool,
    pub source: Option<String>,
    pub memory_type: Option<String>,
}

impl Default for RecallOptions {
    fn default() -> Self {
        Self {
            ignore_session_scope: false,
            temporal_weight: None,
            query_time: None,
            temporal_halflife_hours: 24.0,
            vec_weight: 0.5,
            fts_weight: 0.3,
            importance_weight: 0.2,
            preview_chars: RECALL_CONTENT_PREVIEW_CHARS,
            use_synonyms: true,
            update_recall_counts: true,
            source: None,
            memory_type: None,
        }
    }
}

/// One ranked memory.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct RecallResult {
    pub id: String,
    /// Preview, clipped to `preview_chars` with a trailing `…` when cut.
    pub content: String,
    pub source: Option<String>,
    pub timestamp: Option<String>,
    pub importance: f64,
    pub score: f64,
    pub tier: String,
    pub keyword_score: f64,
    pub fts_score: f64,
    pub recency_score: f64,
    pub temporal_score: f64,
    pub veracity: String,
    pub memory_type: Option<String>,
    pub metadata_json: Option<String>,
    pub truncated: bool,
    pub full_length: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tier {
    Working,
    Episodic,
}

impl Tier {
    fn table(self) -> &'static str {
        match self {
            Self::Working => "working_memory",
            Self::Episodic => "episodic_memory",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Episodic => "episodic",
        }
    }
}

struct Candidate {
    tier: Tier,
    id: String,
    content: String,
    embed_text: Option<String>,
    source: Option<String>,
    timestamp: Option<String>,
    importance: f64,
    veracity: String,
    memory_type: Option<String>,
    metadata_json: Option<String>,
    degradation_tier: i64,
    fts: f64,
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// `normalizedRecallWeights`: negatives clamp to 0, an all-zero triple falls back to the defaults,
/// and the rest is scaled to sum to 1.
pub fn normalized_weights(vec: f64, fts: f64, importance: f64) -> (f64, f64, f64) {
    let (v, f, i) = (vec.max(0.0), fts.max(0.0), importance.max(0.0));
    let total = v + f + i;
    if total == 0.0 {
        return (0.5, 0.3, 0.2);
    }
    if (total - 1.0).abs() < 1e-10 {
        return (v, f, i);
    }
    (v / total, f / total, i / total)
}

fn parse_time(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    if let Ok(time) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(time.with_timezone(&chrono::Utc));
    }
    if let Ok(time) = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f") {
        return Some(time.and_utc());
    }
    if let Ok(time) = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S") {
        return Some(time.and_utc());
    }
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|time| time.and_utc())
}

/// `exp(-hours since timestamp / halflife)`, 0 for a missing or unreadable timestamp.
fn decay(timestamp: Option<&str>, now: chrono::DateTime<chrono::Utc>, halflife_hours: f64) -> f64 {
    let Some(time) = timestamp.and_then(parse_time) else {
        return 0.0;
    };
    let hours = ((now - time).num_milliseconds() as f64 / 3_600_000.0).max(0.0);
    (-hours / halflife_hours.max(0.001)).exp()
}

fn veracity_weight(label: &str) -> f64 {
    let env = |key: &str, default: f64| {
        std::env::var(key)
            .ok()
            .and_then(|value| value.trim().parse::<f64>().ok())
            .filter(|value| value.is_finite())
            .unwrap_or(default)
            .max(0.0)
    };
    match label {
        "true" | "likely_true" => 1.0,
        "false" => 0.0,
        "stated" => env("MNEMOPI_STATED_WEIGHT", 1.0),
        "inferred" => env("MNEMOPI_INFERRED_WEIGHT", 0.7),
        "imported" => env("MNEMOPI_IMPORTED_WEIGHT", 0.6),
        "tool" => env("MNEMOPI_TOOL_WEIGHT", 0.5),
        _ => env("MNEMOPI_UNKNOWN_WEIGHT", 0.8),
    }
}

fn tier_weight(tier: i64) -> f64 {
    match tier {
        1 => 1.0,
        2 => 0.85,
        _ => 0.7,
    }
}

impl Store {
    fn visibility(&self, prefix: &str, options: &RecallOptions) -> (String, Vec<SqlValue>) {
        let mut clauses = vec![
            format!("({prefix}valid_until IS NULL OR {prefix}valid_until > ?)"),
            format!("{prefix}superseded_by IS NULL"),
        ];
        let mut values = vec![SqlValue::Text(now_iso())];
        if !options.ignore_session_scope {
            clauses.push(format!(
                "({prefix}session_id = ? OR {prefix}scope = 'global')"
            ));
            values.push(SqlValue::Text(self.session_id.clone()));
        }
        if let Some(source) = &options.source {
            clauses.push(format!("{prefix}source = ?"));
            values.push(SqlValue::Text(source.clone()));
        }
        if let Some(memory_type) = &options.memory_type {
            clauses.push(format!("{prefix}memory_type = ?"));
            values.push(SqlValue::Text(memory_type.clone()));
        }
        (clauses.join(" AND "), values)
    }

    /// FTS hits of `tier`: key (id for working, rowid for episodic as text) → normalized rank.
    fn fts_hits(
        &self,
        tier: Tier,
        query: &str,
        limit: usize,
        use_synonyms: bool,
    ) -> HashMap<String, f64> {
        let sql = match tier {
            Tier::Working => {
                "SELECT f.id, f.rank FROM fts_working f WHERE f.fts_working MATCH ?1
                 AND EXISTS (SELECT 1 FROM working_memory w WHERE w.id = f.id AND w.superseded_by IS NULL
                     AND (w.valid_until IS NULL OR w.valid_until > ?2))
                 ORDER BY f.rank, f.id LIMIT ?3"
            }
            Tier::Episodic => {
                "SELECT CAST(f.rowid AS TEXT), f.rank FROM fts_episodes f WHERE f.fts_episodes MATCH ?1
                 AND EXISTS (SELECT 1 FROM episodic_memory e WHERE e.rowid = f.rowid AND e.superseded_by IS NULL
                     AND (e.valid_until IS NULL OR e.valid_until > ?2))
                 ORDER BY f.rank, f.rowid LIMIT ?3"
            }
        };
        let rows: Vec<(String, f64)> = (|| -> rusqlite::Result<Vec<(String, f64)>> {
            let mut statement = self.conn.prepare(sql)?;
            let rows = statement.query_map(
                rusqlite::params![fts_query(query, use_synonyms), now_iso(), limit as i64],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
            )?;
            rows.collect()
        })()
        .unwrap_or_default();
        let mut out = HashMap::new();
        if rows.is_empty() {
            return out;
        }
        let min = rows
            .iter()
            .map(|(_, rank)| *rank)
            .fold(f64::INFINITY, f64::min);
        let max = rows
            .iter()
            .map(|(_, rank)| *rank)
            .fold(f64::NEG_INFINITY, f64::max);
        let range = if max == min { 1.0 } else { max - min };
        for (key, rank) in rows {
            out.insert(key, 1.0 - (rank - min) / range);
        }
        out
    }

    fn candidates(
        &self,
        tier: Tier,
        keys: Option<&HashMap<String, f64>>,
        options: &RecallOptions,
    ) -> Result<Vec<Candidate>> {
        let (where_clause, mut values) = self.visibility("m.", options);
        let key_column = match tier {
            Tier::Working => "m.id",
            Tier::Episodic => "CAST(m.rowid AS TEXT)",
        };
        let embed = match tier {
            Tier::Working => "m.embed_text",
            Tier::Episodic => "NULL",
        };
        let degradation = match tier {
            Tier::Working => "1",
            Tier::Episodic => "COALESCE(m.tier, 1)",
        };
        let mut sql = format!(
            "SELECT {key_column}, m.id, m.content, {embed}, m.source, m.timestamp, m.importance, m.veracity,
                    m.memory_type, m.metadata_json, {degradation}
             FROM {} m WHERE {where_clause}",
            tier.table()
        );
        match keys {
            Some(keys) if !keys.is_empty() => {
                let placeholders = vec!["?"; keys.len()].join(",");
                sql.push_str(&format!(" AND {key_column} IN ({placeholders})"));
                values.extend(keys.keys().map(|key| SqlValue::Text(key.clone())));
            }
            Some(_) => return Ok(Vec::new()),
            None => sql.push_str(" ORDER BY m.timestamp DESC LIMIT 500"),
        }
        let mut statement = self.conn.prepare(&sql).map_err(sql_err)?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(values), |row| {
                let key: String = row.get(0)?;
                Ok(Candidate {
                    tier,
                    id: row.get(1)?,
                    content: row.get(2)?,
                    embed_text: row.get(3)?,
                    source: row.get(4)?,
                    timestamp: row.get(5)?,
                    importance: row.get::<_, Option<f64>>(6)?.unwrap_or(0.5),
                    veracity: row
                        .get::<_, Option<String>>(7)?
                        .unwrap_or_else(|| "unknown".into()),
                    memory_type: row.get(8)?,
                    metadata_json: row.get(9)?,
                    degradation_tier: row.get::<_, Option<i64>>(10)?.unwrap_or(1),
                    fts: keys.and_then(|keys| keys.get(&key)).copied().unwrap_or(0.0),
                })
            })
            .map_err(sql_err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sql_err)
    }

    /// The `top_k` best memories for `query`, highest score first.
    pub fn recall(
        &self,
        query: &str,
        top_k: usize,
        options: &RecallOptions,
    ) -> Result<Vec<RecallResult>> {
        if top_k == 0 {
            return Ok(Vec::new());
        }
        let mut temporal_weight = options.temporal_weight.unwrap_or(0.0);
        let mut query_time = options.query_time.as_deref().and_then(parse_time);
        let current_sensitive = query_asks_current(query);
        if current_sensitive {
            query_time.get_or_insert_with(chrono::Utc::now);
            if options.temporal_weight.is_none() {
                temporal_weight = 0.45;
            }
        }
        let now = chrono::Utc::now();
        let (_, fts_weight, importance_weight) = normalized_weights(
            options.vec_weight,
            options.fts_weight,
            options.importance_weight,
        );
        let tokens = expanded_tokens(query, options.use_synonyms);
        let groups = expanded_token_groups(query, options.use_synonyms);
        let limit = (top_k * 3).max(50);

        let mut candidates = Vec::new();
        for tier in [Tier::Working, Tier::Episodic] {
            let hits = self.fts_hits(tier, query, limit, options.use_synonyms);
            if hits.is_empty() {
                candidates.extend(self.candidates(tier, None, options)?);
            } else {
                candidates.extend(self.candidates(tier, Some(&hits), options)?);
            }
        }

        let min_relevance = minimum_relevance(tokens.len());
        let mut scored = Vec::new();
        for candidate in candidates {
            let searchable = candidate
                .embed_text
                .as_deref()
                .filter(|text| !text.is_empty())
                .unwrap_or(&candidate.content);
            let lexical = lexical_group_relevance(&groups, searchable);
            if lexical < min_relevance {
                continue;
            }
            let recency = match query_time {
                None => decay(candidate.timestamp.as_deref(), now, 72.0),
                Some(at) => decay(candidate.timestamp.as_deref(), at, 72.0),
            };
            let keyword = lexical.max(candidate.fts * 0.6);
            let base = match candidate.tier {
                Tier::Episodic => (candidate.fts * fts_weight
                    + candidate.importance * importance_weight)
                    .max(lexical * 0.8),
                Tier::Working => {
                    let share = (1.0 - importance_weight) * 0.6;
                    keyword * share
                        + candidate.importance * importance_weight
                        + keyword * keyword * 0.08
                }
            };
            let mut score = base * (0.7 + 0.3 * recency);
            let mut temporal = 0.0;
            if temporal_weight > 0.0 {
                let at = query_time.unwrap_or(now);
                temporal = decay(
                    candidate.timestamp.as_deref(),
                    at,
                    options.temporal_halflife_hours,
                );
                score *= 1.0 + temporal_weight * temporal;
            }
            if candidate.tier == Tier::Episodic {
                score *= tier_weight(candidate.degradation_tier);
            }
            score *= veracity_weight(&candidate.veracity)
                * current_content_adjustment(searchable, current_sensitive);
            let (content, truncated, full_length) = clip(&candidate.content, options.preview_chars);
            scored.push(RecallResult {
                id: candidate.id,
                content,
                source: candidate.source,
                timestamp: candidate.timestamp,
                importance: candidate.importance,
                score: round4(score),
                tier: candidate.tier.label().to_owned(),
                keyword_score: round4(lexical),
                fts_score: round4(candidate.fts),
                recency_score: round4(recency),
                temporal_score: round4(temporal),
                veracity: candidate.veracity,
                memory_type: candidate.memory_type,
                metadata_json: candidate.metadata_json,
                truncated,
                full_length,
            });
        }
        scored.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| right.timestamp.cmp(&left.timestamp))
                .then_with(|| left.content.cmp(&right.content))
        });
        let mut seen = HashSet::new();
        scored.retain(|result| seen.insert(format!("{}:{}", result.tier, result.id)));
        scored.truncate(top_k);
        if options.update_recall_counts {
            self.count_recalls(&scored)?;
        }
        Ok(scored)
    }

    fn count_recalls(&self, results: &[RecallResult]) -> Result<()> {
        let now = now_iso();
        for result in results {
            let table = if result.tier == "episodic" {
                "episodic_memory"
            } else {
                "working_memory"
            };
            self.conn
                .execute(
                    &format!("UPDATE {table} SET recall_count = COALESCE(recall_count, 0) + 1, last_recalled = ?1 WHERE id = ?2"),
                    rusqlite::params![now, result.id],
                )
                .map_err(sql_err)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::RememberOptions;

    fn remember(store: &Store, content: &str, importance: f64) -> String {
        store
            .remember(
                content,
                &RememberOptions {
                    importance,
                    scope: "global".into(),
                    ..Default::default()
                },
            )
            .expect("remember")
    }

    #[test]
    fn finds_by_keyword_and_synonym() {
        let store = Store::open_in_memory("s").expect("store");
        let deploy = remember(
            &store,
            "Production deploys go to stable-cluster on Fridays.",
            0.8,
        );
        remember(&store, "The team prefers tabs over spaces.", 0.5);
        let results = store
            .recall("deployment target", 5, &RecallOptions::default())
            .expect("recall");
        assert_eq!(
            results.first().map(|r| r.id.as_str()),
            Some(deploy.as_str())
        );
        // `release` is a synonym of `deploy`.
        let results = store
            .recall("release", 5, &RecallOptions::default())
            .expect("recall");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn word_forms_reach_rows_fts_misses() {
        let store = Store::open_in_memory("s").expect("store");
        remember(&store, "Nightly backups run at 02:00.", 0.5);
        let results = store
            .recall("backup", 5, &RecallOptions::default())
            .expect("recall");
        assert_eq!(results.len(), 1);
        assert!(results[0].keyword_score > 0.0);
    }

    #[test]
    fn unrelated_queries_find_nothing() {
        let store = Store::open_in_memory("s").expect("store");
        remember(&store, "Nightly backups run at 02:00.", 0.5);
        assert!(
            store
                .recall("kubernetes ingress", 5, &RecallOptions::default())
                .expect("recall")
                .is_empty()
        );
    }

    #[test]
    fn invalidated_rows_are_hidden_and_counts_update() {
        let store = Store::open_in_memory("s").expect("store");
        let id = remember(&store, "Redis listens on port 6380.", 0.5);
        let found = store
            .recall("redis port", 5, &RecallOptions::default())
            .expect("recall");
        assert_eq!(found.len(), 1);
        let count: i64 = store
            .conn
            .query_row(
                "SELECT recall_count FROM working_memory WHERE id = ?",
                [&id],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(count, 1);
        store.invalidate(&id, None).expect("invalidate");
        assert!(
            store
                .recall("redis port", 5, &RecallOptions::default())
                .expect("recall")
                .is_empty()
        );
    }

    #[test]
    fn session_scope_is_respected() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("bank.db");
        Store::open(&path, "a")
            .expect("a")
            .remember("alpha secret plan", &RememberOptions::default())
            .expect("remember");
        let other = Store::open(&path, "b").expect("b");
        assert!(
            other
                .recall("alpha plan", 5, &RecallOptions::default())
                .expect("recall")
                .is_empty()
        );
        let all = RecallOptions {
            ignore_session_scope: true,
            ..Default::default()
        };
        assert_eq!(
            other.recall("alpha plan", 5, &all).expect("recall").len(),
            1
        );
    }

    #[test]
    fn importance_and_veracity_order_results() {
        let store = Store::open_in_memory("s").expect("store");
        let low = store
            .remember(
                "deploy notes A",
                &RememberOptions {
                    importance: 0.5,
                    veracity: "tool".into(),
                    ..Default::default()
                },
            )
            .expect("low");
        let high = store
            .remember(
                "deploy notes B",
                &RememberOptions {
                    importance: 0.5,
                    veracity: "stated".into(),
                    ..Default::default()
                },
            )
            .expect("high");
        let results = store
            .recall("deploy notes", 5, &RecallOptions::default())
            .expect("recall");
        assert_eq!(results[0].id, high);
        assert_eq!(results[1].id, low);
    }

    #[test]
    fn previews_are_clipped() {
        let store = Store::open_in_memory("s").expect("store");
        remember(&store, &format!("cluster {}", "x".repeat(800)), 0.5);
        let results = store
            .recall("cluster", 5, &RecallOptions::default())
            .expect("recall");
        assert!(results[0].truncated);
        assert_eq!(
            results[0].content.chars().count(),
            RECALL_CONTENT_PREVIEW_CHARS
        );
    }

    #[test]
    fn weights_normalize() {
        assert_eq!(normalized_weights(0.0, 0.0, 0.0), (0.5, 0.3, 0.2));
        let (v, f, i) = normalized_weights(1.0, 1.0, 2.0);
        assert!((v - 0.25).abs() < 1e-9 && (f - 0.25).abs() < 1e-9 && (i - 0.5).abs() < 1e-9);
    }
}
