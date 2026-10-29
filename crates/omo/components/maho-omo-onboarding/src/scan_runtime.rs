//! Lazy scan runtime (upstream `scan-runtime.ts`): the onboarding scan stream, its retained outcome, the apply
//! of a retained scan, the TUI history summary, and the scan's context and inventory text.
//!
//! `omo setup`'s own detection and writers are not ported to Rust; they enter through [`SetupBackend`] and
//! [`SetupPlans`]. Without a backend the scan reports only session history.

use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use crate::scan_contract::{
    OnboardingApplyResponse, OnboardingApplyStage, OnboardingScanAll, OnboardingScanCounts,
    OnboardingScanEvent, OnboardingScanEventKind, OnboardingScanEventSource, OnboardingScanItem,
    OnboardingScanSource,
};
use crate::session_history::{
    MAX_TRANSCRIPTS_PER_SOURCE, MemorySessionDigestStore, ScanEventKind, ScanItem,
    ScanSessionHistoryOptions, SessionDigest, SessionDigestStore, SessionHistoryOptions,
    create_memory_session_digest_store, default_agent_homes,
    newest_sessions::{NewestSessionDigestsInput, newest_session_digests},
    node_session_history_fs,
    project_paths::{ProjectPathRulesInput, create_project_path_rules},
    scan_session_history,
    types::RecentSessionsQuery,
};

/// Writes the scan's auth additions for a provider.
pub type WriteAuth<'a> = &'a mut dyn FnMut(&str, &[AuthAddition]) -> Result<(), String>;

pub use crate::claim_owner::{claim_onboarding_owned, read_onboarding_state};

/// The newest sessions the TUI onboarding summarizes, and the most transcripts it opens.
pub const ONBOARDING_HISTORY_MAX_SESSIONS: usize = 12;
/// Wall-clock budget for that background summary; it aborts every pending read and reports what it had.
pub const ONBOARDING_HISTORY_BUDGET_MS: u64 = 10_000;
/// Only sessions this recent describe what the user is working on now.
pub const ONBOARDING_HISTORY_WINDOW_MS: i64 = 14 * 24 * 60 * 60 * 1000;
const HISTORY_CLIP: usize = 160;
const ROWS_PER_SOURCE: usize = 40;

/// One source's result of `omo setup`'s detection stream.
#[derive(Clone, Debug)]
pub struct SetupStageResult {
    pub source: OnboardingScanSource,
    pub items: Vec<OnboardingScanItem>,
    pub error: Option<String>,
    pub disabled: bool,
}

/// A target changed since the scan read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupStale {
    pub paths: Vec<String>,
    pub applied: Vec<OnboardingApplyStage>,
}

#[derive(Debug)]
pub enum SetupApplyError {
    Stale(SetupStale),
    Failed(String),
}

/// A credential addition for the engine's store: provider and key.
#[derive(Clone, Debug)]
pub struct AuthAddition {
    pub provider: String,
    pub key: String,
}

/// What an apply needs from a scan: the stage plans `omo setup` computed from the files it read.
pub trait SetupPlans: Send + Sync {
    /// Targets of `stages` whose content changed since the scan; `written` are files an earlier stage of
    /// this apply wrote, which are not a change since the scan.
    fn stale_targets(
        &self,
        stages: &[OnboardingApplyStage],
        written: &HashSet<String>,
    ) -> Vec<String>;
    fn auth_writes_pending(&self, stages: &[OnboardingApplyStage]) -> bool;
    /// The credential file the plans would write.
    fn auth_target(&self) -> Option<String>;
    fn apply(
        &self,
        stages: &[OnboardingApplyStage],
        write_auth: Option<WriteAuth<'_>>,
        report: &mut dyn FnMut(&str),
    ) -> Result<Vec<OnboardingApplyStage>, SetupApplyError>;
}

/// `omo setup`'s detection stream and plan collection.
pub trait SetupBackend: Send + Sync {
    fn scan(
        &self,
        environment: &OnboardingScanEnvironment,
        include_custom_providers: bool,
        on_source_start: &mut dyn FnMut(OnboardingScanSource),
    ) -> Vec<SetupStageResult>;
    /// Plans of the sources that read without error.
    fn plans(&self, results: &[SetupStageResult]) -> Arc<dyn SetupPlans>;
}

