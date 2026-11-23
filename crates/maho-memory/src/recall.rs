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

#[path = "recall_order.rs"]
mod recall_order;

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
    /// ISO time against which `valid_until` is checked; `None` uses the current clock.
    pub valid_at: Option<String>,
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
            valid_at: None,
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
    fn hit_key(self, row: &rusqlite::Row<'_>) -> rusqlite::Result<HitKey> {
        match self {
            Self::Working => row.get(0).map(HitKey::Working),
            Self::Episodic => row.get(0).map(HitKey::Episodic),
        }
    }
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

#[derive(Eq, Hash, PartialEq)]
enum HitKey {
    Working(String),
    Episodic(i64),
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

/// A caller-supplied bound as stored text, or the error every input layer reports for one that is
/// not an instant.
pub(crate) fn parse_valid_at(value: &str) -> Result<String> {
    crate::expiry::parse_expiry_instant(value)
        .and_then(crate::time_format::format_millis)
        .ok_or_else(|| format!("`{value}` is not an instant (try 2026-03-01 or 2026-03-01T10:30Z)"))
}

fn valid_at(options: &RecallOptions) -> String {
    options
        .valid_at
        .as_deref()
        .and_then(|given| {
            crate::expiry::parse_expiry_instant(given).and_then(crate::time_format::format_millis)
        })
        .unwrap_or_else(now_iso)
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
            format!("({prefix}timestamp IS NULL OR {prefix}timestamp <= ?)"),
            format!("{prefix}superseded_by IS NULL"),
        ];
        let mut values = vec![
            SqlValue::Text(valid_at(options)),
            SqlValue::Text(valid_at(options)),
        ];
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

