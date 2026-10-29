//! Port of `src/datapoint.ts`: analytics events and request classification.

use crate::bindings::{AnalyticsSink, DataPoint};
use crate::http::Request;

/// The kind of request a download is recorded under (rollup `blob1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestKind {
    Binary,
    Checksums,
    Engine,
    Script,
    Channel,
}

impl RequestKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            RequestKind::Binary => "binary",
            RequestKind::Checksums => "checksums",
            RequestKind::Engine => "engine",
            RequestKind::Script => "script",
            RequestKind::Channel => "channel",
        }
    }
}

/// Where a served download came from (rollup `blob2`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServedFrom {
    R2,
    Cache,
    Github,
    Npm,
    Worker,
}

impl ServedFrom {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ServedFrom::R2 => "r2",
            ServedFrom::Cache => "cache",
            ServedFrom::Github => "github",
            ServedFrom::Npm => "npm",
            ServedFrom::Worker => "worker",
        }
    }
}

/// One recorded download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadEvent {
    pub kind: RequestKind,
    pub source: ServedFrom,
    pub version: String,
    pub asset: String,
    pub country: String,
    pub qa: bool,
}

/// `recordDownload`: column order is the rollup's contract - `blob1` kind,
/// `blob2` source, `blob3` version, `blob4` asset, `blob5` country, `blob6` `"qa"`
/// for tagged QA installs - with a unit `double1` and the kind as the index.
pub fn record_download(sink: &dyn AnalyticsSink, event: &DownloadEvent) {
    sink.write_data_point(DataPoint {
        blobs: vec![
            event.kind.as_str().to_string(),
            event.source.as_str().to_string(),
            event.version.clone(),
            event.asset.clone(),
            event.country.clone(),
            if event.qa { "qa".to_string() } else { String::new() },
        ],
        doubles: vec![1.0],
        indexes: vec![event.kind.as_str().to_string()],
    });
}

/// `isQaInstall`: the `omo-install-qa` word-boundary token in the User-Agent.
#[must_use]
pub fn is_qa_install(request: &Request) -> bool {
    request
        .header("User-Agent")
        .is_some_and(|agent| contains_word(agent, "omo-install-qa"))
}

/// `requestCountry`: `request.cf.country`, or `"XX"` when the host has no edge country.
#[must_use]
pub fn request_country(request: &Request) -> String {
    request
        .cf_country
        .clone()
        .unwrap_or_else(|| "XX".to_string())
}

fn contains_word(haystack: &str, word: &str) -> bool {
    let bytes = haystack.as_bytes();
    let mut from = 0;
    while let Some(index) = haystack[from..].find(word) {
        let start = from + index;
        let end = start + word.len();
        let before = start == 0 || !is_word_byte(bytes[start - 1]);
        let after = end == bytes.len() || !is_word_byte(bytes[end]);
        if before && after {
            return true;
        }
        from = start + 1;
    }
    false
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}