/// The engine's own credential store, narrowed to the surface an apply uses.
pub trait OnboardingEngineAuthStore: Send + Sync {
    fn get_storage_path(&self) -> String;
    /// Under the store's lock: adds the entry only when the store holds none for `provider`.
    fn add_api_key_if_absent(&self, provider: &str, key: &str) -> Result<(), String>;
}

/// Where the scan reads and the apply writes; every field defaults to what `omo setup` uses.
#[derive(Clone, Default)]
pub struct OnboardingScanEnvironment {
    pub home: Option<String>,
    pub env: Option<BTreeMap<String, String>>,
    /// Session-history overrides (agent homes, omo session roots, clock); defaults read this machine.
    pub session_history: Option<SessionHistoryOptions>,
    pub setup: Option<Arc<dyn SetupBackend>>,
}

impl OnboardingScanEnvironment {
    fn home_dir(&self) -> String {
        self.home
            .clone()
            .unwrap_or_else(crate::session_history::project_paths::default_home_dir)
    }
    fn env_map(&self) -> BTreeMap<String, String> {
        self.env
            .clone()
            .unwrap_or_else(|| std::env::vars().collect())
    }
}

/// One scan's outcome: the stage plans an apply needs, the per-source rows, and what understanding may read
/// after consent. The digests live only in memory, with the retained scan.
#[derive(Clone)]
pub struct OnboardingScanOutcome {
    pub plans: Option<Arc<dyn SetupPlans>>,
    pub results: Vec<SetupStageResult>,
    pub digests: Vec<SessionDigest>,
    /// The project roots the scan showed the user; understanding reads AGENTS.md only inside these.
    pub project_roots: HashSet<String>,
}

#[derive(Clone, Debug)]
pub struct OnboardingScanOptions {
    pub scan_id: String,
    pub include_custom_providers: bool,
    /// False runs only `omo setup`'s sources: the TUI inventory never reads session history.
    pub sessions: bool,
}

fn history_item(item: &ScanItem) -> OnboardingScanItem {
    match item {
        ScanItem::Project(project) => OnboardingScanItem {
            id: project.path.clone(),
            kind: "project".into(),
            state: if project.already_imported {
                "imported"
            } else {
                "found"
            }
            .into(),
            detail: Some(project.title.clone()),
            project: None,
        },
        ScanItem::Session(session) => OnboardingScanItem {
            id: session.session_key.clone(),
            kind: "session".into(),
            state: "found".into(),
            detail: Some(session.title.clone()),
            project: Some(session.cwd.clone()),
        },
    }
}

fn history_source(
    source: crate::session_history::SessionHistoryScanSource,
) -> OnboardingScanSource {
    match source {
        crate::session_history::SessionHistoryScanSource::Projects => {
            OnboardingScanSource::Projects
        }
        crate::session_history::SessionHistoryScanSource::Sessions => {
            OnboardingScanSource::Sessions
        }
    }
}