    /// FTS hits of `tier`: working id or episodic integer rowid → normalized rank.
    fn fts_hits(
        &self,
        tier: Tier,
        query: &str,
        limit: usize,
        use_synonyms: bool,
        valid_at: &str,
    ) -> HashMap<HitKey, f64> {
        let sql = match tier {
            Tier::Working => {
                "SELECT f.id, f.rank FROM fts_working f WHERE f.fts_working MATCH ?1
                 AND EXISTS (SELECT 1 FROM working_memory w WHERE w.id = f.id AND w.superseded_by IS NULL
                     AND (w.valid_until IS NULL OR w.valid_until > ?2)
                     AND (w.timestamp IS NULL OR w.timestamp <= ?2))
                 ORDER BY f.rank, f.id LIMIT ?3"
            }
            Tier::Episodic => {
                "SELECT f.rowid, f.rank FROM fts_episodes f WHERE f.fts_episodes MATCH ?1
                 AND EXISTS (SELECT 1 FROM episodic_memory e WHERE e.rowid = f.rowid AND e.superseded_by IS NULL
                     AND (e.valid_until IS NULL OR e.valid_until > ?2)
                     AND (e.timestamp IS NULL OR e.timestamp <= ?2))
                 ORDER BY f.rank, f.rowid LIMIT ?3"
            }
        };
        let rows = (|| -> rusqlite::Result<Vec<(HitKey, f64)>> {
            let mut statement = self.conn.prepare(sql)?;
            let rows = statement.query_map(
                rusqlite::params![fts_query(query, use_synonyms), valid_at, limit as i64],
                |row| Ok((tier.hit_key(row)?, row.get(1)?)),
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

    fn candidate_query(
        &self,
        tier: Tier,
        keys: Option<&HashMap<HitKey, f64>>,
        options: &RecallOptions,
    ) -> (String, Vec<SqlValue>) {
        let (where_clause, mut values) = self.visibility("m.", options);
        let key_column = match tier {
            Tier::Working => "m.id",
            Tier::Episodic => "m.rowid",
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
            Some(keys) => {
                let placeholders = vec!["?"; keys.len()].join(",");
                sql.push_str(&format!(" AND {key_column} IN ({placeholders})"));
                values.extend(keys.keys().map(|key| match key {
                    HitKey::Working(id) => SqlValue::Text(id.clone()),
                    HitKey::Episodic(rowid) => SqlValue::Integer(*rowid),
                }));
            }
            None => sql.push_str(" ORDER BY m.timestamp DESC LIMIT 500"),
        }
        (sql, values)
    }

    fn candidates(
        &self,
        tier: Tier,
        keys: Option<&HashMap<HitKey, f64>>,
        options: &RecallOptions,
    ) -> Result<Vec<Candidate>> {
        let (sql, values) = self.candidate_query(tier, keys, options);
        self.lookup(tier, keys, &sql, &values, &mut |_, _| {})
            .map_err(sql_err)
    }

    fn lookup(
        &self,
        tier: Tier,
        keys: Option<&HashMap<HitKey, f64>>,
        sql: &str,
        values: &[SqlValue],
        observe: &mut impl FnMut(&str, &rusqlite::Statement<'_>),
    ) -> rusqlite::Result<Vec<Candidate>> {
        if tier == Tier::Working {
            return self.read_candidates(tier, keys, sql, values, false, observe);
        }
        let cast_sql = sql.replace("m.rowid", "CAST(m.rowid AS TEXT)");
        if keys.is_none() {
            return self.read_candidates(tier, keys, &cast_sql, values, true, observe);
        }
        let cast_values: Vec<_> = values
            .iter()
            .map(|v| match v {
                SqlValue::Integer(id) => SqlValue::Text(id.to_string()),
                value => value.clone(),
            })
            .collect();
        let mut guard = self.conn.prepare(recall_order::ROOTS)?;
        let mut snapshot = guard.query([])?;
        let root = snapshot
            .next()?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        let roots = [root.get(0)?, root.get(1)?, root.get(2)?];
        let result = match recall_order::order(&self.conn, &cast_sql, &cast_values, roots, observe)?
        {
            Some(order) => {
                // NOT INDEXED leaves the integer primary key available. Don't let an index
                // scan to satisfy ORDER BY turn this back into bank-sized work.
                let seek = sql.replace(
                    "episodic_memory m WHERE",
                    "episodic_memory m NOT INDEXED WHERE",
                );
                self.read_candidates(
                    tier,
                    keys,
                    &format!("{seek} ORDER BY {order}"),
                    values,
                    false,
                    observe,
                )
            }
            None => self.read_candidates(tier, keys, &cast_sql, &cast_values, true, observe),
        };
        // Finish the guard too (not just the candidate cursor). It is an ungrouped aggregate
        // so has exactly one row. The step to DONE releases our last read before scoring.
        let _ = snapshot.next()?;
        drop(snapshot);
        observe("schema", &guard);
        result
    }

    fn read_candidates(
        &self,
        tier: Tier,
        keys: Option<&HashMap<HitKey, f64>>,
        sql: &str,
        values: &[SqlValue],
        cast: bool,
        observe: &mut impl FnMut(&str, &rusqlite::Statement<'_>),
    ) -> rusqlite::Result<Vec<Candidate>> {
        let mut statement = self.conn.prepare(sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(values), |row| {
            let fts = if cast {
                let text: String = row.get(0)?;
                text.parse::<i64>()
                    .ok()
                    .and_then(|id| keys?.get(&HitKey::Episodic(id)))
            } else {
                let key = tier.hit_key(row)?;
                keys.and_then(|hits| hits.get(&key))
            };
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
                fts: fts.copied().unwrap_or(0.0),
            })
        })?;
        let candidates = rows.collect();
        observe(sql, &statement);
        candidates
    }

    /// The `top_k` best memories for `query`, highest score first.
    pub fn recall(
        &self,
        query: &str,
        top_k: usize,
        options: &RecallOptions,
    ) -> Result<Vec<RecallResult>> {
        self.recall_after_fetch(query, top_k, options, || {})
    }

    fn recall_after_fetch(
        &self,
        query: &str,
        top_k: usize,
        options: &RecallOptions,
        after_fetch: impl FnOnce(),
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

        let valid_at = valid_at(options);
        let mut candidates = Vec::new();
        for tier in [Tier::Working, Tier::Episodic] {
            let hits = self.fts_hits(tier, query, limit, options.use_synonyms, &valid_at);
            if hits.is_empty() {
                candidates.extend(self.candidates(tier, None, options)?);
            } else {
                candidates.extend(self.candidates(tier, Some(&hits), options)?);
            }
        }

        after_fetch();
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
    fn episodic_ranks_follow_native_rowids_through_visibility_filters() {
        let store = Store::open_in_memory("s").expect("store");
        store
            .conn
            .execute_batch(
                "INSERT INTO episodic_memory(rowid,id,content,source,memory_type) VALUES
             (4294967297,'007','cache cache cache','test','fact'),
             (1,'4294967297','cache plus many unrelated extra words','test','fact'),
             (2,'wrong-source','cache','other','fact'), (3,'wrong-type','cache','test','other');",
            )
            .expect("episodes");
        let options = RecallOptions {
            source: Some("test".into()),
            memory_type: Some("fact".into()),
            valid_at: Some("2026-11-19".into()),
            query_time: Some("2026-11-19".into()),
            ..Default::default()
        };
        let results = store.recall("cache", 5, &options).expect("recall");
        let ranked: Vec<_> = results
            .iter()
            .map(|r| (r.id.as_str(), r.fts_score))
            .collect();
        assert_eq!(ranked, [("007", 1.0), ("4294967297", 0.0)]);
    }

    #[test]
    fn episodic_hit_statement_seeks_fifty_rows_at_both_bank_sizes() {
        use rusqlite::{StatementStatus, params_from_iter};

        let store = Store::open_in_memory("s").expect("store");
        let options = RecallOptions {
            valid_at: Some("2026-11-19".into()),
            query_time: Some("2026-11-19".into()),
            ignore_session_scope: true,
            ..Default::default()
        };
        let mut first = 1;
        for size in [10_000, 100_000] {
            store
                .conn
                .execute(
                    "WITH RECURSIVE n(i) AS (VALUES(?1) UNION ALL SELECT i+1 FROM n WHERE i<?2)
                 INSERT INTO episodic_memory(rowid,id,content)
                 SELECT 4294967296+i, printf('e%d', i),
                        CASE WHEN i<=50 THEN 'cache' ELSE 'filler' END FROM n",
                    [first, size],
                )
                .expect("seed bank");
            first = size + 1;
            let hits = store.fts_hits(Tier::Episodic, "cache", 50, true, &valid_at(&options));
            assert_eq!(hits.len(), 50);
            let (sql, values) = store.candidate_query(Tier::Episodic, Some(&hits), &options);
            let integer_keys = values
                .iter()
                .filter(|v| matches!(v, SqlValue::Integer(_)))
                .count();
            assert_eq!(integer_keys, 50);

            // Observe the actual statements run by lookup, after drain/reset. No simulated
            // native-only measurement, and no credit for a seek followed by a legacy scan.
            let mut statements = Vec::new();
            let result = store
                .lookup(
                    Tier::Episodic,
                    Some(&hits),
                    &sql,
                    &values,
                    &mut |kind, stmt| {
                        statements.push((
                            kind.to_owned(),
                            stmt.get_status(StatementStatus::FullscanStep),
                            stmt.get_status(StatementStatus::VmStep),
                        ));
                    },
                )
                .expect("lookup");
            let ids: Vec<_> = result.into_iter().map(|row| row.id).collect();
            assert_eq!(statements.len(), 3, "{statements:?}");
            assert_eq!(statements[0].0, "explain");
            assert_eq!(statements[2].0, "schema");
            let plan_for = |query: &str, bindings: &[SqlValue]| {
                store
                    .conn
                    .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
                    .expect("explain")
                    .query_map(params_from_iter(bindings), |r| r.get::<_, String>(3))
                    .expect("plan")
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .expect("details")
            };
            let (seek, scans, native_vm) = &statements[1];
            let plan = plan_for(seek, &values);
            assert!(
                plan.iter().any(|s| s.contains("INTEGER PRIMARY KEY")),
                "{plan:?}"
            );
            assert_eq!(*scans, 0);
            let cast_sql = sql.replace("m.rowid", "CAST(m.rowid AS TEXT)");
            let cast_values: Vec<_> = values
                .iter()
                .map(|v| match v {
                    SqlValue::Integer(rowid) => SqlValue::Text(rowid.to_string()),
                    value => value.clone(),
                })
                .collect();
            let cast_plan = plan_for(&cast_sql, &cast_values);
            let mut control = store.conn.prepare(&cast_sql).expect("prepare control");
            let old_ids = control
                .query_map(params_from_iter(&cast_values), |r| r.get::<_, String>(1))
                .expect("cast")
                .collect::<rusqlite::Result<Vec<_>>>()
                .expect("drain");
            assert_eq!(ids.len(), 50);
            assert_eq!(ids, old_ids);
            let cast_vm = control.get_status(StatementStatus::VmStep);
            let cast_scans = control.get_status(StatementStatus::FullscanStep);
            let total_vm: i32 = statements.iter().map(|s| s.2).sum();
            let total_scans: i32 = statements.iter().map(|s| s.1).sum();
            eprintln!(
                "episodic rows={size} hits=50 plan={plan:?} statements={statements:?} fullscan={total_scans} vm={total_vm}"
            );
            eprintln!(
                "episodic control rows={size} hits=50 plan={cast_plan:?} fullscan={cast_scans} vm={cast_vm}"
            );
            eprintln!(
                "RP2_OWNER {}",
                serde_json::json!({"tag": "seek", "argv": std::env::args().collect::<Vec<_>>(), "exe": std::env::current_exe().expect("current test executable"), "data": {"size":size,"hits":hits.len(),"integers":integer_keys,"sqlite":rusqlite::version(),"plan":plan,"controlPlan":cast_plan,"statements":statements,"controlScans":cast_scans,"controlVm":cast_vm,"ids":ids,"controlIds":old_ids}})
            );
            assert!(*native_vm > 0 && *native_vm < cast_vm);
            assert!(
                total_vm < cast_vm,
                "complete lookup {total_vm} vs {cast_vm}"
            );
        }
    }

    #[test]
    fn analyzed_episodic_ties_keep_the_baseline_winner_and_counts() {
        for limit in [1, 2, 3] {
            let store = Store::open_in_memory("s").expect("store");
            store
                .conn
                .execute_batch(
                    "WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<1000)
                     INSERT INTO episodic_memory(rowid,id,content,importance,timestamp,session_id,
                                                superseded_by,veracity,tier)
                     SELECT 4294967296+i, printf('r%d',i),
                            CASE WHEN i<=3 THEN 'cache' ELSE 'neutral' END,
                            CASE i WHEN 1 THEN 0.8 WHEN 2 THEN 0.1 ELSE 0.5 END,
                            '2000-01-01','s',CASE WHEN i>30 THEN 'r1' END,'true',
                            CASE WHEN i=3 THEN 2 ELSE 1 END FROM n;
                     ANALYZE;",
                )
                .expect("analyzed bank");
            let options = RecallOptions {
                valid_at: Some("2026-11-19".into()),
                query_time: Some("2026-11-19".into()),
                ..Default::default()
            };

            let results = store.recall("cache", limit, &options).expect("recall");

            let ranked: Vec<_> = results.iter().map(|r| (r.id.as_str(), r.score)).collect();
            let expected = [("r2", 0.392), ("r1", 0.392), ("r3", 0.3332)];
            assert_eq!(ranked, expected[..limit]);
            let counts: (i64, i64, i64) = store
                .conn
                .query_row(
                    "SELECT sum(recall_count),sum(recall_count*(id='r2')),
                            sum(recall_count*(id='r1')) FROM episodic_memory",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .expect("counts");
            let total = i64::try_from(limit).expect("k");
            eprintln!(
                "RP2_OWNER {}",
                serde_json::json!({"tag": "tie", "argv": std::env::args().collect::<Vec<_>>(), "exe": std::env::current_exe().expect("current test executable"), "data": {"limit":limit,"ranked":ranked,"counts":counts}})
            );
            assert_eq!(counts, (total, 1, i64::from(limit >= 2)));
        }
    }

    #[test]
    fn writer_can_commit_at_candidate_drain_before_scoring() {
        for mode in ["delete", "wal"] {
            let dir = tempfile::tempdir().expect("dir");
            let path = dir.path().join("bank.db");
            let store = Store::open(&path, "s").expect("store");
            let actual: String = store
                .conn
                .query_row(&format!("PRAGMA journal_mode={mode}"), [], |r| r.get(0))
                .expect("journal mode");
            assert_eq!(actual, mode);
            store
                .conn
                .execute_batch(
                    "INSERT INTO episodic_memory(id,content) VALUES ('r1','cache'),('r2','cache')",
                )
                .expect("seed");
            let writer = rusqlite::Connection::open(&path).expect("writer");
            writer
                .busy_timeout(std::time::Duration::ZERO)
                .expect("no wait");
            let options = RecallOptions::default();
            // Positive control: a real live reader blocks this commit in rollback mode.
            // In WAL mode that same reader must not block it.
            store
                .conn
                .execute_batch("SAVEPOINT caller")
                .expect("caller");
            let check = || {
                writer
                    .execute_batch(
                        "BEGIN IMMEDIATE;
                         UPDATE episodic_memory SET last_recalled='writer' WHERE id='r2'",
                    )
                    .expect("write");
                let committed = writer.execute_batch("COMMIT");
                if mode == "delete" {
                    assert_eq!(
                        committed.expect_err("live reader").sqlite_error_code(),
                        Some(rusqlite::ErrorCode::DatabaseBusy)
                    );
                    writer.execute_batch("ROLLBACK").expect("release writer");
                } else {
                    committed.expect("WAL commit");
                }
            };
            let no_counts = RecallOptions {
                update_recall_counts: false,
                ..options.clone()
            };
            store
                .recall_after_fetch("cache", 1, &no_counts, check)
                .expect("caller recall");
            assert!(!store.conn.is_autocommit());
            store
                .conn
                .execute_batch("ROLLBACK TO caller; RELEASE caller")
                .expect("release caller");

            let results = store
                .recall_after_fetch("cache", 1, &options, || {
                    writer
                        .execute_batch(
                            "BEGIN IMMEDIATE;
                             UPDATE episodic_memory SET content='changed' WHERE id='r1'; COMMIT",
                        )
                        .expect("commit before scoring");
                })
                .expect("autocommit recall");
            assert_eq!(results[0].id, "r1");
            assert_eq!(results[0].content, "cache");
            let saved: (String, i64) = writer
                .query_row(
                    "SELECT content,recall_count FROM episodic_memory WHERE id='r1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .expect("read back");
            assert_eq!(saved, ("changed".into(), 1));
        }
    }

    #[test]
    fn malformed_pair_returns_first_cast_error_in_both_traversal_directions() {
        let store = Store::open_in_memory("s").expect("store");
        store.conn.execute_batch("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<1000)
            INSERT INTO episodic_memory(rowid,id,content,importance,timestamp,session_id,superseded_by,veracity)
            SELECT 4294967296+i,printf('r%d',i), CASE WHEN i<=2 THEN 'cache' ELSE 'neutral' END,
                   CASE i WHEN 1 THEN 0.8 WHEN 2 THEN 0.1 ELSE 0.5 END, '2000-01-01','s',
                   CASE WHEN i>30 THEN 'r1' END,'true' FROM n;
            ANALYZE;
            UPDATE episodic_memory SET metadata_json=x'61' WHERE id='r1';
            UPDATE episodic_memory SET memory_type=x'62' WHERE id='r2';").expect("seed");
        let options = RecallOptions {
            valid_at: Some("2026-11-19".into()),
            ..Default::default()
        };
        for (reverse, column) in [(0, "8, name: memory_type"), (1, "9, name: metadata_json")] {
            store
                .conn
                .execute_batch(&format!("PRAGMA reverse_unordered_selects={reverse}"))
                .expect("direction");
            let err = store.recall("cache", 1, &options).expect_err("type error");
            assert_eq!(
                err,
                format!("memory store: Invalid column type Blob at index: {column}")
            );
            let counts: i64 = store
                .conn
                .query_row("SELECT sum(recall_count) FROM episodic_memory", [], |r| {
                    r.get(0)
                })
                .expect("counts");
            assert_eq!(counts, 0);

            let hits = store.fts_hits(Tier::Episodic, "cache", 50, true, &valid_at(&options));
            let (sql, values) = store.candidate_query(Tier::Episodic, Some(&hits), &options);
            let mut fetches = Vec::new();
            let attempted =
                store.lookup(Tier::Episodic, Some(&hits), &sql, &values, &mut |sql, _| {
                    if sql.starts_with("SELECT") {
                        fetches.push(sql.to_owned());
                    }
                });
            assert!(attempted.is_err());
            assert_eq!(fetches.len(), 1);
            if reverse == 0 {
                assert!(fetches[0].contains("ORDER BY m.scope COLLATE BINARY"));
            } else {
                assert_eq!(fetches[0], sql.replace("m.rowid", "CAST(m.rowid AS TEXT)"));
            }
        }
    }

    #[test]
    fn weights_normalize() {
        assert_eq!(normalized_weights(0.0, 0.0, 0.0), (0.5, 0.3, 0.2));
        let (v, f, i) = normalized_weights(1.0, 1.0, 2.0);
        assert!((v - 0.25).abs() < 1e-9 && (f - 0.25).abs() < 1e-9 && (i - 0.5).abs() < 1e-9);
    }
}
