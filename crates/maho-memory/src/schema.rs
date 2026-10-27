//! SQLite schema: the subset of mnemopi `core/beam/schema.ts` (`initBeam`) and
//! `core/triples.ts` (`initTriples`) this crate reads and writes. Table and column names, defaults,
//! FTS5 mirrors and their triggers are the upstream ones, so a bank written here opens in mnemopi
//! and the tables shared with a mnemopi bank are read as they are.
use rusqlite::Connection;

const STATEMENTS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS working_memory (
        id TEXT PRIMARY KEY,
        content TEXT NOT NULL,
        embed_text TEXT DEFAULT NULL,
        source TEXT,
        timestamp TEXT,
        session_id TEXT DEFAULT 'default',
        importance REAL DEFAULT 0.5,
        metadata_json TEXT,
        veracity TEXT DEFAULT 'unknown',
        memory_type TEXT DEFAULT 'unknown',
        consolidated_at TEXT,
        recall_count INTEGER DEFAULT 0,
        last_recalled TIMESTAMP DEFAULT NULL,
        valid_until TIMESTAMP DEFAULT NULL,
        superseded_by TEXT DEFAULT NULL,
        scope TEXT DEFAULT 'global',
        author_id TEXT DEFAULT NULL,
        author_type TEXT DEFAULT NULL,
        channel_id TEXT DEFAULT NULL,
        trust_tier TEXT DEFAULT 'STATED',
        validator TEXT DEFAULT NULL,
        validated_at TIMESTAMP DEFAULT NULL,
        validation_count INTEGER DEFAULT 0,
        event_date TEXT DEFAULT NULL,
        event_date_precision TEXT DEFAULT 'unknown',
        temporal_tags TEXT DEFAULT '[]',
        corrected_by INTEGER DEFAULT NULL,
        created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
    )",
    "CREATE TABLE IF NOT EXISTS episodic_memory (
        rowid INTEGER PRIMARY KEY AUTOINCREMENT,
        id TEXT UNIQUE NOT NULL,
        content TEXT NOT NULL,
        source TEXT,
        timestamp TEXT,
        session_id TEXT DEFAULT 'default',
        importance REAL DEFAULT 0.5,
        metadata_json TEXT,
        summary_of TEXT DEFAULT '',
        veracity TEXT DEFAULT 'unknown',
        tier INTEGER DEFAULT 1,
        degraded_at TEXT,
        memory_type TEXT DEFAULT 'unknown',
        binary_vector BLOB,
        recall_count INTEGER DEFAULT 0,
        last_recalled TIMESTAMP DEFAULT NULL,
        valid_until TIMESTAMP DEFAULT NULL,
        superseded_by TEXT DEFAULT NULL,
        scope TEXT DEFAULT 'global',
        author_id TEXT DEFAULT NULL,
        author_type TEXT DEFAULT NULL,
        channel_id TEXT DEFAULT NULL,
        trust_tier TEXT DEFAULT 'STATED',
        validator TEXT DEFAULT NULL,
        validated_at TIMESTAMP DEFAULT NULL,
        validation_count INTEGER DEFAULT 0,
        event_date TEXT DEFAULT NULL,
        event_date_precision TEXT DEFAULT 'unknown',
        temporal_tags TEXT DEFAULT '[]',
        corrected_by INTEGER DEFAULT NULL,
        created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
    )",
    "CREATE INDEX IF NOT EXISTS idx_wm_session ON working_memory(session_id)",
    "CREATE INDEX IF NOT EXISTS idx_wm_timestamp ON working_memory(timestamp)",
    "CREATE INDEX IF NOT EXISTS idx_wm_source ON working_memory(source)",
    "CREATE INDEX IF NOT EXISTS idx_em_session ON episodic_memory(session_id)",
    "CREATE INDEX IF NOT EXISTS idx_em_timestamp ON episodic_memory(timestamp)",
    "CREATE INDEX IF NOT EXISTS idx_em_source ON episodic_memory(source)",
    "CREATE INDEX IF NOT EXISTS idx_em_tier ON episodic_memory(tier)",
    "CREATE TABLE IF NOT EXISTS scratchpad (
        id TEXT PRIMARY KEY,
        content TEXT NOT NULL,
        session_id TEXT DEFAULT 'default',
        created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
        updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
    )",
    "CREATE INDEX IF NOT EXISTS idx_sp_session ON scratchpad(session_id)",
    "CREATE VIRTUAL TABLE IF NOT EXISTS fts_episodes USING fts5(
        content,
        content='episodic_memory',
        content_rowid='rowid'
    )",
    "CREATE VIRTUAL TABLE IF NOT EXISTS fts_working USING fts5(
        id UNINDEXED,
        content
    )",
    "CREATE TRIGGER IF NOT EXISTS em_ai AFTER INSERT ON episodic_memory BEGIN
        INSERT INTO fts_episodes(rowid, content) VALUES (new.rowid, new.content);
    END",
    "CREATE TRIGGER IF NOT EXISTS em_ad AFTER DELETE ON episodic_memory BEGIN
        INSERT INTO fts_episodes(fts_episodes, rowid, content) VALUES ('delete', old.rowid, old.content);
    END",
    "CREATE TRIGGER IF NOT EXISTS em_au AFTER UPDATE ON episodic_memory BEGIN
        INSERT INTO fts_episodes(fts_episodes, rowid, content) VALUES ('delete', old.rowid, old.content);
        INSERT INTO fts_episodes(rowid, content) VALUES (new.rowid, new.content);
    END",
    "CREATE TRIGGER IF NOT EXISTS wm_ai AFTER INSERT ON working_memory BEGIN
        INSERT INTO fts_working(id, content) VALUES (new.id, COALESCE(new.embed_text, new.content));
    END",
    "CREATE TRIGGER IF NOT EXISTS wm_ad AFTER DELETE ON working_memory BEGIN
        DELETE FROM fts_working WHERE id = old.id;
    END",
    "CREATE TRIGGER IF NOT EXISTS wm_au AFTER UPDATE OF content, embed_text ON working_memory BEGIN
        DELETE FROM fts_working WHERE id = old.id;
        INSERT INTO fts_working(id, content) VALUES (new.id, COALESCE(new.embed_text, new.content));
    END",
    "CREATE INDEX IF NOT EXISTS idx_wm_unconsolidated ON working_memory(session_id, timestamp) WHERE consolidated_at IS NULL",
    "CREATE TABLE IF NOT EXISTS consolidation_log (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id TEXT,
        items_consolidated INTEGER,
        summary_preview TEXT,
        created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
    )",
    "CREATE INDEX IF NOT EXISTS idx_em_scope_imp ON episodic_memory(scope, importance) WHERE superseded_by IS NULL",
    "CREATE INDEX IF NOT EXISTS idx_wm_session_recall ON working_memory(session_id, last_recalled) WHERE valid_until IS NULL",
    "CREATE INDEX IF NOT EXISTS idx_wm_author ON working_memory(author_id)",
    "CREATE INDEX IF NOT EXISTS idx_wm_channel ON working_memory(channel_id)",
    "CREATE INDEX IF NOT EXISTS idx_em_author ON episodic_memory(author_id)",
    "CREATE INDEX IF NOT EXISTS idx_em_channel ON episodic_memory(channel_id)",
    "CREATE INDEX IF NOT EXISTS idx_wm_event_date ON working_memory(event_date)",
    "CREATE INDEX IF NOT EXISTS idx_em_event_date ON episodic_memory(event_date)",
    "CREATE TABLE IF NOT EXISTS triples (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        subject TEXT NOT NULL,
        predicate TEXT NOT NULL,
        object TEXT NOT NULL,
        valid_from TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        valid_until TEXT,
        source TEXT,
        confidence REAL DEFAULT 1.0,
        created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
    )",
    "CREATE INDEX IF NOT EXISTS idx_triples_subject ON triples(subject)",
    "CREATE INDEX IF NOT EXISTS idx_triples_predicate ON triples(predicate)",
    "CREATE INDEX IF NOT EXISTS idx_triples_object ON triples(object)",
    "CREATE INDEX IF NOT EXISTS idx_triples_valid_from ON triples(valid_from)",
];

