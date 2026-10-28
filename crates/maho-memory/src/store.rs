//! One memory bank (one SQLite file): writes, reads and maintenance.
//!
//! Port of mnemopi `core/beam/store.ts` (`remember` with same-session duplicate merge, `get`,
//! `updateWorking`, `forgetWorking`, `invalidate`, `getWorkingStats`, the scratchpad,
//! `exportToDict`/`importFromDict`), `core/triples.ts` (`TripleStore.add`/`query`), `db.ts`
//! (connection pragmas) and `util/ids.ts` (`generateId`, `stableMemoryId`).
use crate::schema;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub type Result<T> = std::result::Result<T, String>;

pub(crate) fn sql_err(error: rusqlite::Error) -> String {
    format!("memory store: {error}")
}

/// Current time as UTC ISO-8601 with milliseconds and `Z` (`toUtcIso`).
pub fn now_iso() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// First 16 hex characters of SHA-256 (`sha256Hex16`).
pub fn sha256_hex16(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh id for `content` written at `timestamp` (`generateId`: content, time and a per-process
/// nonce).
pub fn generate_id(content: &str, timestamp: &str) -> String {
    let counter = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nonce = format!(
        "{}:{}:{counter}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    sha256_hex16(&format!("{content}\0{timestamp}\0{nonce}"))
}

/// Deterministic id of `content` from `source` (`stableMemoryId`).
pub fn stable_memory_id(content: &str, source: &str) -> String {
    if source.is_empty() {
        sha256_hex16(content)
    } else {
        sha256_hex16(&format!("{content}\0{source}"))
    }
}

const VERACITY: &[&str] = &[
    "true", "false", "stated", "inferred", "tool", "imported", "unknown",
];
const TRUST_TIERS: &[&str] = &["STATED", "DERIVED", "EXTERNAL_WRITE", "IMPORTED"];

/// A canonical veracity label, `unknown` for anything else.
pub fn clamp_veracity(value: &str) -> String {
    let normalized = value.trim().to_lowercase();
    if VERACITY.contains(&normalized.as_str()) {
        normalized
    } else {
        "unknown".to_owned()
    }
}

fn source_trust_tier(source: &str) -> &'static str {
    match source.to_lowercase().as_str() {
        "tool" | "api" | "system" => "EXTERNAL_WRITE",
        "import" | "imported" | "backup" => "IMPORTED",
        _ => "STATED",
    }
}

/// Options of [`Store::remember`]; `Default` matches upstream defaults except `durable`.
#[derive(Clone, Debug)]
pub struct RememberOptions {
    pub source: String,
    pub importance: f64,
    pub timestamp: Option<String>,
    /// `session`, `global`, `channel` or a custom scope.
    pub scope: String,
    pub veracity: String,
    pub trust_tier: Option<String>,
    pub memory_type: String,
    pub valid_until: Option<String>,
    pub metadata: Option<Value>,
    pub memory_id: Option<String>,
    pub author_id: Option<String>,
    pub author_type: Option<String>,
    pub channel_id: Option<String>,
    /// Stamp `consolidated_at` so a mnemopi sleep/TTL trim never treats the row as scratch. This
    /// crate has no consolidation step, so everything it writes is durable.
    pub durable: bool,
}

impl Default for RememberOptions {
    fn default() -> Self {
        Self {
            source: "conversation".into(),
            importance: 0.5,
            timestamp: None,
            scope: "session".into(),
            veracity: "unknown".into(),
            trust_tier: None,
            memory_type: "unknown".into(),
            valid_until: None,
            metadata: None,
            memory_id: None,
            author_id: None,
            author_type: None,
            channel_id: None,
            durable: true,
        }
    }
}

/// A stored memory as `get` returns it.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct MemoryRow {
    pub id: String,
    pub content: String,
    pub source: Option<String>,
    pub timestamp: Option<String>,
    pub session_id: Option<String>,
    pub importance: f64,
    pub metadata: Option<String>,
    pub veracity: Option<String>,
    pub created_at: Option<String>,
    pub valid_until: Option<String>,
    pub superseded_by: Option<String>,
    pub memory_store: String,
}

/// Counts for `stats`.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Stats {
    pub working: i64,
    pub working_active: i64,
    pub episodic: i64,
    pub scratchpad: i64,
    pub triples: i64,
    pub last: Option<String>,
}

