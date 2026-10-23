//! Engine session-history discovery (upstream `session-history-scanner.ts`).

use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};

use super::agent_transcripts::{SourceCandidates, collect_source_candidates, default_agent_homes};
use super::constants::PROJECT_SCAN_CAP;
use super::omo_session_discovery::discover_omo_cli_projects;
use super::project_candidates::{RawProjectEvidence, build_project_candidates};
use super::project_paths::{
    ProjectPathRules, ProjectPathRulesInput, create_project_path_rules, default_home_dir,
};
use super::recent_sessions::{
    RecentSessionsInput, RecentSessionsRun, SourcedGroup, list_recent_sessions,
};
use super::session_history_fs::{SessionHistoryFs, node_session_history_fs};
use super::transcript_discovery::join;
use super::transcript_parser::iso_from_ms;
use super::types::{
    AgentSessionSource, ProjectScanResult, ProjectSource, RecentSessionsQuery, ScanCounts,
    SessionHistoryOptions,
};

/// Where the engine keeps session transcripts: `<agent home>/sessions`
/// (mirrors `resolveAgentSessionsDirectory`).
fn agent_sessions_directory(env: &BTreeMap<String, String>, home_dir: &str) -> String {
    const NAMES: [&str; 4] = [
        "MAHO_CODING_AGENT_DIR",
        "OMO_CODING_AGENT_DIR",
        "SENPI_CODING_AGENT_DIR",
        "PI_CODING_AGENT_DIR",
    ];
    for name in NAMES {
        if let Some(configured) = env.get(name).map(|v| v.trim()).filter(|v| !v.is_empty()) {
            let cwd = std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            return join(
                &super::project_paths::resolve_path(&join(&cwd, configured)),
                "sessions",
            );
        }
    }
    let branded = join(home_dir, ".omo");
    let canonical = join(&branded, "agent");
    let exists = |path: &str| Path::new(path).exists();
    if exists(&join(&canonical, "settings.json")) {
        return join(&canonical, "sessions");
    }
    if exists(&join(&branded, "settings.json")) {
        return join(&branded, "sessions");
    }
    join(&join(home_dir, ".senpi"), "agent/sessions")
}

pub struct SessionHistoryScanner {
    fs: Arc<dyn SessionHistoryFs>,
    rules: Arc<ProjectPathRules>,
    homes: Vec<super::types::AgentHome>,
    omo_roots: Vec<String>,
    imported_projects: Vec<super::types::ImportedProject>,
    now: super::types::NowFn,
    cached_groups: Arc<Mutex<Option<Vec<SourcedGroup>>>>,
}

pub fn create_session_history_scanner(options: SessionHistoryOptions) -> SessionHistoryScanner {
    let fs = options.fs.clone().unwrap_or_else(node_session_history_fs);
    let env = options
        .env
        .clone()
        .unwrap_or_else(|| std::env::vars().collect());
    let now: super::types::NowFn = options
        .now
        .clone()
        .unwrap_or_else(|| Arc::new(|| jiff::Timestamp::now().as_millisecond()));
    let rules = Arc::new(create_project_path_rules(ProjectPathRulesInput {
        home_dir: options
            .home_dir
            .clone()
            .or_else(|| Some(default_home_dir())),
        tmp_dir: options.tmp_dir.clone(),
        excluded_directories: options.excluded_directories.clone(),
        managed_worktree_directories: options.managed_worktree_directories.clone(),
        platform: options.platform.clone(),
        fs: Arc::clone(&fs),
    }));
    let homes = options
        .homes
        .clone()
        .unwrap_or_else(|| default_agent_homes(&env, &rules.home_dir));
    let omo_roots = options
        .omo_sessions_roots
        .clone()
        .unwrap_or_else(|| vec![agent_sessions_directory(&env, &rules.home_dir)]);
    SessionHistoryScanner {
        fs,
        rules,
        homes,
        omo_roots,
        imported_projects: options.imported_projects.clone().unwrap_or_default(),
        now,
        cached_groups: Arc::new(Mutex::new(None)),
    }
}