/// Runs `omo setup`'s own stream and reports each source as `source_started`, its items, then `source_done`
/// (or `source_failed`), ending with one `done`. Returns the stage plans an apply needs and the source rows;
/// a failed source has no plan, so its stage is never applied.
pub fn run_onboarding_scan(
    environment: &OnboardingScanEnvironment,
    options: &OnboardingScanOptions,
    emit: &mut dyn FnMut(OnboardingScanEvent),
) -> OnboardingScanOutcome {
    let started_at = Instant::now();
    let mut send = |kind: OnboardingScanEventKind,
                    source: OnboardingScanEventSource,
                    item: Option<OnboardingScanItem>,
                    counts: Option<OnboardingScanCounts>,
                    reason: Option<String>| {
        emit(OnboardingScanEvent {
            scan_id: options.scan_id.clone(),
            kind,
            source,
            item,
            counts,
            elapsed_ms: u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            reason,
        });
    };
    let src = OnboardingScanEventSource::Source;
    let mut results: Vec<SetupStageResult> = Vec::new();
    let mut found = 0_usize;
    if let Some(setup) = &environment.setup {
        let mut starts = Vec::new();
        let scanned = setup.scan(
            environment,
            options.include_custom_providers,
            &mut |source| starts.push(source),
        );
        for source in starts {
            send(
                OnboardingScanEventKind::SourceStarted,
                src(source),
                None,
                None,
                None,
            );
        }
        for result in scanned {
            results.push(result.clone());
            if result.disabled {
                continue;
            }
            if let Some(error) = &result.error {
                send(
                    OnboardingScanEventKind::SourceFailed,
                    src(result.source),
                    None,
                    None,
                    Some(error.clone()),
                );
                continue;
            }
            for item in &result.items {
                send(
                    OnboardingScanEventKind::Item,
                    src(result.source),
                    Some(item.clone()),
                    None,
                    None,
                );
            }
            found += result.items.len();
            let counts = OnboardingScanCounts {
                found: result.items.len(),
                read: result.items.len(),
                cap: None,
            };
            send(
                OnboardingScanEventKind::SourceDone,
                src(result.source),
                None,
                Some(counts),
                None,
            );
        }
    }
    let mut read = found;
    // Nothing is persisted before consent: the digests stay in memory, with the retained scan.
    let digest_store: Arc<MemorySessionDigestStore> =
        Arc::new(create_memory_session_digest_store());
    let mut project_roots = HashSet::new();
    if options.sessions {
        let mut history_options = environment.session_history.clone().unwrap_or_default();
        history_options
            .home_dir
            .get_or_insert_with(|| environment.home_dir());
        history_options
            .env
            .get_or_insert_with(|| environment.env_map());
        scan_session_history(
            ScanSessionHistoryOptions {
                options: history_options,
                query: RecentSessionsQuery::default(),
                scan_id: options.scan_id.clone(),
                digest_store: digest_store.clone(),
            },
            &mut |event| {
                let source = src(history_source(event.source));
                match event.kind {
                    ScanEventKind::SourceStarted => send(
                        OnboardingScanEventKind::SourceStarted,
                        source,
                        None,
                        None,
                        None,
                    ),
                    ScanEventKind::SourceFailed => send(
                        OnboardingScanEventKind::SourceFailed,
                        source,
                        None,
                        None,
                        Some(
                            event
                                .error
                                .clone()
                                .unwrap_or_else(|| "unknown error".into()),
                        ),
                    ),
                    ScanEventKind::Item => {
                        if let Some(item) = &event.item {
                            if let ScanItem::Project(project) = item {
                                project_roots.insert(project.path.clone());
                            }
                            send(
                                OnboardingScanEventKind::Item,
                                source,
                                Some(history_item(item)),
                                None,
                                None,
                            );
                        }
                    }
                    ScanEventKind::SourceDone => {
                        if let Some(counts) = event.counts {
                            found += counts.found;
                            read += counts.read;
                            let counts = OnboardingScanCounts {
                                found: counts.found,
                                read: counts.read,
                                cap: Some(MAX_TRANSCRIPTS_PER_SOURCE),
                            };
                            send(
                                OnboardingScanEventKind::SourceDone,
                                source,
                                None,
                                Some(counts),
                                None,
                            );
                        }
                    }
                }
            },
        );
    }
    send(
        OnboardingScanEventKind::Done,
        OnboardingScanEventSource::All(OnboardingScanAll::All),
        None,
        Some(OnboardingScanCounts {
            found,
            read,
            cap: None,
        }),
        None,
    );
    let plans = environment.setup.as_ref().map(|setup| {
        setup.plans(
            &results
                .iter()
                .filter(|r| r.error.is_none())
                .cloned()
                .collect::<Vec<_>>(),
        )
    });
    let digests = digest_store.list(&options.scan_id).unwrap_or_default();
    OnboardingScanOutcome {
        plans,
        results,
        digests,
        project_roots,
    }
}

fn clip(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > HISTORY_CLIP {
        format!("{}...", line.chars().take(HISTORY_CLIP).collect::<String>())
    } else {
        line
    }
}