/// A temporal fact triple.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Triple {
    pub id: i64,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub valid_from: String,
    pub valid_until: Option<String>,
    pub source: Option<String>,
    pub confidence: f64,
}

/// Counters of [`Store::import_json`].
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct ImportStats {
    pub inserted: u64,
    pub skipped: u64,
    pub overwritten: u64,
    pub scratchpad: u64,
}

/// An open bank.
pub struct Store {
    pub(crate) conn: Connection,
    pub(crate) session_id: String,
    path: Option<PathBuf>,
}

impl Store {
    /// Opens (creating directories and schema) the bank at `path` for `session_id`.
    pub fn open(path: &Path, session_id: &str) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                format!("memory store: cannot create {}: {error}", parent.display())
            })?;
        }
        let conn = Connection::open(path).map_err(sql_err)?;
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")
            .map_err(sql_err)?;
        // WAL is best effort: some filesystems refuse it, and the bank still works without it.
        let _ = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0));
        Self::with_connection(conn, session_id, Some(path.to_path_buf()))
    }

    /// An in-memory bank (tests, dry runs).
    pub fn open_in_memory(session_id: &str) -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(sql_err)?;
        Self::with_connection(conn, session_id, None)
    }

    fn with_connection(conn: Connection, session_id: &str, path: Option<PathBuf>) -> Result<Self> {
        schema::init(&conn).map_err(sql_err)?;
        Ok(Self {
            conn,
            session_id: session_id.to_owned(),
            path,
        })
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Stores `content`. Identical content already stored in this session is refreshed instead
    /// (importance raised to the larger value, timestamp/source updated) and keeps its id.
    pub fn remember(&self, content: &str, options: &RememberOptions) -> Result<String> {
        let timestamp = options.timestamp.clone().unwrap_or_else(now_iso);
        let veracity = clamp_veracity(&options.veracity);
        let trust_tier = options
            .trust_tier
            .as_deref()
            .filter(|tier| TRUST_TIERS.contains(tier))
            .unwrap_or_else(|| source_trust_tier(&options.source))
            .to_owned();
        let metadata = options.metadata.as_ref().map(Value::to_string);
        let consolidated = options.durable.then(now_iso);
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM working_memory WHERE content = ? AND session_id = ? LIMIT 1",
                params![content, self.session_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_err)?;
        if let Some(id) = existing {
            self.conn
                .execute(
                    "UPDATE working_memory
                     SET importance = MAX(importance, ?1), timestamp = ?2, source = ?3,
                         valid_until = COALESCE(?4, valid_until),
                         scope = COALESCE(?5, scope),
                         author_id = COALESCE(?6, author_id),
                         author_type = COALESCE(?7, author_type),
                         channel_id = COALESCE(?8, channel_id),
                         memory_type = COALESCE(?9, memory_type),
                         veracity = CASE WHEN ?10 != 'unknown' THEN ?10 ELSE veracity END,
                         trust_tier = COALESCE(?11, trust_tier),
                         consolidated_at = ?12
                     WHERE id = ?13 AND session_id = ?14",
                    params![
                        options.importance,
                        timestamp,
                        options.source,
                        options.valid_until,
                        options.scope,
                        options.author_id,
                        options.author_type,
                        options.channel_id,
                        options.memory_type,
                        veracity,
                        trust_tier,
                        consolidated,
                        id,
                        self.session_id
                    ],
                )
                .map_err(sql_err)?;
            return Ok(id);
        }
        let id = options
            .memory_id
            .clone()
            .unwrap_or_else(|| generate_id(content, &timestamp));
        self.conn
            .execute(
                "INSERT INTO working_memory
                 (id, content, source, timestamp, session_id, importance, metadata_json, valid_until, scope,
                  author_id, author_type, channel_id, veracity, memory_type, trust_tier, consolidated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    id,
                    content,
                    options.source,
                    timestamp,
                    self.session_id,
                    options.importance,
                    metadata,
                    options.valid_until,
                    options.scope,
                    options.author_id,
                    options.author_type,
                    options.channel_id,
                    veracity,
                    options.memory_type,
                    trust_tier,
                    consolidated
                ],
            )
            .map_err(sql_err)?;
        Ok(id)
    }

    /// True when a working or episodic row has this id.
    pub fn contains(&self, id: &str) -> Result<bool> {
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM working_memory WHERE id = ?1 UNION ALL SELECT 1 FROM episodic_memory WHERE id = ?1 LIMIT 1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_err)?;
        Ok(found.is_some())
    }

    fn row_to_memory(row: &Row<'_>, store: &str) -> rusqlite::Result<MemoryRow> {
        Ok(MemoryRow {
            id: row.get(0)?,
            content: row.get(1)?,
            source: row.get(2)?,
            timestamp: row.get(3)?,
            session_id: row.get(4)?,
            importance: row.get::<_, Option<f64>>(5)?.unwrap_or(0.5),
            metadata: row.get(6)?,
            veracity: row.get(7)?,
            created_at: row.get(8)?,
            valid_until: row.get(9)?,
            superseded_by: row.get(10)?,
            memory_store: store.to_owned(),
        })
    }

    /// The full row with this id (working first, then episodic rows visible to this session).
    pub fn get(&self, id: &str) -> Result<Option<MemoryRow>> {
        const COLUMNS: &str = "id, content, source, timestamp, session_id, importance, metadata_json, veracity, created_at, valid_until, superseded_by";
        let working = self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM working_memory WHERE id = ?"),
                [id],
                |row| Self::row_to_memory(row, "working"),
            )
            .optional()
            .map_err(sql_err)?;
        if working.is_some() {
            return Ok(working);
        }
        self.conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM episodic_memory WHERE id = ? AND (session_id = ? OR scope = 'global')"),
                params![id, self.session_id],
                |row| Self::row_to_memory(row, "episodic"),
            )
            .optional()
            .map_err(sql_err)
    }

    /// Replaces the content and/or importance of a working memory visible to this session.
    pub fn update(&self, id: &str, content: Option<&str>, importance: Option<f64>) -> Result<bool> {
        if content.is_none() && importance.is_none() {
            return Ok(false);
        }
        let changed = self
            .conn
            .execute(
                "UPDATE working_memory
                 SET content = COALESCE(?1, content),
                     embed_text = CASE WHEN ?1 IS NULL THEN embed_text ELSE NULL END,
                     importance = COALESCE(?2, importance)
                 WHERE id = ?3 AND (session_id = ?4 OR scope = 'global')",
                params![content, importance, id, self.session_id],
            )
            .map_err(sql_err)?;
        Ok(changed > 0)
    }

    /// Permanently deletes a working memory visible to this session.
    pub fn forget(&self, id: &str) -> Result<bool> {
        let changed = self
            .conn
            .execute(
                "DELETE FROM working_memory WHERE id = ?1 AND (session_id = ?2 OR scope = 'global')",
                params![id, self.session_id],
            )
            .map_err(sql_err)?;
        Ok(changed > 0)
    }

    /// Marks a memory expired now, optionally superseded by `replacement`.
    pub fn invalidate(&self, id: &str, replacement: Option<&str>) -> Result<bool> {
        let now = now_iso();
        for table in ["working_memory", "episodic_memory"] {
            let changed = self
                .conn
                .execute(
                    &format!(
                        "UPDATE {table} SET valid_until = ?1, superseded_by = ?2
                         WHERE id = ?3 AND (session_id = ?4 OR scope = 'global')"
                    ),
                    params![now, replacement, id, self.session_id],
                )
                .map_err(sql_err)?;
            if changed > 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn count(&self, sql: &str) -> Result<i64> {
        self.conn
            .query_row(sql, [], |row| row.get(0))
            .map_err(sql_err)
    }

    /// Row counts of this bank.
    pub fn stats(&self) -> Result<Stats> {
        Ok(Stats {
            working: self.count("SELECT COUNT(*) FROM working_memory")?,
            working_active: self.count(
                "SELECT COUNT(*) FROM working_memory WHERE superseded_by IS NULL AND (valid_until IS NULL OR valid_until > strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            )?,
            episodic: self.count("SELECT COUNT(*) FROM episodic_memory")?,
            scratchpad: self.count("SELECT COUNT(*) FROM scratchpad")?,
            triples: self.count("SELECT COUNT(*) FROM triples")?,
            last: self
                .conn
                .query_row(
                    "SELECT timestamp FROM working_memory ORDER BY timestamp DESC LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sql_err)?
                .flatten(),
        })
    }

    /// The most recent active working memories, newest first.
    pub fn recent(&self, limit: usize) -> Result<Vec<MemoryRow>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT id, content, source, timestamp, session_id, importance, metadata_json, veracity, created_at, valid_until, superseded_by
                 FROM working_memory
                 WHERE superseded_by IS NULL AND (valid_until IS NULL OR valid_until > ?1)
                 ORDER BY timestamp DESC LIMIT ?2",
            )
            .map_err(sql_err)?;
        let rows = statement
            .query_map(params![now_iso(), limit as i64], |row| {
                Self::row_to_memory(row, "working")
            })
            .map_err(sql_err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sql_err)
    }

    /// Writes a scratchpad note for this session.
    pub fn scratchpad_write(&self, content: &str) -> Result<String> {
        let now = now_iso();
        let id = generate_id(content, &now);
        self.conn
            .execute(
                "INSERT INTO scratchpad (id, content, session_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)
                 ON CONFLICT(id) DO UPDATE SET content = excluded.content, updated_at = excluded.updated_at",
                params![id, content, self.session_id, now],
            )
            .map_err(sql_err)?;
        Ok(id)
    }

    /// This session's scratchpad notes, newest first (at most 1000).
    pub fn scratchpad_read(&self) -> Result<Vec<Value>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT id, content, created_at, updated_at FROM scratchpad WHERE session_id = ?
                 ORDER BY updated_at DESC LIMIT 1000",
            )
            .map_err(sql_err)?;
        let rows = statement
            .query_map([&self.session_id], |row| {
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "content": row.get::<_, String>(1)?,
                    "created_at": row.get::<_, Option<String>>(2)?,
                    "updated_at": row.get::<_, Option<String>>(3)?,
                }))
            })
            .map_err(sql_err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sql_err)
    }

    pub fn scratchpad_clear(&self) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM scratchpad WHERE session_id = ?",
                [&self.session_id],
            )
            .map(|_| ())
            .map_err(sql_err)
    }

    /// Adds a fact triple valid from `valid_from` (default today), closing the open triple with the
    /// same subject and predicate.
    pub fn triple_add(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        valid_from: Option<&str>,
        source: &str,
        confidence: f64,
    ) -> Result<i64> {
        let valid_from = valid_from.map_or_else(today, str::to_owned);
        self.conn
            .execute(
                "UPDATE triples SET valid_until = ?1 WHERE subject = ?2 AND predicate = ?3 AND valid_until IS NULL",
                params![valid_from, subject, predicate],
            )
            .map_err(sql_err)?;
        self.conn
            .execute(
                "INSERT INTO triples (subject, predicate, object, valid_from, source, confidence) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![subject, predicate, object, valid_from, source, confidence],
            )
            .map_err(sql_err)?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Triples matching the given parts that are valid at `as_of` (default today).
    pub fn triple_query(
        &self,
        subject: Option<&str>,
        predicate: Option<&str>,
        object: Option<&str>,
        as_of: Option<&str>,
    ) -> Result<Vec<Triple>> {
        let mut clauses = Vec::new();
        let mut values: Vec<String> = Vec::new();
        for (column, value) in [
            ("subject", subject),
            ("predicate", predicate),
            ("object", object),
        ] {
            if let Some(value) = value.filter(|value| !value.is_empty()) {
                clauses.push(format!("{column} = ?"));
                values.push(value.to_owned());
            }
        }
        let as_of = as_of.map_or_else(today, str::to_owned);
        clauses.push("valid_from <= ?".into());
        values.push(as_of.clone());
        clauses.push("(valid_until IS NULL OR valid_until > ?)".into());
        values.push(as_of);
        let sql = format!(
            "SELECT id, subject, predicate, object, valid_from, valid_until, source, confidence FROM triples WHERE {} ORDER BY valid_from DESC, id DESC",
            clauses.join(" AND ")
        );
        let mut statement = self.conn.prepare(&sql).map_err(sql_err)?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                Ok(Triple {
                    id: row.get(0)?,
                    subject: row.get(1)?,
                    predicate: row.get(2)?,
                    object: row.get(3)?,
                    valid_from: row.get(4)?,
                    valid_until: row.get(5)?,
                    source: row.get(6)?,
                    confidence: row.get::<_, Option<f64>>(7)?.unwrap_or(1.0),
                })
            })
            .map_err(sql_err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sql_err)
    }

    fn table_rows(&self, sql: &str) -> Result<Vec<Value>> {
        let mut statement = self.conn.prepare(sql).map_err(sql_err)?;
        let names: Vec<String> = statement
            .column_names()
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        let rows = statement
            .query_map([], |row| {
                let mut object = Map::new();
                for (index, name) in names.iter().enumerate() {
                    let value = match row.get_ref(index)? {
                        rusqlite::types::ValueRef::Null => Value::Null,
                        rusqlite::types::ValueRef::Integer(n) => json!(n),
                        rusqlite::types::ValueRef::Real(n) => json!(n),
                        rusqlite::types::ValueRef::Text(text) => {
                            json!(String::from_utf8_lossy(text))
                        }
                        rusqlite::types::ValueRef::Blob(_) => Value::Null,
                    };
                    object.insert(name.clone(), value);
                }
                Ok(Value::Object(object))
            })
            .map_err(sql_err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(sql_err)
    }

    /// The bank as mnemopi's export document (`mnemopi_export` header, `working_memory`,
    /// `episodic_memory`, `scratchpad`, `consolidation_log`).
    pub fn export_json(&self) -> Result<Value> {
        Ok(json!({
            "mnemopi_export": {
                "version": "1.0",
                "export_date": now_iso(),
                "source_db": self.path.as_ref().map_or_else(|| ":memory:".to_owned(), |p| p.display().to_string()),
                "component": "beam",
            },
            "working_memory": self.table_rows(
                "SELECT id, content, source, timestamp, session_id, importance, embed_text, metadata_json, valid_until,
                        superseded_by, scope, recall_count, last_recalled, created_at, veracity, consolidated_at,
                        memory_type, author_id, author_type, channel_id, trust_tier, event_date, event_date_precision,
                        temporal_tags
                 FROM working_memory ORDER BY session_id, timestamp")?,
            "episodic_memory": self.table_rows(
                "SELECT id, content, source, timestamp, session_id, importance, metadata_json, summary_of, valid_until,
                        superseded_by, scope, recall_count, last_recalled, created_at, veracity, memory_type
                 FROM episodic_memory ORDER BY session_id, timestamp")?,
            "episodic_embeddings": [],
            "scratchpad": self.table_rows("SELECT id, content, session_id, created_at, updated_at FROM scratchpad ORDER BY session_id, updated_at")?,
            "consolidation_log": self.table_rows("SELECT id, session_id, items_consolidated, summary_preview, created_at FROM consolidation_log ORDER BY session_id, created_at")?,
        }))
    }

    /// Imports the working memories and scratchpad of an export document. Existing ids are
    /// skipped unless `force`. Episodic rows are imported as working memories (no consolidation
    /// tier here).
    pub fn import_json(&self, data: &Value, force: bool) -> Result<ImportStats> {
        let mut stats = ImportStats::default();
        let imported_at = now_iso();
        let tx = self.conn.unchecked_transaction().map_err(sql_err)?;
        let text =
            |item: &Value, key: &str| item.get(key).and_then(Value::as_str).map(str::to_owned);
        let rows = ["working_memory", "episodic_memory"]
            .iter()
            .flat_map(|key| {
                data.get(*key)
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            });
        for item in rows {
            let Some(id) = text(&item, "id").filter(|id| !id.is_empty()) else {
                continue;
            };
            let exists = self.contains(&id)?;
            if exists && !force {
                stats.skipped += 1;
                continue;
            }
            if exists {
                tx.execute("DELETE FROM working_memory WHERE id = ?", [&id])
                    .map_err(sql_err)?;
                stats.overwritten += 1;
            } else {
                stats.inserted += 1;
            }
            let metadata = match item.get("metadata_json") {
                Some(Value::String(text)) => Some(text.clone()),
                Some(Value::Null) | None => None,
                Some(other) => Some(other.to_string()),
            };
            tx.execute(
                "INSERT INTO working_memory
                 (id, content, source, timestamp, session_id, importance, metadata_json, valid_until, superseded_by,
                  scope, recall_count, last_recalled, veracity, consolidated_at, memory_type, embed_text, author_id,
                  author_type, channel_id, trust_tier)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
                params![
                    id,
                    text(&item, "content").unwrap_or_default(),
                    text(&item, "source"),
                    text(&item, "timestamp"),
                    text(&item, "session_id").unwrap_or_else(|| "default".into()),
                    item.get("importance").and_then(Value::as_f64).unwrap_or(0.5),
                    metadata,
                    text(&item, "valid_until"),
                    text(&item, "superseded_by"),
                    text(&item, "scope").unwrap_or_else(|| "session".into()),
                    item.get("recall_count").and_then(Value::as_i64).unwrap_or(0),
                    text(&item, "last_recalled"),
                    clamp_veracity(&text(&item, "veracity").unwrap_or_default()),
                    text(&item, "consolidated_at").unwrap_or_else(|| imported_at.clone()),
                    text(&item, "memory_type").unwrap_or_else(|| "unknown".into()),
                    text(&item, "embed_text"),
                    text(&item, "author_id"),
                    text(&item, "author_type"),
                    text(&item, "channel_id"),
                    text(&item, "trust_tier").unwrap_or_else(|| "IMPORTED".into()),
                ],
            )
            .map_err(sql_err)?;
        }
        for item in data
            .get("scratchpad")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let (Some(id), Some(content)) = (text(&item, "id"), text(&item, "content")) else {
                continue;
            };
            tx.execute(
                "INSERT INTO scratchpad (id, content, session_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET content = excluded.content, updated_at = excluded.updated_at",
                params![
                    id,
                    content,
                    text(&item, "session_id").unwrap_or_else(|| "default".into()),
                    text(&item, "created_at"),
                    text(&item, "updated_at")
                ],
            )
            .map_err(sql_err)?;
            stats.scratchpad += 1;
        }
        tx.commit().map_err(sql_err)?;
        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory("s1").expect("store")
    }

    #[test]
    fn remember_merges_duplicates_in_a_session() {
        let store = store();
        let first = store
            .remember(
                "deploys go to stable-cluster",
                &RememberOptions {
                    importance: 0.4,
                    ..Default::default()
                },
            )
            .expect("first");
        let second = store
            .remember(
                "deploys go to stable-cluster",
                &RememberOptions {
                    importance: 0.9,
                    ..Default::default()
                },
            )
            .expect("second");
        assert_eq!(first, second);
        let row = store.get(&first).expect("get").expect("row");
        assert!((row.importance - 0.9).abs() < 1e-9);
        assert_eq!(store.stats().expect("stats").working, 1);
    }

    #[test]
    fn veracity_and_trust_are_normalized() {
        let store = store();
        let id = store
            .remember(
                "x",
                &RememberOptions {
                    veracity: " Tool ".into(),
                    source: "import".into(),
                    ..Default::default()
                },
            )
            .expect("id");
        let (veracity, tier): (String, String) = store
            .conn
            .query_row(
                "SELECT veracity, trust_tier FROM working_memory WHERE id = ?",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");
        assert_eq!((veracity.as_str(), tier.as_str()), ("tool", "IMPORTED"));
        assert_eq!(clamp_veracity("bogus"), "unknown");
    }

    #[test]
    fn update_forget_invalidate() {
        let store = store();
        let id = store
            .remember("old text", &RememberOptions::default())
            .expect("id");
        assert!(
            store
                .update(&id, Some("new text"), Some(0.8))
                .expect("update")
        );
        assert_eq!(
            store.get(&id).expect("get").expect("row").content,
            "new text"
        );
        assert!(store.invalidate(&id, None).expect("invalidate"));
        assert_eq!(store.stats().expect("stats").working_active, 0);
        assert!(store.forget(&id).expect("forget"));
        assert!(store.get(&id).expect("get").is_none());
        assert!(!store.forget("missing").expect("forget"));
    }

    #[test]
    fn other_sessions_cannot_touch_session_scoped_rows() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("bank.db");
        let id = Store::open(&path, "a")
            .expect("a")
            .remember("private", &RememberOptions::default())
            .expect("id");
        let other = Store::open(&path, "b").expect("b");
        assert!(!other.forget(&id).expect("forget"));
        let global = other
            .remember(
                "shared",
                &RememberOptions {
                    scope: "global".into(),
                    ..Default::default()
                },
            )
            .expect("global");
        assert!(
            Store::open(&path, "a")
                .expect("a")
                .forget(&global)
                .expect("forget")
        );
    }

    #[test]
    fn triples_close_previous_value() {
        let store = store();
        store
            .triple_add(
                "svc",
                "runs_on",
                "vm1",
                Some("2026-01-01"),
                "conversation",
                1.0,
            )
            .expect("add");
        store
            .triple_add(
                "svc",
                "runs_on",
                "vm2",
                Some("2026-02-01"),
                "conversation",
                1.0,
            )
            .expect("add");
        let now = store
            .triple_query(Some("svc"), None, None, Some("2026-03-01"))
            .expect("query");
        assert_eq!(now.len(), 1);
        assert_eq!(now[0].object, "vm2");
        let then = store
            .triple_query(Some("svc"), None, None, Some("2026-01-15"))
            .expect("query");
        assert_eq!(then[0].object, "vm1");
    }

    #[test]
    fn scratchpad_round_trip() {
        let store = store();
        store.scratchpad_write("note").expect("write");
        assert_eq!(store.scratchpad_read().expect("read").len(), 1);
        store.scratchpad_clear().expect("clear");
        assert!(store.scratchpad_read().expect("read").is_empty());
    }

    #[test]
    fn export_import_round_trip() {
        let source = store();
        let id = source
            .remember("exported fact", &RememberOptions::default())
            .expect("id");
        let data = source.export_json().expect("export");
        let target = Store::open_in_memory("t").expect("target");
        let stats = target.import_json(&data, false).expect("import");
        assert_eq!(stats.inserted, 1);
        assert_eq!(
            target.get(&id).expect("get").expect("row").content,
            "exported fact"
        );
        assert_eq!(target.import_json(&data, false).expect("again").skipped, 1);
        assert_eq!(
            target.import_json(&data, true).expect("force").overwritten,
            1
        );
    }

    #[test]
    fn ids_are_hex16() {
        assert_eq!(sha256_hex16("abc").len(), 16);
        assert_eq!(stable_memory_id("a", "b"), stable_memory_id("a", "b"));
        assert_ne!(generate_id("a", "t"), generate_id("a", "t"));
    }
}