fn collect_agent_groups(
    fs: &dyn SessionHistoryFs,
    rules: &ProjectPathRules,
    homes: &[super::types::AgentHome],
    cache: &Mutex<Option<Vec<SourcedGroup>>>,
) -> (Vec<SourcedGroup>, Vec<SourceCandidates>) {
    let per_source: Vec<SourceCandidates> = AgentSessionSource::ALL
        .iter()
        .map(|source| collect_source_candidates(fs, rules, homes, *source))
        .collect();
    let groups: Vec<SourcedGroup> = per_source
        .iter()
        .flat_map(|result| {
            result.groups.iter().map(|group| SourcedGroup {
                group: group.clone(),
                source: result.source,
            })
        })
        .collect();
    *cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(groups.clone());
    (groups, per_source)
}

impl SessionHistoryScanner {
    /// Every directory the agent homes and omo CLI sessions ran in, newest-first. Read-only, best-effort.
    pub fn scan_projects(&self) -> ProjectScanResult {
        let (groups, per_source) = collect_agent_groups(
            self.fs.as_ref(),
            self.rules.as_ref(),
            &self.homes,
            &self.cached_groups,
        );
        let omo = discover_omo_cli_projects(self.fs.as_ref(), &self.omo_roots);
        let mut raw: Vec<RawProjectEvidence> = groups
            .iter()
            .map(|sourced| RawProjectEvidence {
                cwd: sourced.group.cwd.clone(),
                source: ProjectSource::Agent(sourced.source),
                thread_count: sourced.group.transcripts.len(),
                last_active_at_ms: Some(sourced.group.last_active_at_ms),
            })
            .collect();
        raw.extend(omo.projects.iter().map(|project| RawProjectEvidence {
            cwd: project.cwd.clone(),
            source: ProjectSource::Omo,
            thread_count: 0,
            last_active_at_ms: project.last_active_at_ms,
        }));
        let candidates = build_project_candidates(
            self.fs.as_ref(),
            self.rules.as_ref(),
            &raw,
            &self.imported_projects,
        );
        let truncated = omo.truncated || per_source.iter().any(|result| result.truncated);
        ProjectScanResult {
            candidates,
            scanned_at: iso_from_ms((self.now)()),
            truncated,
            counts: ScanCounts {
                found: omo.found + per_source.iter().map(|r| r.found).sum::<usize>(),
                read: omo.read + per_source.iter().map(|r| r.read).sum::<usize>(),
                cap: PROJECT_SCAN_CAP,
            },
        }
    }

    pub fn recent_sessions(&self, query: RecentSessionsQuery) -> RecentSessionsRun {
        let fs = Arc::clone(&self.fs);
        let rules = Arc::clone(&self.rules);
        let homes = self.homes.clone();
        let cache = Arc::clone(&self.cached_groups);
        let loader_fs = Arc::clone(&self.fs);
        let loader_rules = Arc::clone(&self.rules);
        list_recent_sessions(RecentSessionsInput {
            fs,
            rules,
            query,
            now: Arc::clone(&self.now),
            groups: Box::new(move || {
                let cached = cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                match cached {
                    Some(groups) => groups,
                    None => {
                        collect_agent_groups(
                            loader_fs.as_ref(),
                            loader_rules.as_ref(),
                            &homes,
                            &cache,
                        )
                        .0
                    }
                }
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_projects_from_agent_homes_and_reuses_groups_for_sessions()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project)?;
        let home = dir.path().join("claude");
        let session_dir = home.join("projects/p");
        std::fs::create_dir_all(&session_dir)?;
        let cwd = project.to_string_lossy().into_owned();
        std::fs::write(
            session_dir.join("s.jsonl"),
            format!(
                "{}\n",
                serde_json::json!({"type":"user","cwd":cwd,"sessionId":"S","message":{"role":"user","content":"hi"}})
            ),
        )?;
        let scanner = create_session_history_scanner(SessionHistoryOptions {
            homes: Some(vec![super::super::types::AgentHome {
                source: AgentSessionSource::ClaudeAgent,
                home_path: home.to_string_lossy().into_owned(),
            }]),
            omo_sessions_roots: Some(vec![]),
            home_dir: Some("/nonexistent-home".into()),
            tmp_dir: Some("/nonexistent-tmp".into()),
            excluded_directories: Some(vec![]),
            env: Some(BTreeMap::new()),
            ..Default::default()
        });
        let result = scanner.scan_projects();
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].thread_count, 1);
        assert_eq!(result.counts.cap, PROJECT_SCAN_CAP);
        let outcomes: Vec<&str> = scanner
            .recent_sessions(RecentSessionsQuery::default())
            .map(|o| o.tag())
            .collect::<Vec<_>>();
        assert_eq!(outcomes, vec!["Importable"]);
        Ok(())
    }
}