/// The TUI onboarding's background view of this machine's agent history: the newest
/// `ONBOARDING_HISTORY_MAX_SESSIONS` recent sessions as one context line. Discovery only lists and stats to
/// order transcripts; at most that many are opened. At `budget_ms` every pending filesystem call is aborted
/// and nothing more is read. `None` when nothing was read in time. It never runs on the first prompt's path.
pub fn onboarding_history_summary(
    environment: &OnboardingScanEnvironment,
    budget_ms: u64,
) -> Option<String> {
    let options = environment.session_history.clone().unwrap_or_default();
    let home = options
        .home_dir
        .clone()
        .unwrap_or_else(|| environment.home_dir());
    let env = options.env.clone().unwrap_or_else(|| environment.env_map());
    let fs = options.fs.clone().unwrap_or_else(node_session_history_fs);
    let homes = options
        .homes
        .clone()
        .unwrap_or_else(|| default_agent_homes(&env, &home));
    let now_ms = options
        .now
        .as_ref()
        .map_or_else(|| jiff::Timestamp::now().as_millisecond(), |now| now());
    let signal = Arc::new(AtomicBool::new(false));
    // The watchdog aborts at the budget; `done` releases it as soon as the scan returns.
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let watchdog_signal = Arc::clone(&signal);
    let watchdog = std::thread::spawn(move || {
        if done_rx
            .recv_timeout(Duration::from_millis(budget_ms))
            .is_err()
        {
            watchdog_signal.store(true, Ordering::SeqCst);
        }
    });
    let rules_options = options.clone();
    let rules_home = home.clone();
    let digests = newest_session_digests(NewestSessionDigestsInput {
        fs,
        create_rules: Box::new(move |fs| {
            Arc::new(create_project_path_rules(ProjectPathRulesInput {
                home_dir: Some(rules_home.clone()),
                tmp_dir: rules_options.tmp_dir.clone(),
                excluded_directories: rules_options.excluded_directories.clone(),
                managed_worktree_directories: rules_options.managed_worktree_directories.clone(),
                platform: rules_options.platform.clone(),
                fs,
            }))
        }),
        homes,
        now_ms,
        recent_window_ms: ONBOARDING_HISTORY_WINDOW_MS,
        limit: ONBOARDING_HISTORY_MAX_SESSIONS,
        signal,
    });
    let _ = done_tx.send(());
    let _ = watchdog.join();
    if digests.is_empty() {
        return None;
    }
    let lines: Vec<String> = digests
        .iter()
        .map(|digest| {
            let date: String = digest.last_active_at.chars().take(10).collect();
            let request = if digest.first_request.is_empty() {
                &digest.title
            } else {
                &digest.first_request
            };
            let outcome = if digest.outcome.is_empty() {
                String::new()
            } else {
                format!(" -> {}", clip(&digest.outcome))
            };
            format!(
                "{date} {} {}: {}{outcome}",
                digest.source.as_str(),
                digest.cwd,
                clip(request)
            )
        })
        .collect();
    Some(format!(
        "Recent agent sessions on this machine (newest {}, from a background scan; onboarding_session_history has more): {}",
        digests.len(),
        lines.join("; ")
    ))
}

/// How an apply's credential additions and notice lines travel.
#[derive(Default)]
pub struct ApplyOnboardingScanOptions<'a> {
    pub write_auth: Option<WriteAuth<'a>>,
    pub report: Option<&'a mut dyn FnMut(&str)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum OnboardingApplyOutcome {
    Applied {
        applied: Vec<OnboardingApplyStage>,
        notices: Vec<String>,
    },
    Stale {
        reason: String,
        paths: Vec<String>,
        applied: Option<Vec<OnboardingApplyStage>>,
    },
}

/// Applies the requested stages of a finished scan with the same writers `omo setup` uses. Every requested
/// stage's targets are re-read first and compared by content hash against what the scan read: anything that
/// changed since the scan answers stale and nothing is written, and a target that changes mid-apply refuses
/// that stage. Files an earlier stage of this apply wrote do not make later stages stale.
pub fn apply_onboarding_scan(
    plans: &dyn SetupPlans,
    stages: &[OnboardingApplyStage],
    options: ApplyOnboardingScanOptions<'_>,
) -> Result<OnboardingApplyOutcome, String> {
    let stale = plans.stale_targets(stages, &HashSet::new());
    if !stale.is_empty() {
        return Ok(OnboardingApplyOutcome::Stale {
            reason: format!("targets changed since the scan: {}", stale.join(", ")),
            paths: stale,
            applied: None,
        });
    }
    let mut notices: Vec<String> = Vec::new();
    let mut sink = options.report;
    let mut report = |line: &str| {
        notices.push(line.to_owned());
        if let Some(sink) = sink.as_mut() {
            sink(line);
        }
    };
    match plans.apply(stages, options.write_auth, &mut report) {
        Ok(applied) => Ok(OnboardingApplyOutcome::Applied { applied, notices }),
        Err(SetupApplyError::Stale(stale)) => Ok(OnboardingApplyOutcome::Stale {
            reason: format!("targets changed since the scan: {}", stale.paths.join(", ")),
            paths: stale.paths,
            applied: (!stale.applied.is_empty()).then_some(stale.applied),
        }),
        Err(SetupApplyError::Failed(message)) => Err(message),
    }
}

