//! Bounded native interfaces for the Cloudflare bindings the service consumes.
//!
//! Upstream `env.ts` binds R2 (`RELEASES`), D1 (`DB`), Analytics Engine
//! (`DOWNLOADS`), the edge `cache` and `waitUntil`. None has a local equivalent, so
//! each is a trait: production keeps the remote binding, tests inject a deterministic
//! local adapter. No trait carries an inert or no-op default.

use crate::http::{Body, Headers, Response};

/// One Analytics Engine data point (`writeDataPoint`).
#[derive(Clone, Debug, PartialEq)]
pub struct DataPoint {
    pub blobs: Vec<String>,
    pub doubles: Vec<f64>,
    pub indexes: Vec<String>,
}

/// `R2Object` metadata returned by `head`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectMeta {
    pub key: String,
    pub size: u64,
}

/// `R2ObjectBody` returned by `get`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredObject {
    pub key: String,
    pub size: u64,
    pub http_etag: String,
    pub body: Body,
    pub http_metadata: HttpMetadata,
}

impl StoredObject {
    #[must_use]
    pub fn text(&self) -> String {
        self.body.text()
    }

    /// Mirrors `object.writeHttpMetadata(headers)`: applies each present metadata field to
    /// its header, matching the runtime implementation this TS call resolves to
    /// (`R2Bucket::HeadResult::writeHttpMetadata`). An absent field writes nothing, so the
    /// `Headers.set` calls `objectResponse` makes afterwards win.
    pub fn write_http_metadata(&self, headers: &mut Headers) {
        let metadata = &self.http_metadata;
        for (name, value) in [
            ("Content-Type", &metadata.content_type),
            ("Content-Language", &metadata.content_language),
            ("Content-Disposition", &metadata.content_disposition),
            ("Content-Encoding", &metadata.content_encoding),
            ("Cache-Control", &metadata.cache_control),
            ("Expires", &metadata.cache_expiry),
        ] {
            if let Some(value) = value {
                headers.set(name, value.clone());
            }
        }
    }
}

/// `R2Object.httpMetadata`: the stored HTTP metadata `writeHttpMetadata` applies.
///
/// Bounded to the six fields the R2 binding stores. Upstream `cacheExpiry` is a `Date`
/// rendered by `toUTCString()`; this crate carries no date type, so the host supplies that
/// HTTP-date string.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HttpMetadata {
    pub content_type: Option<String>,
    pub content_language: Option<String>,
    pub content_disposition: Option<String>,
    pub content_encoding: Option<String>,
    pub cache_control: Option<String>,
    pub cache_expiry: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct StoreError {
    pub message: String,
}

/// The `RELEASES: R2Bucket` binding.
pub trait ReleaseStore {
    /// `R2Bucket.head`.
    fn head(&self, key: &str) -> Result<Option<ObjectMeta>, StoreError>;
    /// `R2Bucket.get`.
    fn get(&self, key: &str) -> Result<Option<StoredObject>, StoreError>;
}

/// The `DOWNLOADS: AnalyticsEngineDataset` binding.
pub trait AnalyticsSink {
    /// `AnalyticsEngineDataset.writeDataPoint`.
    fn write_data_point(&self, point: DataPoint);
}

/// An edge-cache lookup key: the canonical GET URL plus the request headers that
/// participate in matching (a `Range` request looks up a distinct entry).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheKey {
    pub url: String,
    pub headers: Headers,
}

impl CacheKey {
    #[must_use]
    pub fn get(url: impl Into<String>) -> Self {
        CacheKey {
            url: url.into(),
            headers: Headers::new(),
        }
    }

    #[must_use]
    pub fn with_headers(mut self, headers: Headers) -> Self {
        self.headers = headers;
        self
    }
}

/// A deferred edge-cache population: the only work the service hands to `waitUntil`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachePopulation {
    pub key: CacheKey,
    pub response: Response,
}

/// The `cache: Cache` binding.
pub trait EdgeCache {
    /// `Cache.match`; `None` is the upstream `undefined`.
    fn match_request(&self, key: &CacheKey) -> Option<Response>;
    /// `Cache.put`.
    fn put(&self, key: &CacheKey, response: Response);
}

