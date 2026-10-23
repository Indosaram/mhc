//! The onboarding scan stream (upstream `scan-session-history.ts`).

use std::{sync::Arc, time::Instant};

use serde_json::{Value, json};

use super::digest_store::SessionDigestStore;
use super::session_digest::digest_byte_length;
use super::session_history_scanner::create_session_history_scanner;
use super::types::{
    AgentSessionSource, ProjectCandidate, RecentSessionOutcome, RecentSessionsQuery, ScanCounts,
    SessionHistoryOptions,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionHistoryScanSource {
    Projects,
    Sessions,
}

impl SessionHistoryScanSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Projects => "projects",
            Self::Sessions => "sessions",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionScanItem {
    pub session_key: String,
    pub source: AgentSessionSource,
    pub provider_session_id: String,
    pub cwd: String,
    pub title: String,
    pub last_active_at: String,
    pub digest_bytes: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ScanItem {
    Project(Box<ProjectCandidate>),
    Session(SessionScanItem),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanEventKind {
    SourceStarted,
    Item,
    SourceDone,
    SourceFailed,
}

impl ScanEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SourceStarted => "source_started",
            Self::Item => "item",
            Self::SourceDone => "source_done",
            Self::SourceFailed => "source_failed",
        }
    }
}

/// One event of the onboarding scan stream, for the `projects` and `sessions` sources.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionHistoryScanEvent {
    pub scan_id: String,
    pub kind: ScanEventKind,
    pub source: SessionHistoryScanSource,
    pub item: Option<ScanItem>,
    pub counts: Option<ScanCounts>,
    pub error: Option<String>,
    pub elapsed_ms: u64,
}

impl SessionHistoryScanEvent {
    pub fn to_json(&self) -> Value {
        let mut out = serde_json::Map::new();
        out.insert("scanId".into(), json!(self.scan_id));
        out.insert("kind".into(), json!(self.kind.as_str()));
        out.insert("source".into(), json!(self.source.as_str()));
        match &self.item {
            Some(ScanItem::Project(project)) => {
                out.insert("item".into(), project.to_json());
            }
            Some(ScanItem::Session(item)) => {
                out.insert(
                    "item".into(),
                    json!({"sessionKey": item.session_key, "source": item.source.as_str(),
                           "providerSessionId": item.provider_session_id, "cwd": item.cwd,
                           "title": item.title, "lastActiveAt": item.last_active_at,
                           "digestBytes": item.digest_bytes}),
                );
            }
            None => {}
        }
        if let Some(counts) = self.counts {
            out.insert("counts".into(), counts.to_json());
        }
        if let Some(error) = &self.error {
            out.insert("error".into(), json!(error));
        }
        out.insert("elapsedMs".into(), json!(self.elapsed_ms));
        Value::Object(out)
    }
}

pub struct ScanSessionHistoryOptions {
    pub options: SessionHistoryOptions,
    pub query: RecentSessionsQuery,
    pub scan_id: String,
    pub digest_store: Arc<dyn SessionDigestStore>,
}

/// Programmatic, read-only discovery: no model call and no network I/O. Projects are emitted first, then the
/// recent sessions, whose digests are written to the store under `scan_id` before their item is emitted.
/// A failing source ends with `source_failed` and never hides the other source. `emit` receives each event.
pub fn scan_session_history(
    options: ScanSessionHistoryOptions,
    emit: &mut dyn FnMut(SessionHistoryScanEvent),
) {
    let started_at = Instant::now();
    let elapsed_ms = || u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    let ScanSessionHistoryOptions {
        options: scan_options,
        query,
        scan_id,
        digest_store,
    } = options;
    let scanner = create_session_history_scanner(scan_options);
    let event = |kind, source, item, counts, error, elapsed_ms| SessionHistoryScanEvent {
        scan_id: scan_id.clone(),
        kind,
        source,
        item,
        counts,
        error,
        elapsed_ms,
    };
    use ScanEventKind::{Item, SourceDone, SourceFailed, SourceStarted};
    use SessionHistoryScanSource::{Projects, Sessions};

    emit(event(
        SourceStarted,
        Projects,
        None,
        None,
        None,
        elapsed_ms(),
    ));
    let result = scanner.scan_projects();
    for candidate in result.candidates {
        emit(event(
            Item,
            Projects,
            Some(ScanItem::Project(Box::new(candidate))),
            None,
            None,
            elapsed_ms(),
        ));
    }
    emit(event(
        SourceDone,
        Projects,
        None,
        Some(result.counts),
        None,
        elapsed_ms(),
    ));

    emit(event(
        SourceStarted,
        Sessions,
        None,
        None,
        None,
        elapsed_ms(),
    ));
    let session_query = RecentSessionsQuery {
        workspace_root: query.workspace_root.clone(),
        recent_window_ms: query.recent_window_ms,
        completed: None,
    };
    let mut run = scanner.recent_sessions(session_query);
    let mut failure: Option<String> = None;
    for outcome in run.by_ref() {
        let RecentSessionOutcome::Importable {
            thread,
            cwd,
            digest,
            ..
        } = outcome
        else {
            continue;
        };
        if let Err(error) = digest_store.put(&scan_id, &digest) {
            failure = Some(error.to_string());
            break;
        }
        let item = SessionScanItem {
            session_key: digest.session_key.clone(),
            source: thread.source,
            provider_session_id: thread.provider_session_id.clone(),
            cwd,
            title: thread.title.clone(),
            last_active_at: thread.updated_at.clone(),
            digest_bytes: digest_byte_length(&digest),
        };
        emit(event(
            Item,
            Sessions,
            Some(ScanItem::Session(item)),
            None,
            None,
            elapsed_ms(),
        ));
    }
    match failure {
        Some(error) => emit(event(
            SourceFailed,
            Sessions,
            None,
            None,
            Some(error),
            elapsed_ms(),
        )),
        None => emit(event(
            SourceDone,
            Sessions,
            None,
            Some(run.counts()),
            None,
            elapsed_ms(),
        )),
    }
}