/// Columns added by later upstream schema versions; an older bank gets them on open.
const ADDED_COLUMNS: &[(&str, &str, &str)] = &[
    ("working_memory", "veracity", "TEXT DEFAULT 'unknown'"),
    ("working_memory", "memory_type", "TEXT DEFAULT 'unknown'"),
    ("working_memory", "embed_text", "TEXT DEFAULT NULL"),
    ("working_memory", "consolidated_at", "TEXT"),
    ("working_memory", "recall_count", "INTEGER DEFAULT 0"),
    ("working_memory", "last_recalled", "TIMESTAMP DEFAULT NULL"),
    ("working_memory", "valid_until", "TIMESTAMP DEFAULT NULL"),
    ("working_memory", "superseded_by", "TEXT DEFAULT NULL"),
    ("working_memory", "scope", "TEXT DEFAULT 'global'"),
    ("working_memory", "author_id", "TEXT DEFAULT NULL"),
    ("working_memory", "author_type", "TEXT DEFAULT NULL"),
    ("working_memory", "channel_id", "TEXT DEFAULT NULL"),
    ("working_memory", "trust_tier", "TEXT DEFAULT 'STATED'"),
    ("working_memory", "event_date", "TEXT DEFAULT NULL"),
    (
        "working_memory",
        "event_date_precision",
        "TEXT DEFAULT 'unknown'",
    ),
    ("working_memory", "temporal_tags", "TEXT DEFAULT '[]'"),
    ("episodic_memory", "tier", "INTEGER DEFAULT 1"),
    ("episodic_memory", "degraded_at", "TEXT"),
    ("episodic_memory", "veracity", "TEXT DEFAULT 'unknown'"),
    ("episodic_memory", "memory_type", "TEXT DEFAULT 'unknown'"),
    ("episodic_memory", "recall_count", "INTEGER DEFAULT 0"),
    ("episodic_memory", "last_recalled", "TIMESTAMP DEFAULT NULL"),
    ("episodic_memory", "valid_until", "TIMESTAMP DEFAULT NULL"),
    ("episodic_memory", "superseded_by", "TEXT DEFAULT NULL"),
    ("episodic_memory", "scope", "TEXT DEFAULT 'global'"),
    ("episodic_memory", "trust_tier", "TEXT DEFAULT 'STATED'"),
    ("episodic_memory", "event_date", "TEXT DEFAULT NULL"),
];

fn has_column(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = statement.query_map([], |row| row.get::<_, String>(1))?;
    for name in names {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Creates missing tables, indexes, triggers and columns. Safe to run on every open.
pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    // Tables first (CREATE TABLE IF NOT EXISTS leaves an older bank's tables as they are), then
    // the added columns, then everything that may refer to them.
    for statement in STATEMENTS.iter().filter(|s| s.starts_with("CREATE TABLE")) {
        conn.execute_batch(statement)?;
    }
    for (table, column, definition) in ADDED_COLUMNS {
        if !has_column(conn, table, column)? {
            conn.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {definition}"
            ))?;
        }
    }
    for statement in STATEMENTS.iter().filter(|s| !s.starts_with("CREATE TABLE")) {
        conn.execute_batch(statement)?;
    }
    Ok(())
}
