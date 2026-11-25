//! Deterministic `/doctor` checks. Every check is read-only except the skill
//! frontmatter repair, which is the one documented auto-fix.
//! Port of `components/memory/commands/doctor-checks.ts` at pin 77f3067f1.

use std::path::{Path, PathBuf};

use memory_core::{
    locks::parse_lock_record,
    memfs::parse_memory_file,
    seeds::V1_PERSONA_SEED_SHA256,
    support::{host::hostname, sha256::sha256_hex},
};

use super::repo::run_git;
use super::tokens::estimate_system_tokens;
use super::types::{MemoryCommandDeps, MemoryCommandIdentity};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckLevel {
    Ok,
    Warn,
    Fail,
}

impl CheckLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

pub struct DoctorCheck {
    pub name: String,
    pub level: CheckLevel,
    pub detail: String,
}

const PERSONA_PATH: &str = "system/persona.md";

fn list_markdown(root: &Path, prefix: &str) -> Vec<String> {
    let target = if prefix.is_empty() {
        root.to_path_buf()
    } else {
        root.join(prefix)
    };
    let Ok(entries) = std::fs::read_dir(&target) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == ".git" || (prefix.is_empty() && name == "skills") {
            continue;
        }
        let relative = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            files.extend(list_markdown(root, &relative));
        } else if file_type.is_file() && name.ends_with(".md") {
            files.push(relative);
        }
    }
    files
}

pub fn check_repository(identity: &MemoryCommandIdentity) -> DoctorCheck {
    let repo = &identity.identity_paths.repo;
    if !repo.join(".git").exists() {
        return DoctorCheck {
            name: "repository".to_owned(),
            level: CheckLevel::Fail,
            detail: format!(
                "missing at {}; run /memfs init to create it",
                repo.display()
            ),
        };
    }
    DoctorCheck {
        name: "repository".to_owned(),
        level: CheckLevel::Ok,
        detail: repo.display().to_string(),
    }
}

pub fn check_frontmatter(repo_dir: &Path) -> Vec<DoctorCheck> {
    let paths = list_markdown(repo_dir, "");
    let mut failures: Vec<String> = Vec::new();
    for path in &paths {
        let content = match std::fs::read_to_string(repo_dir.join(path)) {
            Ok(content) => content,
            Err(error) => {
                failures.push(format!("{path} (unreadable: {error})"));
                continue;
            }
        };
        if let Err(error) = parse_memory_file(&content) {
            failures.push(format!("{path} ({error})"));
        }
    }

    let frontmatter = if failures.is_empty() {
        DoctorCheck {
            name: "frontmatter".to_owned(),
            level: CheckLevel::Ok,
            detail: format!(
                "{} memory file{} valid",
                paths.len(),
                if paths.len() == 1 { "" } else { "s" }
            ),
        }
    } else {
        DoctorCheck {
            name: "frontmatter".to_owned(),
            level: CheckLevel::Fail,
            detail: format!(
                "{} invalid file{}: {}; fix the frontmatter or remove the file",
                failures.len(),
                if failures.len() == 1 { "" } else { "s" },
                failures.join("; ")
            ),
        }
    };

    let persona = if paths.iter().any(|path| path == PERSONA_PATH) {
        DoctorCheck {
            name: "persona".to_owned(),
            level: CheckLevel::Ok,
            detail: format!("{PERSONA_PATH} present"),
        }
    } else {
        DoctorCheck {
            name: "persona".to_owned(),
            level: CheckLevel::Warn,
            detail: format!("{PERSONA_PATH} is missing; create it with /init or the memory tools"),
        }
    };

    vec![frontmatter, persona]
}

pub fn check_soul_seed(repo_dir: &Path) -> DoctorCheck {
    let Ok(content) = std::fs::read(repo_dir.join(PERSONA_PATH)) else {
        return DoctorCheck {
            name: "soul-seed".to_owned(),
            level: CheckLevel::Ok,
            detail: "no persona file to compare".to_owned(),
        };
    };
    if sha256_hex(&content) == V1_PERSONA_SEED_SHA256 {
        return DoctorCheck {
            name: "soul-seed".to_owned(),
            level: CheckLevel::Warn,
            detail: format!(
                "{PERSONA_PATH} is still the v1 seed; review it against the v2 soul seed and rewrite it with the memory tools when ready"
            ),
        };
    }
    DoctorCheck {
        name: "soul-seed".to_owned(),
        level: CheckLevel::Ok,
        detail: "persona differs from the v1 seed".to_owned(),
    }
}