fn same_path(left: &str, right: &str) -> bool {
    let norm = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| Path::new(p).to_path_buf());
    norm(left) == norm(right)
}

/// `omo.onboarding.apply` once the shell has validated the request and the retained scan: staleness, the
/// credential-store checks and the writers.
pub fn apply_retained_scan(
    scan_id: &str,
    plans: &dyn SetupPlans,
    requested: &[OnboardingApplyStage],
    auth_store: &dyn Fn() -> Option<Arc<dyn OnboardingEngineAuthStore>>,
) -> OnboardingApplyResponse {
    // Staleness is answered first: a target that changed since the scan means the plans no longer describe
    // the machine, so no store is consulted and nothing is written.
    let stale = plans.stale_targets(requested, &HashSet::new());
    if !stale.is_empty() {
        return OnboardingApplyResponse::stale(
            format!("targets changed since the scan: {}", stale.join(", ")),
            stale,
            None,
        );
    }
    if plans.auth_writes_pending(requested) {
        let Some(store) = auth_store() else {
            return OnboardingApplyResponse::unavailable(
                "The engine's credential store is not available, so credentials cannot be imported here; run omo setup --yes instead.",
            );
        };
        let path = store.get_storage_path();
        if plans
            .auth_target()
            .is_none_or(|target| !same_path(&path, &target))
        {
            return OnboardingApplyResponse::unavailable(format!(
                "The engine's credential store ({path}) is not the planned target; run omo setup --yes instead."
            ));
        }
    }
    let store = auth_store();
    let mut write_auth = |target: &str, additions: &[AuthAddition]| -> Result<(), String> {
        let Some(store) = &store else {
            return Err("the engine's credential store is not available".into());
        };
        if !same_path(&store.get_storage_path(), target) {
            return Err(format!(
                "the engine's credential store is not the planned target {target}"
            ));
        }
        // An entry the store already holds (a login or refresh that landed after the scan) is kept as is.
        additions
            .iter()
            .try_for_each(|item| store.add_api_key_if_absent(&item.provider, &item.key))
    };
    let write_auth_ref: Option<WriteAuth<'_>> =
        if store.is_some() {
            Some(&mut write_auth)
        } else {
            None
        };
    let options = ApplyOnboardingScanOptions {
        write_auth: write_auth_ref,
        report: None,
    };
    match apply_onboarding_scan(plans, requested, options) {
        Ok(OnboardingApplyOutcome::Applied { applied, notices }) => {
            OnboardingApplyResponse::Applied {
                scan_id: scan_id.to_owned(),
                applied,
                notices: (!notices.is_empty()).then_some(notices),
            }
        }
        Ok(OnboardingApplyOutcome::Stale {
            reason,
            paths,
            applied,
        }) => OnboardingApplyResponse::stale(reason, paths, applied),
        Err(message) => {
            OnboardingApplyResponse::unavailable(format!("The apply failed: {message}"))
        }
    }
}

const SUMMARY_KINDS: [(&str, &[&str]); 4] = [
    ("sign-ins", &["api-key", "oauth"]),
    ("providers", &["provider"]),
    ("MCP servers", &["mcp"]),
    ("skills", &["skill"]),
];

