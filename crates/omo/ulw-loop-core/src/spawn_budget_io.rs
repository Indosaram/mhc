//! Spawn budget I/O helpers.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::Path;
use serde_json::Value;

pub fn read_admission_breaker(session_id: &str) -> Option<String> {
    read_admission_breaker_with_env(session_id, None, |key| env::var(key).ok())
}

pub fn read_admission_breaker_with_env(
    session_id: &str,
    repo_root: Option<&Path>,
    get_env: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let data_dir = get_env("PLUGIN_DATA")?;
    let data_dir = Path::new(&data_dir);
    let data_dir = if data_dir.is_absolute() {
        data_dir.to_path_buf()
    } else if let Some(root) = repo_root {
        root.join(data_dir)
    } else {
        data_dir.to_path_buf()
    };
    let path = data_dir.join("spawn-breaker").join(format!("{session_id}.json"));
    let content = fs::read_to_string(path).ok()?;
    let parsed: Value = serde_json::from_str(&content).ok()?;
    let reason = parsed.get("reason").and_then(|v| v.as_str());
    Some(reason.unwrap_or("capacity limit").to_string())
}

pub fn atomic_write_json<T: serde::Serialize>(target_path: &Path, data: &T) -> std::io::Result<()> {
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let rand_str = uuid::Uuid::new_v4().to_string();
    let tmp = target_path.with_file_name(format!(".tmp-{}", rand_str));
    let json = serde_json::to_string(data).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    fs::write(&tmp, json)?;
    fs::rename(tmp, target_path)
}

pub fn is_non_empty_file(path: &Path) -> bool {
    fs::metadata(path).map(|m| m.is_file() && m.len() > 0).unwrap_or(false)
}

pub fn read_count(counter_path: &Path) -> u32 {
    let Ok(content) = fs::read_to_string(counter_path) else {
        return 0;
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&content) else {
        return 0;
    };
    parsed.get("count").and_then(|v| v.as_u64()).map(|n| n as u32).unwrap_or(0)
}

pub fn read_counts(counter_path: &Path) -> BTreeMap<String, u32> {
    let Ok(content) = fs::read_to_string(counter_path) else {
        return BTreeMap::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&content) else {
        return BTreeMap::new();
    };
    let mut map = BTreeMap::new();
    if let Some(obj) = parsed.as_object() {
        for (k, v) in obj {
            if let Some(n) = v.as_u64() {
                map.insert(k.clone(), n as u32);
            }
        }
    }
    map
}
