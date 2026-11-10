//! Identity-scoped usage accounting for memory read tool calls.
use maho_ext_api::{EventKind, EventResult, ExtensionApi, ExtensionEvent, ToolCallEvent};
use memory_core::locks::{
    AcquireLockOptions, CreateLockRecordOptions, create_lock_record, with_lock,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageEntry {
    count: u64,
    last_read_at: String,
}

fn record_read_calls(
    options: &crate::skills_usage_wiring::SkillsUsageOptions,
    event: &ToolCallEvent,
) -> Result<(), String> {
    let name = event.tool_name.to_lowercase().replace('-', "_");
    if name != "read"
        && !["_read", ":read", "/read"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
    {
        return Ok(());
    }
    let Some(context) = (options.resolve_context)(event) else {
        return Ok(());
    };
    let repo = match context.identity_paths.repo.canonicalize() {
        Ok(repo) => repo,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    let cwd = (options.resolve_cwd)();
    let mut paths = BTreeSet::new();
    for raw in ["path", "filePath", "file_path", "target"]
        .iter()
        .filter_map(|key| event.input.get(key).and_then(serde_json::Value::as_str))
        .chain(
            event
                .input
                .get("paths")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str),
        )
    {
        let raw = Path::new(raw);
        let absolute = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            cwd.join(raw)
        };
        let Ok(absolute) = absolute.canonicalize() else {
            continue;
        };
        let Ok(relative) = absolute.strip_prefix(&repo) else {
            continue;
        };
        if !absolute.is_file() || relative.components().any(|part| part.as_os_str() == ".git") {
            continue;
        }
        paths.insert(relative.to_string_lossy().replace('\\', "/"));
    }
    if paths.is_empty() {
        return Ok(());
    }
    let runtime = &context.identity_paths.runtime;
    let ledger = runtime.join("memory-usage.json");
    let lock = context.identity_paths.locks.join("memory-usage.lock");
    let record = create_lock_record("memory-usage", CreateLockRecordOptions::default())
        .map_err(|error| error.to_string())?;
    with_lock(
        &lock,
        &record,
        &AcquireLockOptions {
            wait_timeout_ms: Some(2000),
            ..Default::default()
        },
        || -> std::io::Result<()> {
            let mut entries: BTreeMap<String, UsageEntry> = match std::fs::read(&ledger) {
                Ok(bytes) => serde_json::from_slice(&bytes)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
                Err(error) => return Err(error),
            };
            let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            for path in paths {
                let entry = entries.entry(path).or_insert(UsageEntry {
                    count: 0,
                    last_read_at: String::new(),
                });
                entry.count = entry.count.saturating_add(1);
                entry.last_read_at = timestamp.clone();
            }
            std::fs::create_dir_all(runtime)?;
            let temporary = runtime.join("memory-usage.json.tmp");
            std::fs::write(&temporary, serde_json::to_vec_pretty(&entries)?)?;
            std::fs::rename(temporary, ledger)
        },
    )
    .map_err(|error| error.to_string())
}

pub fn register_memory_usage(
    api: &mut ExtensionApi,
    options: crate::skills_usage_wiring::SkillsUsageOptions,
    warn: crate::guard::GuardWarning,
) {
    api.on(
        EventKind::ToolCall,
        Arc::new(move |event, _| {
            if let ExtensionEvent::ToolCall(event) = event
                && let Err(error) = record_read_calls(&options, event)
            {
                warn(&format!("memory usage ledger: {error}"));
            }
            Box::pin(async { Ok(EventResult::None) })
        }),
    );
}