pub fn check_locks(deps: &MemoryCommandDeps, locks_dir: &Path) -> DoctorCheck {
    let Ok(entries) = std::fs::read_dir(locks_dir) else {
        return DoctorCheck {
            name: "locks".to_owned(),
            level: CheckLevel::Ok,
            detail: "no lock directory yet".to_owned(),
        };
    };

    let host = hostname();
    let mut stale: Vec<String> = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.ends_with(".lock") {
            continue;
        }
        let path = locks_dir.join(name);
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let Some(record) = parse_lock_record(&content) else {
            stale.push(format!("{} (unreadable lock record)", path.display()));
            continue;
        };
        if record.hostname != host {
            continue;
        }
        if deps.process_alive(record.pid) {
            continue;
        }
        stale.push(format!(
            "{} (pid {} is not running)",
            path.display(),
            record.pid
        ));
    }

    if stale.is_empty() {
        return DoctorCheck {
            name: "locks".to_owned(),
            level: CheckLevel::Ok,
            detail: "no stale locks".to_owned(),
        };
    }
    DoctorCheck {
        name: "locks".to_owned(),
        level: CheckLevel::Warn,
        detail: format!(
            "{} stale lock{}: {}; delete the file after confirming no run is active",
            stale.len(),
            if stale.len() == 1 { "" } else { "s" },
            stale.join("; ")
        ),
    }
}

pub fn check_worktrees(deps: &MemoryCommandDeps, identity: &MemoryCommandIdentity) -> DoctorCheck {
    let repo = &identity.identity_paths.repo;
    let worktrees = &identity.identity_paths.worktrees;
    if !repo.join(".git").exists() {
        return DoctorCheck {
            name: "worktrees".to_owned(),
            level: CheckLevel::Ok,
            detail: "no repository to inspect".to_owned(),
        };
    }

    let result = match run_git(deps, repo, &["worktree", "list", "--porcelain"]) {
        Ok(result) => result,
        Err(error) => {
            return DoctorCheck {
                name: "worktrees".to_owned(),
                level: CheckLevel::Warn,
                detail: format!("could not list worktrees: {error}"),
            };
        }
    };
    if result.code != 0 {
        let detail = if result.stderr.trim().is_empty() {
            format!("exit {}", result.code)
        } else {
            result.stderr.trim().to_owned()
        };
        return DoctorCheck {
            name: "worktrees".to_owned(),
            level: CheckLevel::Warn,
            detail: format!("could not list worktrees: {detail}"),
        };
    }

    let registered: Vec<String> = result
        .stdout
        .split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| line.starts_with("worktree "))
        .map(|line| line["worktree ".len()..].trim().to_owned())
        .collect();

    let Ok(entries) = std::fs::read_dir(worktrees) else {
        return DoctorCheck {
            name: "worktrees".to_owned(),
            level: CheckLevel::Ok,
            detail: "no reflection worktrees".to_owned(),
        };
    };

    let orphans: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| worktrees.join(entry.file_name()).display().to_string())
        .filter(|path| !registered.contains(path))
        .collect();
    let prefix = format!("{}/", worktrees.display());
    let missing: Vec<String> = registered
        .iter()
        .filter(|path| path.starts_with(&prefix) && !Path::new(path.as_str()).exists())
        .cloned()
        .collect();

    let mut problems: Vec<String> = Vec::new();
    problems.extend(
        orphans
            .iter()
            .map(|path| format!("{path} (not registered with git)")),
    );
    problems.extend(
        missing
            .iter()
            .map(|path| format!("{path} (registered but missing; run git worktree prune)")),
    );
    if problems.is_empty() {
        return DoctorCheck {
            name: "worktrees".to_owned(),
            level: CheckLevel::Ok,
            detail: "no orphaned worktrees".to_owned(),
        };
    }
    DoctorCheck {
        name: "worktrees".to_owned(),
        level: CheckLevel::Warn,
        detail: format!(
            "{} orphan{}: {}",
            problems.len(),
            if problems.len() == 1 { "" } else { "s" },
            problems.join("; ")
        ),
    }
}