/// The first-prompt context of one scan, computed from its events every time: one row per found item (ids and
/// states, never a key; capped per source with the remainder counted), failed sources, and the value summary.
pub fn onboarding_scan_context(scan_id: &str, events: &[OnboardingScanEvent]) -> String {
    let items: Vec<&OnboardingScanEvent> = events
        .iter()
        .filter(|e| e.kind == OnboardingScanEventKind::Item && e.item.is_some())
        .collect();
    let mut lines = vec![format!(
        "Onboarding scan {scan_id} (the same scan omo setup and the desktop app run; one row per found item):"
    )];
    let mut sources: Vec<&str> = Vec::new();
    for event in &items {
        let name = event.source.as_str();
        if !sources.contains(&name) {
            sources.push(name);
        }
    }
    for source in sources {
        let own: Vec<&OnboardingScanItem> = items
            .iter()
            .filter(|e| e.source.as_str() == source)
            .filter_map(|e| e.item.as_ref())
            .collect();
        for item in own.iter().take(ROWS_PER_SOURCE) {
            let detail = item
                .detail
                .as_ref()
                .map_or_else(String::new, |d| format!(" - {d}"));
            let project = item
                .project
                .as_ref()
                .map_or_else(String::new, |p| format!(" (project {p})"));
            lines.push(format!(
                "- {source} {} [{}: {}]{detail}{project}",
                item.id, item.kind, item.state
            ));
        }
        if own.len() > ROWS_PER_SOURCE {
            lines.push(format!("- {source}: {} more", own.len() - ROWS_PER_SOURCE));
        }
    }
    for failed in events
        .iter()
        .filter(|e| e.kind == OnboardingScanEventKind::SourceFailed)
    {
        lines.push(format!(
            "! {} could not be read: {}",
            failed.source.as_str(),
            failed.reason.as_deref().unwrap_or("unknown error")
        ));
    }
    // A setup-only scan (sessions: false) never read session history, so it states no session count.
    let read_sessions = events
        .iter()
        .any(|e| matches!(e.source.as_str(), "sessions" | "projects"));
    let rows: Vec<&OnboardingScanItem> = items.iter().filter_map(|e| e.item.as_ref()).collect();
    lines.push(format!(
        "Value summary: {}",
        value_summary(&rows, read_sessions)
    ));
    lines.join("\n")
}

fn value_summary(rows: &[&OnboardingScanItem], read_sessions: bool) -> String {
    let count = |kind: &str| rows.iter().filter(|r| r.kind == kind).count();
    let (sessions, projects) = (count("session"), count("project"));
    let mut parts: Vec<String> = Vec::new();
    if read_sessions {
        parts.push(format!(
            "{sessions} recent {} across {projects} {}",
            if sessions == 1 { "session" } else { "sessions" },
            if projects == 1 { "project" } else { "projects" }
        ));
    }
    for (label, kinds) in SUMMARY_KINDS {
        let found: Vec<&&OnboardingScanItem> = rows
            .iter()
            .filter(|r| kinds.contains(&r.kind.as_str()))
            .collect();
        if found.is_empty() {
            continue;
        }
        let carried = found
            .iter()
            .filter(|r| r.state == "import" || r.state == "login")
            .count();
        parts.push(format!("{} {label} ({carried} to carry over)", found.len()));
    }
    if parts.is_empty() {
        "nothing found to carry over.".into()
    } else {
        format!("{}.", parts.join("; "))
    }
}

const FACT_GROUPS: [(&str, &[&str]); 5] = [
    ("sign-ins", &["api-key", "oauth"]),
    ("providers", &["provider"]),
    ("mcp", &["mcp"]),
    ("skills", &["skill"]),
    ("model choices", &["default", "categories", "agents"]),
];