/// The `waitUntil` member of `RequestContext`.
pub trait DeferredWork {
    /// Defers an edge-cache population past the response.
    fn wait_until(&self, population: CachePopulation);
}

/// A bound SQL parameter or a result column value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Text(String),
    Integer(i64),
    Number(f64),
    Null,
}

/// A prepared D1 statement with its bound parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct Statement {
    pub sql: String,
    pub params: Vec<Value>,
}

impl Statement {
    #[must_use]
    pub fn new(sql: impl Into<String>) -> Self {
        Statement {
            sql: sql.into(),
            params: Vec::new(),
        }
    }

    #[must_use]
    pub fn bind(mut self, value: Value) -> Self {
        self.params.push(value);
        self
    }

    #[must_use]
    pub fn bind_text(self, value: impl Into<String>) -> Self {
        self.bind(Value::Text(value.into()))
    }

    #[must_use]
    pub fn bind_number(self, value: f64) -> Self {
        self.bind(Value::Number(value))
    }
}

/// One result row, addressed by column name as D1 returns objects.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    columns: Vec<(String, Value)>,
}

impl Row {
    #[must_use]
    pub fn new(columns: Vec<(String, Value)>) -> Self {
        Row { columns }
    }

    #[must_use]
    pub fn get(&self, column: &str) -> Option<&Value> {
        self.columns
            .iter()
            .find(|(name, _)| name == column)
            .map(|(_, value)| value)
    }

    #[must_use]
    pub fn text(&self, column: &str) -> Option<&str> {
        match self.get(column) {
            Some(Value::Text(text)) => Some(text),
            _ => None,
        }
    }

    #[must_use]
    pub fn number(&self, column: &str) -> Option<f64> {
        match self.get(column) {
            Some(Value::Number(number)) => Some(*number),
            Some(Value::Integer(integer)) => Some(*integer as f64),
            _ => None,
        }
    }

    #[must_use]
    pub fn integer(&self, column: &str) -> Option<i64> {
        match self.get(column) {
            Some(Value::Integer(integer)) => Some(*integer),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct DbError {
    pub message: String,
}

/// The `DB: D1Database` binding.
pub trait DownloadDatabase {
    /// `D1Database.batch`: one row set per statement, empty for a non-`SELECT`.
    fn batch(&self, statements: Vec<Statement>) -> Result<Vec<Vec<Row>>, DbError>;
}

/// An outbound HTTP request issued through the `fetch` seam.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchRequest {
    pub url: String,
    pub method: String,
    pub headers: Headers,
    pub body: Option<String>,
}

impl FetchRequest {
    #[must_use]
    pub fn get(url: impl Into<String>) -> Self {
        FetchRequest {
            url: url.into(),
            method: "GET".to_string(),
            headers: Headers::new(),
            body: None,
        }
    }

    #[must_use]
    pub fn post(url: impl Into<String>, body: impl Into<String>) -> Self {
        FetchRequest {
            url: url.into(),
            method: "POST".to_string(),
            headers: Headers::new(),
            body: Some(body.into()),
        }
    }

    #[must_use]
    pub fn with_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.set(name, value);
        self
    }
}

/// An outbound HTTP response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchResponse {
    pub status: u16,
    pub body: String,
}

impl FetchResponse {
    #[must_use]
    pub fn new(status: u16, body: impl Into<String>) -> Self {
        FetchResponse {
            status,
            body: body.into(),
        }
    }

    #[must_use]
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// `Response.json()`.
    pub fn json(&self) -> Result<serde_json::Value, FetchError> {
        serde_json::from_str(&self.body).map_err(|error| FetchError {
            message: error.to_string(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct FetchError {
    pub message: String,
}

/// The outbound `fetch` seam used by the npm dist-tag fallback and the rollup query.
pub trait HttpFetcher {
    fn fetch(&self, request: FetchRequest) -> Result<FetchResponse, FetchError>;
}
