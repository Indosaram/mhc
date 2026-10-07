//! Port of `test/fakes.ts`: deterministic local adapters and the shared harness.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use get_worker::{
    Body, CacheKey, CachePopulation, DataPoint, DbError, DeferredWork, DownloadDatabase,
    EdgeCache, Env, FetchError, FetchRequest, FetchResponse, HttpFetcher, HttpMetadata, ObjectMeta,
    ReleaseStore, RequestContext, Response, Row, Statement, StoreError, StoredObject, Value,
};
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{params_from_iter, Connection, Error as SqlError};

/// One stored object: its bytes plus the `R2HTTPMetadata` an `R2Bucket.put` would attach.
#[derive(Clone, Default)]
struct StoredEntry {
    body: Vec<u8>,
    metadata: HttpMetadata,
}

#[derive(Default)]
struct BucketState {
    objects: RefCell<BTreeMap<String, StoredEntry>>,
    reads: RefCell<Vec<String>>,
    fail_with: RefCell<Option<String>>,
}

/// In-memory `RELEASES`; clones share one bucket.
#[derive(Clone, Default)]
pub struct MemoryReleaseStore {
    state: Rc<BucketState>,
}

impl MemoryReleaseStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&self, key: &str, body: impl AsRef<[u8]>) {
        self.put_with_metadata(key, body, HttpMetadata::default());
    }

    /// `R2Bucket.put(key, value, { httpMetadata })`: stores the object's HTTP metadata.
    pub fn put_with_metadata(&self, key: &str, body: impl AsRef<[u8]>, metadata: HttpMetadata) {
        self.state.objects.borrow_mut().insert(
            key.to_string(),
            StoredEntry {
                body: body.as_ref().to_vec(),
                metadata,
            },
        );
    }

    pub fn clear(&self) {
        self.state.objects.borrow_mut().clear();
    }

    #[must_use]
    pub fn reads(&self) -> Vec<String> {
        self.state.reads.borrow().clone()
    }

    pub fn fail_with(&self, message: &str) {
        *self.state.fail_with.borrow_mut() = Some(message.to_string());
    }

    fn read(&self, key: &str) -> Result<Option<StoredEntry>, StoreError> {
        self.state.reads.borrow_mut().push(key.to_string());
        if let Some(message) = self.state.fail_with.borrow().as_ref() {
            return Err(StoreError {
                message: message.clone(),
            });
        }
        Ok(self.state.objects.borrow().get(key).cloned())
    }
}

impl ReleaseStore for MemoryReleaseStore {
    fn head(&self, key: &str) -> Result<Option<ObjectMeta>, StoreError> {
        Ok(self.read(key)?.map(|stored| ObjectMeta {
            key: key.to_string(),
            size: stored.body.len(),
        }))
    }

    fn get(&self, key: &str) -> Result<Option<StoredObject>, StoreError> {
        Ok(self.read(key)?.map(|stored| StoredObject {
            key: key.to_string(),
            size: stored.body.len(),
            http_etag: format!("\"{key}\""),
            body: Body::Bytes(stored.body),
            http_metadata: stored.metadata,
        }))
    }
}

/// Recording `DOWNLOADS`; clones share one log.
#[derive(Clone, Default)]
pub struct RecordingSink {
    points: Rc<RefCell<Vec<DataPoint>>>,
}

impl RecordingSink {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn points(&self) -> Vec<DataPoint> {
        self.points.borrow().clone()
    }
}

impl get_worker::AnalyticsSink for RecordingSink {
    fn write_data_point(&self, point: DataPoint) {
        self.points.borrow_mut().push(point);
    }
}

/// In-memory edge `cache`, keyed by URL as the upstream fake is; clones share it.
#[derive(Clone, Default)]
pub struct MemoryEdgeCache {
    entries: Rc<RefCell<BTreeMap<String, Response>>>,
}

impl MemoryEdgeCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.borrow().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.borrow().is_empty()
    }
}

impl EdgeCache for MemoryEdgeCache {
    fn match_request(&self, key: &CacheKey) -> Option<Response> {
        self.entries.borrow().get(&key.url).cloned()
    }

    fn put(&self, key: &CacheKey, response: Response) {
        self.entries.borrow_mut().insert(key.url.clone(), response);
    }
}

/// Collects `waitUntil` work so a test settles it deterministically; clones share it.
#[derive(Clone, Default)]
pub struct PendingWork {
    populations: Rc<RefCell<Vec<CachePopulation>>>,
}

impl PendingWork {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn settle(&self, cache: &dyn EdgeCache) {
        let queued: Vec<CachePopulation> = self.populations.borrow_mut().drain(..).collect();
        for population in queued {
            cache.put(&population.key, population.response);
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.populations.borrow().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.populations.borrow().is_empty()
    }
}

impl DeferredWork for PendingWork {
    fn wait_until(&self, population: CachePopulation) {
        self.populations.borrow_mut().push(population);
    }
}

/// A `fetch` seam backed by a fixed response table keyed by URL.
#[derive(Clone, Default)]
pub struct StaticFetcher {
    responses: Rc<BTreeMap<String, FetchResponse>>,
    calls: Rc<RefCell<Vec<FetchRequest>>>,
}

impl StaticFetcher {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with(mut self, url: &str, response: FetchResponse) -> Self {
        let mut table = (*self.responses).clone();
        table.insert(url.to_string(), response);
        self.responses = Rc::new(table);
        self
    }