/// The scan's rows as the one-line migration inventory the TUI onboarding injects: ids grouped by category
/// with each row's state, never a key. `None` when the scan found nothing at all.
pub fn migration_facts_from_scan(results: &[SetupStageResult]) -> Option<String> {
    let rows: Vec<&OnboardingScanItem> = results
        .iter()
        .filter(|r| !r.disabled)
        .flat_map(|r| r.items.iter())
        .collect();
    if rows.is_empty() {
        return None;
    }
    let groups: Vec<String> = FACT_GROUPS
        .iter()
        .filter_map(|(label, kinds)| {
            let own: Vec<String> = rows
                .iter()
                .filter(|r| kinds.contains(&r.kind.as_str()))
                .map(|r| {
                    format!(
                        "{} ({}{})",
                        r.id,
                        r.state,
                        r.detail
                            .as_ref()
                            .map_or_else(String::new, |d| format!(" - {d}"))
                    )
                })
                .collect();
            (!own.is_empty()).then(|| format!("{label}: {}", own.join(", ")))
        })
        .collect();
    Some(groups.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, kind: &str, state: &str) -> OnboardingScanItem {
        OnboardingScanItem {
            id: id.into(),
            kind: kind.into(),
            state: state.into(),
            detail: None,
            project: None,
        }
    }

    #[test]
    fn inventory_groups_rows_and_is_none_when_empty() {
        assert_eq!(migration_facts_from_scan(&[]), None);
        let results = vec![SetupStageResult {
            source: OnboardingScanSource::Credentials,
            items: vec![
                item("openai", "api-key", "import"),
                item("fs", "mcp", "found"),
            ],
            error: None,
            disabled: false,
        }];
        assert_eq!(
            migration_facts_from_scan(&results).as_deref(),
            Some("sign-ins: openai (import); mcp: fs (found)")
        );
    }

    #[test]
    fn context_lists_rows_failures_and_summary_without_sessions() {
        let event = |kind, source, item, reason: Option<&str>| OnboardingScanEvent {
            scan_id: "s".into(),
            kind,
            source: OnboardingScanEventSource::Source(source),
            item,
            counts: None,
            elapsed_ms: 0,
            reason: reason.map(str::to_owned),
        };
        let events = vec![
            event(
                OnboardingScanEventKind::Item,
                OnboardingScanSource::Credentials,
                Some(item("openai", "api-key", "import")),
                None,
            ),
            event(
                OnboardingScanEventKind::SourceFailed,
                OnboardingScanSource::Assets,
                None,
                None,
            ),
        ];
        let context = onboarding_scan_context("s", &events);
        assert!(context.contains("- credentials openai [api-key: import]"));
        assert!(context.contains("! assets could not be read: unknown error"));
        assert!(context.ends_with("Value summary: 1 sign-ins (1 to carry over)."));
    }

    #[test]
    fn history_clip_collapses_whitespace_and_truncates() {
        assert_eq!(clip("a \n b"), "a b");
        let long = "x".repeat(200);
        assert_eq!(clip(&long), format!("{}...", "x".repeat(160)));
    }

    struct FakePlans {
        stale: Vec<String>,
    }
    impl SetupPlans for FakePlans {
        fn stale_targets(&self, _: &[OnboardingApplyStage], _: &HashSet<String>) -> Vec<String> {
            self.stale.clone()
        }
        fn auth_writes_pending(&self, _: &[OnboardingApplyStage]) -> bool {
            false
        }
        fn auth_target(&self) -> Option<String> {
            None
        }
        fn apply(
            &self,
            stages: &[OnboardingApplyStage],
            _: Option<WriteAuth<'_>>,
            report: &mut dyn FnMut(&str),
        ) -> Result<Vec<OnboardingApplyStage>, SetupApplyError> {
            report("wrote");
            Ok(stages.to_vec())
        }
    }

    #[test]
    fn stale_targets_refuse_the_apply_and_fresh_ones_report_notices() {
        let stale = apply_retained_scan(
            "s",
            &FakePlans {
                stale: vec!["/a".into()],
            },
            &[OnboardingApplyStage::Assets],
            &|| None,
        );
        assert_eq!(
            stale,
            OnboardingApplyResponse::stale(
                "targets changed since the scan: /a",
                vec!["/a".into()],
                None
            )
        );
        let fresh = apply_retained_scan(
            "s",
            &FakePlans { stale: vec![] },
            &[OnboardingApplyStage::Assets],
            &|| None,
        );
        assert_eq!(
            fresh,
            OnboardingApplyResponse::Applied {
                scan_id: "s".into(),
                applied: vec![OnboardingApplyStage::Assets],
                notices: Some(vec!["wrote".into()])
            }
        );
    }

    #[test]
    fn scan_without_sessions_emits_only_done() {
        let mut events = Vec::new();
        let outcome = run_onboarding_scan(
            &OnboardingScanEnvironment::default(),
            &OnboardingScanOptions {
                scan_id: "s".into(),
                include_custom_providers: true,
                sessions: false,
            },
            &mut |event| events.push(event),
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, OnboardingScanEventKind::Done);
        assert!(outcome.digests.is_empty() && outcome.plans.is_none());
    }
}