/// Convenience: run the scan and collect every event.
pub fn collect_scan_session_history(
    options: ScanSessionHistoryOptions,
) -> Vec<SessionHistoryScanEvent> {
    let mut events = Vec::new();
    scan_session_history(options, &mut |event| events.push(event));
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_history::digest_store::create_memory_session_digest_store;
    use crate::session_history::types::AgentHome;
    use std::{collections::BTreeMap, sync::Arc};

    #[test]
    fn emits_projects_then_sessions_with_digest_stored_before_item()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("proj");
        let home = dir.path().join("claude/projects/p");
        std::fs::create_dir_all(&project)?;
        std::fs::create_dir_all(&home)?;
        let cwd = project.to_string_lossy().into_owned();
        std::fs::write(
            home.join("s.jsonl"),
            format!(
                "{}\n",
                json!({"type":"user","cwd":cwd,"sessionId":"S","message":{"role":"user","content":"hi"}})
            ),
        )?;
        let store = Arc::new(create_memory_session_digest_store());
        let events = collect_scan_session_history(ScanSessionHistoryOptions {
            options: SessionHistoryOptions {
                homes: Some(vec![AgentHome {
                    source: AgentSessionSource::ClaudeAgent,
                    home_path: dir.path().join("claude").to_string_lossy().into_owned(),
                }]),
                omo_sessions_roots: Some(vec![]),
                home_dir: Some("/nonexistent-home".into()),
                tmp_dir: Some("/nonexistent-tmp".into()),
                excluded_directories: Some(vec![]),
                env: Some(BTreeMap::new()),
                ..Default::default()
            },
            query: RecentSessionsQuery::default(),
            scan_id: "scan-1".into(),
            digest_store: store.clone(),
        });
        let kinds: Vec<(&str, &str)> = events
            .iter()
            .map(|e| (e.source.as_str(), e.kind.as_str()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("projects", "source_started"),
                ("projects", "item"),
                ("projects", "source_done"),
                ("sessions", "source_started"),
                ("sessions", "item"),
                ("sessions", "source_done"),
            ]
        );
        assert_eq!(store.list("scan-1")?.len(), 1);
        Ok(())
    }

    #[test]
    fn invalid_scan_id_fails_only_the_sessions_source() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("proj");
        let home = dir.path().join("claude/projects/p");
        std::fs::create_dir_all(&project)?;
        std::fs::create_dir_all(&home)?;
        let cwd = project.to_string_lossy().into_owned();
        std::fs::write(
            home.join("s.jsonl"),
            format!(
                "{}\n",
                json!({"type":"user","cwd":cwd,"sessionId":"S","message":{"role":"user","content":"hi"}})
            ),
        )?;
        let events = collect_scan_session_history(ScanSessionHistoryOptions {
            options: SessionHistoryOptions {
                homes: Some(vec![AgentHome {
                    source: AgentSessionSource::ClaudeAgent,
                    home_path: dir.path().join("claude").to_string_lossy().into_owned(),
                }]),
                omo_sessions_roots: Some(vec![]),
                home_dir: Some("/nonexistent-home".into()),
                tmp_dir: Some("/nonexistent-tmp".into()),
                excluded_directories: Some(vec![]),
                env: Some(BTreeMap::new()),
                ..Default::default()
            },
            query: RecentSessionsQuery::default(),
            scan_id: "../bad".into(),
            digest_store: Arc::new(
                crate::session_history::digest_store::create_session_digest_store(dir.path()),
            ),
        });
        assert!(
            events
                .iter()
                .any(|e| e.source == SessionHistoryScanSource::Projects
                    && e.kind == ScanEventKind::SourceDone)
        );
        let last = events.last().ok_or("no events")?;
        assert_eq!(last.kind, ScanEventKind::SourceFailed);
        assert!(
            last.error
                .as_deref()
                .is_some_and(|e| e.starts_with("invalid scanId: "))
        );
        Ok(())
    }
}
