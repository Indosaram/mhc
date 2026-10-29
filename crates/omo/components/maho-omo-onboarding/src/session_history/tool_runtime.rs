//! Body of the `onboarding_session_history` tool (upstream `session-history-tool-runtime.ts`).

use std::{collections::BTreeMap, path::Path, sync::Arc};

use serde_json::{Value, json};

use super::{
    ProjectCandidate, RecentSessionsQuery, ScanCounts, ScanEventKind, ScanItem,
    ScanSessionHistoryOptions, SessionDigest, SessionDigestStore, SessionHistoryOptions,
    SessionHistoryScanSource, create_session_digest_store, scan_session_history,
};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const SHOWN_PROJECTS: usize = 40;
const SHOWN_DIGESTS: usize = 40;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OnboardingSessionHistoryParams {
    pub workspace_root: Option<String>,
    pub recent_days: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceSummary {
    pub counts: Option<ScanCounts>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OnboardingSessionHistoryDetails {
    pub scan_id: String,
    pub digests_file: String,
    pub sources: BTreeMap<&'static str, SourceSummary>,
    pub project_count: usize,
    pub digest_count: usize,
}

impl OnboardingSessionHistoryDetails {
    pub fn to_json(&self) -> Value {
        let sources: serde_json::Map<String, Value> = self
            .sources
            .iter()
            .map(|(name, summary)| {
                let mut out = serde_json::Map::new();
                if let Some(counts) = summary.counts {
                    out.insert("counts".into(), counts.to_json());
                }
                if let Some(error) = &summary.error {
                    out.insert("error".into(), json!(error));
                }
                ((*name).to_owned(), Value::Object(out))
            })
            .collect();
        json!({
            "scanId": self.scan_id,
            "digestsFile": self.digests_file,
            "sources": sources,
            "projectCount": self.project_count,
            "digestCount": self.digest_count,
        })
    }
}

pub struct OnboardingSessionHistoryRun {
    pub params: OnboardingSessionHistoryParams,
    pub state_dir: String,
    pub scan_options: Option<SessionHistoryOptions>,
    pub scan_id: String,
}

pub struct OnboardingSessionHistoryResult {
    pub text: String,
    pub details: OnboardingSessionHistoryDetails,
}

pub fn execute_onboarding_session_history(
    run: OnboardingSessionHistoryRun,
) -> Result<OnboardingSessionHistoryResult, String> {
    let OnboardingSessionHistoryRun {
        params,
        state_dir,
        scan_options,
        scan_id,
    } = run;
    let digest_store: Arc<dyn SessionDigestStore> =
        Arc::new(create_session_digest_store(&state_dir));
    let query = RecentSessionsQuery {
        workspace_root: params.workspace_root.clone(),
        recent_window_ms: params.recent_days.map(|days| days * DAY_MS),
        completed: None,
    };
    let mut projects: Vec<ProjectCandidate> = Vec::new();
    let mut sources: BTreeMap<&'static str, SourceSummary> = BTreeMap::new();
    scan_session_history(
        ScanSessionHistoryOptions {
            options: scan_options.unwrap_or_default(),
            query,
            scan_id: scan_id.clone(),
            digest_store: Arc::clone(&digest_store),
        },
        &mut |event| {
            if event.kind == ScanEventKind::Item
                && event.source == SessionHistoryScanSource::Projects
                && let Some(ScanItem::Project(project)) = event.item
            {
                projects.push(*project);
            }
            if event.kind == ScanEventKind::SourceDone {
                sources.insert(
                    event.source.as_str(),
                    SourceSummary {
                        counts: event.counts,
                        error: None,
                    },
                );
            }
            if event.kind == ScanEventKind::SourceFailed {
                sources.insert(
                    event.source.as_str(),
                    SourceSummary {
                        counts: None,
                        error: Some(event.error.unwrap_or_else(|| "unknown error".to_owned())),
                    },
                );
            }
        },
    );
    let digests = digest_store
        .list(&scan_id)
        .map_err(|error| error.to_string())?;
    let details = OnboardingSessionHistoryDetails {
        digests_file: Path::new(&state_dir)
            .join("onboarding")
            .join("scans")
            .join(&scan_id)
            .join("digests.jsonl")
            .to_string_lossy()
            .into_owned(),
        scan_id,
        sources,
        project_count: projects.len(),
        digest_count: digests.len(),
    };
    let text = format_scan(&details, &projects, &digests);
    Ok(OnboardingSessionHistoryResult { text, details })
}

fn format_source(name: &str, summary: Option<&SourceSummary>) -> String {
    if let Some(error) = summary.and_then(|s| s.error.as_ref()) {
        return format!("{name}: failed ({error})");
    }
    match summary.and_then(|s| s.counts) {
        None => format!("{name}: no result"),
        Some(counts) => format!(
            "{name}: found {}, read {}, cap {}",
            counts.found, counts.read, counts.cap
        ),
    }
}

pub fn format_scan(
    details: &OnboardingSessionHistoryDetails,
    projects: &[ProjectCandidate],
    digests: &[SessionDigest],
) -> String {
    let mut lines = vec![
        format!("scan {}", details.scan_id),
        format_source("projects", details.sources.get("projects")),
        format_source("sessions", details.sources.get("sessions")),
        String::new(),
        format!("projects ({}, newest first):", projects.len()),
    ];
    lines.extend(projects.iter().take(SHOWN_PROJECTS).map(|project| {
        format!(
            "- {} [{}] threads={} last={}",
            project.path,
            project
                .sources
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(","),
            project.thread_count,
            project.last_active_at.as_deref().unwrap_or("unknown"),
        )
    }));
    if projects.len() > SHOWN_PROJECTS {
        lines.push(format!("- ... {} more", projects.len() - SHOWN_PROJECTS));
    }
    lines.push(String::new());
    lines.push(format!(
        "session digests ({}; all in {}):",
        digests.len(),
        details.digests_file
    ));
    lines.extend(
        digests
            .iter()
            .take(SHOWN_DIGESTS)
            .map(SessionDigest::to_json_string),
    );
    if digests.len() > SHOWN_DIGESTS {
        lines.push(format!(
            "... {} more digests in the file above",
            digests.len() - SHOWN_DIGESTS
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_history::types::{AgentHome, AgentSessionSource};

    #[test]
    fn formats_a_scan_and_persists_digests_under_the_state_dir()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("proj");
        let session_dir = dir.path().join("claude/projects/p");
        std::fs::create_dir_all(&project)?;
        std::fs::create_dir_all(&session_dir)?;
        let cwd = project.to_string_lossy().into_owned();
        std::fs::write(
            session_dir.join("s.jsonl"),
            format!(
                "{}\n",
                json!({"type":"user","cwd":cwd,"sessionId":"S","message":{"role":"user","content":"hi"}})
            ),
        )?;
        let state = dir.path().join("state");
        let result = execute_onboarding_session_history(OnboardingSessionHistoryRun {
            params: OnboardingSessionHistoryParams {
                workspace_root: None,
                recent_days: Some(365),
            },
            state_dir: state.to_string_lossy().into_owned(),
            scan_options: Some(SessionHistoryOptions {
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
            }),
            scan_id: "tui-test".into(),
        })?;
        assert_eq!(result.details.project_count, 1);
        assert_eq!(result.details.digest_count, 1);
        assert!(Path::new(&result.details.digests_file).exists());
        assert!(result.text.starts_with("scan tui-test\nprojects: found 1, read 1, cap 15000\nsessions: found 1, read 1, cap 100\n"));
        assert!(result.text.contains("projects (1, newest first):"));
        assert!(result.text.contains("[claudeAgent] threads=1 last="));
        assert!(result.text.contains("session digests (1; all in "));
        Ok(())
    }

    #[test]
    fn empty_homes_report_zero_counts() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let result = execute_onboarding_session_history(OnboardingSessionHistoryRun {
            params: OnboardingSessionHistoryParams::default(),
            state_dir: dir.path().to_string_lossy().into_owned(),
            scan_options: Some(SessionHistoryOptions {
                homes: Some(vec![]),
                omo_sessions_roots: Some(vec![]),
                env: Some(BTreeMap::new()),
                ..Default::default()
            }),
            scan_id: "ok-id".into(),
        })?;
        assert!(result.text.contains("sessions: found 0, read 0, cap 100"));
        Ok(())
    }
}
