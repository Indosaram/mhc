//! JSONC parsing plus config-file detection with a process-wide memo.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsoncError {
    pub message: String,
    pub offset: usize,
    pub length: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JsoncParseResult {
    pub data: Option<Value>,
    pub errors: Vec<JsoncError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsoncSyntaxError {
    pub message: String,
}

impl fmt::Display for JsoncSyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for JsoncSyntaxError {}

pub fn parse_jsonc_safe(content: &str) -> JsoncParseResult {
    let result = omo_config_core::parse_jsonc_safe(content);
    JsoncParseResult {
        data: result.data,
        errors: result
            .errors
            .into_iter()
            .map(|error| JsoncError {
                message: error.message,
                offset: error.offset,
                length: error.length,
            })
            .collect(),
    }
}

pub fn parse_jsonc(content: &str) -> Result<Value, JsoncSyntaxError> {
    let result = parse_jsonc_safe(content);
    match result.data {
        Some(data) if result.errors.is_empty() => Ok(data),
        _ => {
            let messages = result
                .errors
                .iter()
                .map(|error| format!("{} at offset {}", error.message, error.offset))
                .collect::<Vec<_>>()
                .join(", ");
            Err(JsoncSyntaxError {
                message: format!("JSONC parse error: {messages}"),
            })
        }
    }
}

/// Read and parse a JSONC file; `None` on any read or parse failure.
pub fn read_jsonc_file(path: impl AsRef<Path>) -> Option<Value> {
    let content = std::fs::read_to_string(path).ok()?;
    parse_jsonc(&content).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Json,
    Jsonc,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedConfigFile {
    pub format: ConfigFormat,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectPluginConfigResult {
    pub format: ConfigFormat,
    pub path: PathBuf,
    pub legacy_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectPluginConfigFileOptions {
    /// Candidate basenames; the first is canonical. Must be non-empty.
    pub basenames: Vec<String>,
    pub legacy_basenames: Vec<String>,
}

pub fn detect_config_file(base_path: impl AsRef<Path>) -> DetectedConfigFile {
    let base = base_path.as_ref().to_string_lossy().into_owned();
    let jsonc_path = PathBuf::from(format!("{base}.jsonc"));
    let json_path = PathBuf::from(format!("{base}.json"));
    if jsonc_path.exists() {
        return DetectedConfigFile {
            format: ConfigFormat::Jsonc,
            path: jsonc_path,
        };
    }
    if json_path.exists() {
        return DetectedConfigFile {
            format: ConfigFormat::Json,
            path: json_path,
        };
    }
    DetectedConfigFile {
        format: ConfigFormat::None,
        path: json_path,
    }
}

static DETECTION_CACHE: LazyLock<Mutex<HashMap<String, DetectPluginConfigResult>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock_cache() -> std::sync::MutexGuard<'static, HashMap<String, DetectPluginConfigResult>> {
    DETECTION_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn clear_plugin_config_file_detection_cache() {
    lock_cache().clear();
}

pub fn detect_plugin_config_file(
    dir: impl AsRef<Path>,
    options: &DetectPluginConfigFileOptions,
) -> DetectPluginConfigResult {
    let dir = dir.as_ref();
    let cache_key = format!(
        "{}::{}::{}",
        dir.display(),
        options.basenames.join(","),
        options.legacy_basenames.join(",")
    );
    if let Some(cached) = lock_cache().get(&cache_key) {
        return cached.clone();
    }
    let canonical_basename = options.basenames.first().map_or("", String::as_str);
    let canonical = detect_config_file(dir.join(canonical_basename));
    let first_legacy = options
        .legacy_basenames
        .iter()
        .map(|legacy| detect_config_file(dir.join(legacy)))
        .find(|result| result.format != ConfigFormat::None);
    let result = match (canonical.format, first_legacy) {
        (ConfigFormat::Json | ConfigFormat::Jsonc, legacy) => DetectPluginConfigResult {
            format: canonical.format,
            path: canonical.path,
            legacy_path: legacy.map(|found| found.path),
        },
        (ConfigFormat::None, Some(legacy)) => DetectPluginConfigResult {
            format: legacy.format,
            path: legacy.path,
            legacy_path: None,
        },
        (ConfigFormat::None, None) => DetectPluginConfigResult {
            format: ConfigFormat::None,
            path: dir.join(format!("{canonical_basename}.json")),
            legacy_path: None,
        },
    };
    lock_cache().insert(cache_key, result.clone());
    result
}