pub fn check_abandoned_runs(reflection_dir: &Path) -> DoctorCheck {
    let runs_dir = reflection_dir.join("runs");
    let Ok(entries) = std::fs::read_dir(&runs_dir) else {
        return DoctorCheck {
            name: "abandoned-runs".to_owned(),
            level: CheckLevel::Ok,
            detail: "no abandoned runs".to_owned(),
        };
    };
    let mut abandoned: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                return false;
            }
            let sentinel = entry.path().join("abandoned.json");
            if !sentinel.exists() {
                return false;
            }
            let Ok(content) = std::fs::read_to_string(sentinel) else {
                return true;
            };
            serde_json::from_str::<serde_json::Value>(&content)
                .ok()
                .and_then(|value| {
                    value
                        .get("reason")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                .as_deref()
                != Some("launch_interrupted")
        })
        .map(|entry| entry.path().display().to_string())
        .collect();
    abandoned.sort();
    if abandoned.is_empty() {
        return DoctorCheck {
            name: "abandoned-runs".to_owned(),
            level: CheckLevel::Ok,
            detail: "no abandoned runs".to_owned(),
        };
    }
    DoctorCheck {
        name: "abandoned-runs".to_owned(),
        level: CheckLevel::Warn,
        detail: format!(
            "{} run{} need manual disposal: {}",
            abandoned.len(),
            if abandoned.len() == 1 { "" } else { "s" },
            abandoned.join("; ")
        ),
    }
}

pub fn check_reflection_health(reflection_dir: &Path, now_ms: i64) -> DoctorCheck {
    let health = crate::worker::health::read_reflection_health(
        &reflection_dir.join("completions"),
        100,
        now_ms,
    );
    let last_success = health
        .last_success_at
        .clone()
        .unwrap_or_else(|| "never".to_owned());
    if health.streak == 0 && health.pending_count == 0 {
        return DoctorCheck {
            name: "reflection-health".to_owned(),
            level: CheckLevel::Ok,
            detail: format!("streak 0; pending 0; last success {last_success}"),
        };
    }
    let failure = health.last_failure.as_ref();
    let hint = crate::worker::remediation::reflection_remediation(
        failure.map(|failure| failure.reason.as_str()),
        failure.and_then(|failure| failure.detail.as_deref()),
    );
    DoctorCheck {
        name: "reflection-health".to_owned(),
        level: if health.streak >= 3 {
            CheckLevel::Warn
        } else {
            CheckLevel::Ok
        },
        detail: format!(
            "streak {}; fingerprint {}; pending {}; last success {last_success}; {hint}",
            health.streak,
            if health.fingerprint.is_empty() {
                "none"
            } else {
                health.fingerprint.as_str()
            },
            health.pending_count
        ),
    }
}

pub fn check_tokens(repo_dir: &Path, warn_tokens: usize) -> DoctorCheck {
    let estimate = estimate_system_tokens(repo_dir);
    if estimate.total_tokens < warn_tokens {
        return DoctorCheck {
            name: "tokens".to_owned(),
            level: CheckLevel::Ok,
            detail: format!(
                "~{} tokens in system/ (warn at {warn_tokens})",
                estimate.total_tokens
            ),
        };
    }
    DoctorCheck {
        name: "tokens".to_owned(),
        level: CheckLevel::Warn,
        detail: format!(
            "~{} tokens in system/ reached the {warn_tokens} warning threshold; trim system/ and move detail into external memory",
            estimate.total_tokens
        ),
    }
}

pub fn repo_dir_of(identity: &MemoryCommandIdentity) -> PathBuf {
    identity.identity_paths.repo.clone()
}

const DISK_WARN_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const DISK_FAIL_BYTES: u64 = 8 * 1024 * 1024 * 1024;

pub fn check_disk(identity: &MemoryCommandIdentity) -> DoctorCheck {
    check_disk_with_limits(identity, DISK_WARN_BYTES, DISK_FAIL_BYTES)
}

pub fn check_disk_with_limits(
    identity: &MemoryCommandIdentity,
    warn_bytes: u64,
    fail_bytes: u64,
) -> DoctorCheck {
    let root = &identity.identity_paths.root;
    let (bytes, files) = tree_usage(root);
    let label = format!(
        "{} MB across {} files in {}",
        bytes / (1024 * 1024),
        files,
        root.display()
    );
    let level = if bytes >= fail_bytes {
        CheckLevel::Fail
    } else if bytes >= warn_bytes {
        CheckLevel::Warn
    } else {
        CheckLevel::Ok
    };
    DoctorCheck {
        name: "disk".to_owned(),
        level,
        detail: if level == CheckLevel::Ok {
            label
        } else {
            format!(
                "{label}; transcripts expire after {}d, parked facts after {}d, reflection runs after {}d",
                crate::memory_retention::TRANSCRIPT_RETENTION_DAYS,
                crate::memory_retention::FACTS_PARKED_RETENTION_DAYS,
                crate::memory_retention::REFLECTION_RUN_RETENTION_DAYS
            )
        },
    }
}

