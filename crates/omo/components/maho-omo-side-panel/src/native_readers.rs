use crate::{
    commands::{ExecOutput, PanelExec},
    runtime::PanelReaders,
    store::PanelState,
    types::*,
};
use serde_json::Value;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

pub fn load_config(cwd: &str) -> Value {
    Value::Object(
        omo_config_core::loader::loader::load_omo_config(
            &omo_config_core::loader::types::LoadOmoConfigOptions {
                cwd: Some(cwd.into()),
                harness: Some("senpi".into()),
                ..Default::default()
            },
        )
        .config,
    )
}
pub fn task_store(cwd: &str) -> senpi_task::store::TaskRecordStore {
    let config = load_config(cwd);
    let task_state_dir = config
        .pointer("/task/state_dir")
        .and_then(Value::as_str)
        .map(|path| Path::new(cwd).join(path));
    senpi_task::store::TaskRecordStore::new(&senpi_task::store::StateDirConfig {
        project_dir: cwd.into(),
        task_state_dir,
    })
}
pub struct NativeExec(pub Arc<dyn maho_ext_api::ExtensionSessionActions>);
impl PanelExec for NativeExec {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        args: Vec<String>,
        cwd: &'a str,
        timeout_ms: u64,
    ) -> PanelFuture<'a, Result<ExecOutput, String>> {
        Box::pin(async move {
            let output = self
                .0
                .exec(
                    command,
                    &args,
                    Path::new(cwd),
                    maho_ext_api::ExecOptions {
                        timeout_ms: Some(timeout_ms),
                        cwd: Some(cwd.into()),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            Ok(ExecOutput {
                stdout: output.stdout,
                code: output.code,
            })
        })
    }
}
pub struct NativeReaders {
    pub exec: Option<Arc<dyn PanelExec>>,
    goal: Mutex<crate::data::goal::PanelGoalReader>,
    stores: Mutex<std::collections::BTreeMap<String, senpi_task::store::TaskRecordStore>>,
    identities:
        Mutex<std::collections::BTreeMap<String, memory_core::identity::resolve::MemoryIdentity>>,
}
impl NativeReaders {
    pub fn new(exec: Option<Arc<dyn PanelExec>>) -> Self {
        Self {
            exec,
            goal: Mutex::default(),
            stores: Mutex::default(),
            identities: Mutex::default(),
        }
    }
}
pub fn read_json(path: &Path) -> Option<Value> {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(value) => Some(value),
            Err(error) => {
                eprintln!("side panel: {}: {error}", path.display());
                None
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            eprintln!("side panel: {}: {error}", path.display());
            None
        }
    }
}
impl PanelReaders for NativeReaders {
    fn find_git_root(&self, cwd: &str) -> Option<String> {
        crate::git::repo::find_git_root(Path::new(cwd))
            .map(|path| path.to_string_lossy().into_owned())
    }
    fn git_branch(&self, root: &str) -> Option<String> {
        crate::git::repo::read_git_branch(Path::new(root))
    }
    fn git<'a>(&'a self, root: &'a str) -> PanelFuture<'a, Option<PanelGitStatus>> {
        Box::pin(async move {
            crate::git::read::read_git_status(self.exec.as_deref()?, root).await
        })
    }
    fn children<'a>(
        &'a self,
        cwd: &'a str,
        session_id: &'a str,
    ) -> PanelFuture<'a, Result<Vec<crate::store::PanelChild>, String>> {
        Box::pin(async move {
            let store = self
                .stores
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(cwd.into())
                .or_insert_with(|| task_store(cwd))
                .clone();
            let listed = store.list().map_err(|error| error.to_string())?;
            for diagnostic in listed.diagnostics {
                eprintln!("side panel: task record: {diagnostic:?}");
            }
            let mut state = PanelState::default();
            for record in listed.records {
                // PERF-DIVERGENCE(omo side-panel native readers): the earlier port serialized and re-deserialized every task record of every session before filtering by parent; we compare the typed parent_session_id first and skip other sessions' records. Kept records and output are identical.
                if record.parent_session_id != session_id {
                    continue;
                }
                let value = serde_json::to_value(record).map_err(|error| error.to_string())?;
                let record: crate::data::task_records::PanelTaskRecord =
                    serde_json::from_value(value).map_err(|error| error.to_string())?;
                if record.parent_session_id.as_deref() == Some(session_id) {
                    let mut child = crate::data::task_records::panel_child_from_record(&record);
                    let mut card = vec![
                        PanelRow::colored(
                            child.name.as_deref().unwrap_or(&record.task_id),
                            PanelColor::Accent,
                        ),
                        PanelRow::new(format!("task {}", record.task_id)),
                        PanelRow::new(format!("status {}", record.status)),
                    ];
                    if let Some(cause) = &child.parked_reason {
                        card.push(PanelRow::colored(cause, PanelColor::Warning));
                    }
                    if let Some(host) = &child.host {
                        card.push(PanelRow::new(format!("host {host}")));
                    }
                    if let Some(category) = &child.category {
                        card.push(PanelRow::new(format!("category {category}")));
                    }
                    if let Some(turns) = child.turns {
                        card.push(PanelRow::new(format!("turns {turns}")));
                    }
                    if let Some(tokens) = child.tokens {
                        card.push(PanelRow::new(format!("tokens {tokens}")));
                    }
                    if let Some(cost) = child.cost {
                        card.push(PanelRow::new(format!("cost ${cost:.4}")));
                    }
                    child.card = Some(card);
                    state.merge_child(child, 0);
                }
            }
            Ok(state.children)
        })
    }
    fn goal(&self, path: Option<&str>) -> Option<PanelGoal> {
        self.goal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .read(path.map(Path::new))
    }
    fn resolve_memory(&self, cwd: &str) -> Option<String> {
        let config = load_config(cwd);
        if config.pointer("/memory/enabled").and_then(Value::as_bool) == Some(false) {
            return None;
        }
        let identity =
            match memory_core::identity::resolve::resolve_memory_identity_with_process_env(
                config.pointer("/memory/agent").and_then(Value::as_str),
                Path::new(cwd),
            ) {
                Ok(identity) => identity,
                Err(error) => {
                    eprintln!("side panel: {error}");
                    return None;
                }
            };
        let id = identity.id.clone();
        self.identities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.clone(), identity);
        Some(id)
    }
    fn memory<'a>(
        &'a self,
        identity: &'a str,
        session_id: Option<&'a str>,
    ) -> PanelFuture<'a, Option<PanelMemory>> {
        Box::pin(async move {
            let identity = self
                .identities
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(identity)
                .cloned()?;
            Some(read_memory(&identity, session_id))
        })
    }
    fn child_output<'a>(&'a self, cwd: &'a str, id: &'a str) -> PanelFuture<'a, Vec<PanelRow>> {
        Box::pin(async move {
            use senpi_task::tools::output::{
                render::{RenderOptions, render_transcript},
                transcript::reader::default_transcript_reader,
                types::{TranscriptMode, TranscriptReaderInput},
            };
            let store = task_store(cwd);
            match default_transcript_reader(&TranscriptReaderInput {
                task_id: id,
                state_dir: &store.state_dir().to_string_lossy(),
            }) {
                Ok(read) => {
                    let rendered = render_transcript(
                        &read.entries,
                        &RenderOptions {
                            mode: TranscriptMode::Full,
                            tail_lines: 0,
                        },
                    );
                    crate::data::child_output::child_output_rows(
                        &rendered.text,
                        rendered.truncated || read.truncated == Some(true),
                    )
                }
                Err(error) => vec![PanelRow::colored(
                    format!("Transcript unreadable: {error}"),
                    PanelColor::Error,
                )],
            }
        })
    }
}
fn read_memory(
    identity: &memory_core::identity::resolve::MemoryIdentity,
    session: Option<&str>,
) -> PanelMemory {
    let mut detail = Vec::new();
    let queued = match std::fs::read_dir(&identity.paths.facts_queue) {
        Ok(entries) => entries
            .filter_map(|entry| match entry {
                Ok(entry) => Some(entry.file_name().to_string_lossy().into_owned()),
                Err(error) => {
                    eprintln!("side panel: facts queue: {error}");
                    None
                }
            })
            .filter(|name| {
                name.ends_with(".json")
                    && !["claims.json", "consumed.json", "failures.json"].contains(&name.as_str())
            })
            .count(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => {
            eprintln!("side panel: facts queue: {error}");
            0
        }
    };
    detail.push(PanelRow::new(format!("identity {}", identity.id)));
    detail.push(PanelRow::new(format!("facts queued {queued}")));
    if let Some(park) = read_json(&identity.paths.reflection.join("park.json"))
        && (park.get("streak").and_then(Value::as_u64).unwrap_or(0) > 0
            || park.get("parkedAt").is_some())
    {
        detail.push(PanelRow::colored(
            format!(
                "reflection {} · {} failures",
                if park.get("parkedAt").and_then(Value::as_str).is_some() {
                    "parked"
                } else {
                    "retrying"
                },
                park.get("streak").and_then(Value::as_u64).unwrap_or(0)
            ),
            PanelColor::Warning,
        ));
        for key in ["reason", "detail"] {
            if let Some(text) = park
                .get("lastFailure")
                .and_then(|failure| failure.get(key))
                .and_then(Value::as_str)
            {
                detail.push(PanelRow::colored(text, PanelColor::Muted));
            }
        }
        if let Some(parked) = park.get("parkedAt").and_then(Value::as_str) {
            detail.push(PanelRow::new(format!("parked {parked}")));
            if let Ok(probe) = chrono::DateTime::parse_from_rfc3339(
                park.get("lastProbeAt")
                    .and_then(Value::as_str)
                    .unwrap_or(parked),
            ) {
                detail.push(PanelRow::new(format!(
                    "next probe {}",
                    (probe + chrono::Duration::hours(6)).to_rfc3339()
                )));
            }
        }
    }
    if let Some(session) = session {
        let file = format!(
            "{}.json",
            memory_core::recall::sanitize_session_filename(session)
        );
        let ledger = read_json(&identity.paths.recall_ledger.join(&file));
        let surfaced = ledger
            .as_ref()
            .and_then(|value| value.get("surfaced"))
            .and_then(Value::as_object)
            .map_or(0, serde_json::Map::len);
        detail.push(PanelRow::new(format!("recall surfaced {surfaced}")));
        let pending = read_json(&identity.paths.recall_pending.join(&file))
            .and_then(|value| memory_core::recall::parse_pending_file(&value.to_string()));
        let count = pending
            .filter(|pending| {
                pending.session_id == session
                    && chrono::DateTime::parse_from_rfc3339(&pending.written_at)
                        .ok()
                        .is_some_and(|written| {
                            i128::from(crate::native::now())
                                - i128::from(written.timestamp_millis())
                                <= 86_400_000
                        })
            })
            .map_or(0, |pending| pending.nudges.len());
        detail.push(PanelRow::new(format!("recall pending {count}")));
        use base64::Engine;
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(session);
        let wakes = identity
            .paths
            .recall
            .join("sidecars")
            .join(encoded)
            .join("wakes.ndjson");
        match read_wakes(&wakes) {
            Ok(rows) => detail.extend(rows),
            Err(error) => eprintln!("side panel: {}: {error}", wakes.display()),
        }
    }
    PanelMemory {
        identity: identity.id.clone(),
        rows: detail
            .iter()
            .skip(1)
            .cloned()
            .map(|mut row| {
                row.action = Some(crate::links::PanelAction::Memory);
                row
            })
            .collect(),
        detail,
    }
}
fn read_wakes(path: &Path) -> std::io::Result<Vec<PanelRow>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let length = file.metadata()?.len();
    let cap = crate::constants::KIBITZER_WAKES_TAIL_BYTES as u64;
    let partial = length > cap;
    file.seek(SeekFrom::Start(length.saturating_sub(cap)))?;
    let mut bytes = Vec::new();
    file.take(cap).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let text = if partial {
        text.split_once('\n').map_or("", |(_, tail)| tail)
    } else {
        &text
    };
    let mut wakes = 0;
    let mut nudged = 0;
    let mut tokens = 0;
    let mut last = None;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        match serde_json::from_str::<Value>(line) {
            Ok(record) if record.is_object() => {
                wakes += 1;
                nudged += record
                    .get("nudged")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                for key in ["input", "output", "cacheRead", "cacheWrite"] {
                    tokens += record
                        .get("usage")
                        .and_then(|usage| usage.get(key))
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                }
                last = Some(record);
            }
            Ok(_) => {}
            Err(error) => {
                if text.ends_with('\n') || Some(line) != text.lines().last() {
                    eprintln!("side panel: wake record: {error}");
                }
            }
        }
    }
    if wakes == 0 {
        return Ok(Vec::new());
    }
    let mut rows = vec![PanelRow::new(format!(
        "kibitzer {wakes} wakes · {nudged} nudges · {tokens} tokens{}",
        if partial { " (tail)" } else { "" }
    ))];
    if let Some(last) = last {
        rows.push(PanelRow::colored(
            format!(
                "last {} {}",
                last.get("at").and_then(Value::as_str).unwrap_or(""),
                last.get("status").and_then(Value::as_str).unwrap_or("")
            ),
            if last.get("diagnostic").and_then(Value::as_bool) == Some(true) {
                PanelColor::Warning
            } else {
                PanelColor::Muted
            },
        ));
    }
    Ok(rows)
}