    #[must_use]
    pub fn calls(&self) -> Vec<FetchRequest> {
        self.calls.borrow().clone()
    }
}

impl HttpFetcher for StaticFetcher {
    fn fetch(&self, request: FetchRequest) -> Result<FetchResponse, FetchError> {
        self.calls.borrow_mut().push(request.clone());
        self.responses
            .get(&request.url)
            .cloned()
            .ok_or_else(|| FetchError {
                message: format!("no fixture for {}", request.url),
            })
    }
}

/// A D1-shaped adapter over an in-memory SQLite database; clones share one connection.
#[derive(Clone)]
pub struct SqliteDatabase {
    connection: Rc<Connection>,
}

impl SqliteDatabase {
    pub fn open(migration: &str) -> Result<Self, DbError> {
        let connection = Connection::open_in_memory().map_err(db_error)?;
        if !migration.trim().is_empty() {
            connection.execute_batch(migration).map_err(db_error)?;
        }
        Ok(SqliteDatabase {
            connection: Rc::new(connection),
        })
    }

    pub fn execute(&self, sql: &str) -> Result<(), DbError> {
        self.connection.execute_batch(sql).map_err(db_error)
    }
}

impl DownloadDatabase for SqliteDatabase {
    fn batch(&self, statements: Vec<Statement>) -> Result<Vec<Vec<Row>>, DbError> {
        statements.iter().map(|statement| self.run(statement)).collect()
    }
}

impl SqliteDatabase {
    fn run(&self, statement: &Statement) -> Result<Vec<Row>, DbError> {
        let mut prepared = self.connection.prepare(&statement.sql).map_err(db_error)?;
        let params: Vec<SqlValue> = statement.params.iter().map(to_sql).collect();
        let column_count = prepared.column_count();
        if column_count == 0 {
            prepared.execute(params_from_iter(params)).map_err(db_error)?;
            return Ok(Vec::new());
        }
        let names: Vec<String> = prepared
            .column_names()
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        let mut rows = prepared.query(params_from_iter(params)).map_err(db_error)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(db_error)? {
            let mut columns = Vec::with_capacity(column_count);
            for (index, name) in names.iter().enumerate() {
                let value = row.get_ref(index).map_err(db_error)?;
                columns.push((name.clone(), from_sql(value)));
            }
            out.push(Row::new(columns));
        }
        Ok(out)
    }
}

fn to_sql(value: &Value) -> SqlValue {
    match value {
        Value::Text(text) => SqlValue::Text(text.clone()),
        Value::Integer(integer) => SqlValue::Integer(*integer),
        Value::Number(number) => SqlValue::Real(*number),
        Value::Null => SqlValue::Null,
    }
}

fn from_sql(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(integer) => Value::Integer(integer),
        ValueRef::Real(real) => Value::Number(real),
        ValueRef::Text(bytes) => Value::Text(String::from_utf8_lossy(bytes).into_owned()),
        ValueRef::Blob(_) => Value::Null,
    }
}

fn db_error(error: SqlError) -> DbError {
    DbError {
        message: error.to_string(),
    }
}

/// The upstream `harness()`: one bucket, cache, sink, deferred queue and database.
pub struct Harness {
    pub bucket: MemoryReleaseStore,
    pub cache: MemoryEdgeCache,
    pub sink: RecordingSink,
    pub pending: PendingWork,
    pub db: SqliteDatabase,
    pub fetcher: StaticFetcher,
    env: Env,
}

impl Harness {
    pub fn new(migration: &str, fetcher: StaticFetcher) -> Self {
        let bucket = MemoryReleaseStore::new();
        let cache = MemoryEdgeCache::new();
        let sink = RecordingSink::new();
        let pending = PendingWork::new();
        let db = SqliteDatabase::open(migration).expect("in-memory sqlite opens");
        let env = Env {
            releases: Box::new(bucket.clone()),
            db: Box::new(db.clone()),
            downloads: Box::new(sink.clone()),
            account_id: "account".to_string(),
            analytics_dataset: "omo_downloads".to_string(),
            analytics_token: Some("token".to_string()),
        };
        Harness {
            bucket,
            cache,
            sink,
            pending,
            db,
            fetcher,
            env,
        }
    }

    #[must_use]
    pub fn env(&self) -> &Env {
        &self.env
    }

    #[must_use]
    pub fn context(&self) -> RequestContext<'_> {
        RequestContext {
            env: &self.env,
            cache: &self.cache,
            wait_until: &self.pending,
            fetch: &self.fetcher,
        }
    }

    /// The upstream `settle()`: runs the queued `waitUntil` work.
    pub fn settle(&self) {
        self.pending.settle(&self.cache);
    }
}