fn tree_usage(root: &Path) -> (u64, u64) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return (0, 0);
    };
    let mut bytes = 0;
    let mut files = 0;
    for entry in entries.filter_map(Result::ok) {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            let (child_bytes, child_files) = tree_usage(&entry.path());
            bytes += child_bytes;
            files += child_files;
        } else {
            bytes += meta.len();
            files += 1;
        }
    }
    (bytes, files)
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSizeUnknownReport {
    pub budget_ms: u64,
    pub at: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeLastCleanupReport {
    pub at: String,
    pub total: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePastRetentionReport {
    pub runs: usize,
    pub journals: usize,
    pub sidecars: usize,
    pub unclassified_journals: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStoreSizeReport {
    pub store: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSizeReport {
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_unknown: Option<RuntimeSizeUnknownReport>,
    pub max_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<u64>,
    pub stores: Vec<RuntimeStoreSizeReport>,
    pub past_retention: RuntimePastRetentionReport,
    pub unclassified_journals: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unreflected_bytes: Option<u64>,
    pub discard_unreflected_journals: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evictable_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suspended_since: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_cleanup: Option<RuntimeLastCleanupReport>,
}

pub struct RuntimeSizeCheckInput {
    pub max_bytes: u64,
    pub policy: memory_core::retention::RuntimeRetentionPolicy,
    pub now_ms: i64,
    pub reflection_enabled: bool,
    pub discard_unreflected_journals: bool,
    pub measure_budget_ms: Option<u64>,
}

pub fn check_runtime_size(
    identity_paths: &memory_core::identity::layout::MemoryIdentityPaths,
    input: &RuntimeSizeCheckInput,
) -> (DoctorCheck, RuntimeSizeReport) {
    use memory_core::retention::{
        RuntimeCapEvictableOptions, RuntimeCapExcess, RuntimeSizeBudgetOutcome,
        RuntimeSizeStandingLevel, count_runtime_past_retention, describe_runtime_cap_excess,
        format_runtime_bytes, measure_runtime_cap_evictable_bytes,
        measure_runtime_size_within_budget, measure_unreflected_journal_bytes,
        read_last_retention_log_entry, read_unreflected_retention_gate, runtime_size_standing,
    };
    let deadline = input
        .measure_budget_ms
        .map(|ms| std::time::Instant::now() + std::time::Duration::from_millis(ms));
    let aborted = || deadline.is_some_and(|d| std::time::Instant::now() >= d);
    let measured = measure_runtime_size_within_budget(
        &identity_paths.runtime,
        input.measure_budget_ms,
        input.now_ms,
        Some(&aborted),
    );
    let (mut check, mut report) = match measured {
        RuntimeSizeBudgetOutcome::Unknown(unknown) => size_unknown_result(
            unknown.budget_ms,
            unknown.at,
            input.max_bytes,
            input.discard_unreflected_journals,
        ),
        RuntimeSizeBudgetOutcome::Size(size) => {
            let now_fn = || input.now_ms;
            let past = count_runtime_past_retention(
                identity_paths,
                &input.policy,
                &now_fn,
                None,
                input.reflection_enabled,
                Some(&aborted),
            );
            let mut admit = || !aborted();
            let gate = if input.discard_unreflected_journals {
                read_unreflected_retention_gate(
                    &identity_paths.reflection,
                    input.reflection_enabled,
                    &mut admit,
                )
            } else {
                None
            };
            let suspended_since = gate
                .as_ref()
                .filter(|g| !g.open)
                .and_then(|g| g.newest.as_ref())
                .map(|newest| newest.at.clone());
            let standing = runtime_size_standing(size.bytes, input.max_bytes);
            let over_cap = if input.max_bytes > 0 {
                size.bytes.saturating_sub(input.max_bytes)
            } else {
                0
            };
            let unreflected_bytes = if over_cap > 0 && !input.discard_unreflected_journals {
                measure_unreflected_journal_bytes(identity_paths, &input.policy, Some(&aborted))
                    .ok()
            } else {
                None
            };
            let mut cap_policy = input.policy.clone();
            cap_policy.discard_unreflected_journals = input.discard_unreflected_journals;
            let evictable_bytes = if over_cap > 0 {
                measure_runtime_cap_evictable_bytes(&RuntimeCapEvictableOptions {
                    paths: identity_paths,
                    policy: cap_policy,
                    now_ms: &now_fn,
                    reflection_enabled: input.reflection_enabled,
                    journal_floor_ms: None,
                    aborted: Some(&aborted),
                })
                .ok()
            } else {
                None
            };
            let excess = describe_runtime_cap_excess(RuntimeCapExcess {
                over_cap_bytes: over_cap,
                evictable_bytes: evictable_bytes.unwrap_or(0),
                unreflected_bytes: unreflected_bytes.unwrap_or(0),
                discard_unreflected_journals: input.discard_unreflected_journals,
            });
            let stores: Vec<RuntimeStoreSizeReport> = size
                .stores
                .into_iter()
                .take(3)
                .map(|s| RuntimeStoreSizeReport {
                    store: s.store,
                    bytes: s.bytes,
                })
                .collect();
            let report = RuntimeSizeReport {
                bytes: size.bytes,
                size_unknown: None,
                max_bytes: input.max_bytes,
                percent: standing.percent,
                stores: stores.clone(),
                past_retention: RuntimePastRetentionReport {
                    runs: past.runs,
                    journals: past.journals,
                    sidecars: past.sidecars,
                    unclassified_journals: past.unclassified_journals,
                },
                unclassified_journals: past.unclassified_journals,
                unreflected_bytes,
                discard_unreflected_journals: input.discard_unreflected_journals,
                evictable_bytes,
                suspended_since: suspended_since.clone(),
                last_cleanup: None,
            };
            let cap = if input.max_bytes > 0 {
                format!(
                    "{}% of the {} cap",
                    standing.percent.unwrap_or(0),
                    format_runtime_bytes(input.max_bytes)
                )
            } else {
                "no cap".to_owned()
            };
            let largest = stores
                .iter()
                .map(|store| format!("{} {}", store.store, format_runtime_bytes(store.bytes)))
                .collect::<Vec<_>>()
                .join(", ");
            let past_total = past.runs + past.journals + past.sidecars;
            let level = match standing.level {
                RuntimeSizeStandingLevel::Ok => CheckLevel::Ok,
                RuntimeSizeStandingLevel::Warn => CheckLevel::Warn,
            };
            let mut parts = vec![format!("{} ({cap})", format_runtime_bytes(size.bytes))];
            if !largest.is_empty() {
                parts.push(format!("largest: {largest}"));
            }
            parts.push(format!(
                "past retention: {} runs, {} journals, {} sidecars",
                past.runs, past.journals, past.sidecars
            ));
            if past.unclassified_journals > 0 {
                parts.push(format!(
                    "{} unclassified journals kept",
                    past.unclassified_journals
                ));
            }
            if let Some(excess_line) = &excess {
                parts.push(excess_line.clone());
            }
            if let Some(since) = &suspended_since {
                parts.push(format!(
                    "abandoned-journal retention suspended since {since} (reflection is failing)"
                ));
            }
            if level == CheckLevel::Warn && excess.is_none() {
                parts.push(format!(
                    "the next maintenance pass evicts the oldest runtime data down to the cap{}",
                    if past_total > 0 {
                        " after retention frees what is past its window"
                    } else {
                        ""
                    }
                ));
            }
            if level == CheckLevel::Warn {
                parts.push(
                    "raise memory.runtime.max_bytes, or set it to 0, to keep more".to_owned(),
                );
            }
            (
                DoctorCheck {
                    name: "runtime-size".to_owned(),
                    level,
                    detail: parts.join("; "),
                },
                report,
            )
        }
    };
    let last_cleanup = read_last_retention_log_entry(&identity_paths.runtime)
        .ok()
        .flatten()
        .map(|entry| RuntimeLastCleanupReport {
            at: entry.at,
            total: entry.total,
        });
    let cleanup_text = match &last_cleanup {
        Some(entry) => format!("last cleanup {} removed {}", entry.at, entry.total),
        None => "no cleanup recorded".to_owned(),
    };
    check.detail = format!("{}; {cleanup_text}", check.detail);
    report.last_cleanup = last_cleanup;
    (check, report)
}

fn size_unknown_result(
    budget_ms: u64,
    at: String,
    max_bytes: u64,
    discard_unreflected_journals: bool,
) -> (DoctorCheck, RuntimeSizeReport) {
    (
        DoctorCheck {
            name: "runtime-size".to_owned(),
            level: CheckLevel::Warn,
            detail: format!("size unknown: measurement exceeded {budget_ms} ms on {at}"),
        },
        RuntimeSizeReport {
            bytes: 0,
            size_unknown: Some(RuntimeSizeUnknownReport { budget_ms, at }),
            max_bytes,
            percent: None,
            stores: Vec::new(),
            past_retention: RuntimePastRetentionReport {
                runs: 0,
                journals: 0,
                sidecars: 0,
                unclassified_journals: 0,
            },
            unclassified_journals: 0,
            unreflected_bytes: None,
            discard_unreflected_journals,
            evictable_bytes: None,
            suspended_since: None,
            last_cleanup: None,
        },
    )
}
