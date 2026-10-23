//! One understanding step (upstream `understand-step.ts`): the bounded read of digests and AGENTS.md, the
//! model payload, and one model call through the facts extractor's launch path in a scratch directory.

use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use maho_ext_api::ModelRegistry;
use memory_core::facts::extraction::{FactsExtractionRecord, parse_facts_extraction_record};
use serde_json::{Value, json};

use crate::session_history::{SessionDigest, session_digest::digest_byte_length};
use crate::understand_control::{Cancel, OnboardingUnderstandingControl};

/// Everything the facts-extractor launch path needs for one step; the call blocks until the child is done.
pub struct FactsStepLaunch<'a> {
    pub run_id: &'a str,
    pub run_dir: &'a Path,
    pub payload: &'a str,
    /// Resolved as category `quick`, no fallbacks.
    pub model: &'a str,
    pub model_registry: &'a Arc<dyn ModelRegistry>,
    pub env: &'a BTreeMap<String, String>,
    pub deadline_ms: u64,
    /// Record limits: at most this many `record_fact` calls.
    pub max_calls: u32,
    pub on_call: Arc<dyn Fn() + Send + Sync>,
    /// The launcher reports how to cancel the started child.
    pub register_cancel: &'a dyn Fn(Cancel),
}

/// The facts extractor's own launch path (`launchFactsInProcess`). Returns whether the child was cancelled.
pub trait FactsStepLauncher: Send + Sync {
    fn launch(&self, launch: &FactsStepLaunch<'_>) -> Result<bool, String>;
}

#[derive(Clone)]
pub struct UnderstandingModel {
    pub model: String,
    pub model_registry: Arc<dyn ModelRegistry>,
    pub env: BTreeMap<String, String>,
    pub launcher: Arc<dyn FactsStepLauncher>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentsMd {
    pub path: String,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct StepRead {
    pub digests: Vec<SessionDigest>,
    pub agents_md: Option<AgentsMd>,
    pub bytes: usize,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StepResult {
    Completed(Vec<FactsExtractionRecord>),
    Timeout(Vec<FactsExtractionRecord>),
    Cancelled(Vec<FactsExtractionRecord>),
    Failed {
        reason: String,
        records: Vec<FactsExtractionRecord>,
    },
}

pub fn read_step(
    digests: &[SessionDigest],
    project: Option<&str>,
    limit: usize,
    project_roots: &HashSet<String>,
) -> StepRead {
    let mut kept = Vec::new();
    let mut bytes = 0_usize;
    let mut truncated = false;
    for digest in digests {
        let size = digest_byte_length(digest);
        if bytes + size > limit {
            truncated = true;
            break;
        }
        kept.push(digest.clone());
        bytes += size;
    }
    let plain = |digests, bytes, truncated| StepRead {
        digests,
        agents_md: None,
        bytes,
        truncated,
    };
    let Some(project) = project.filter(|p| !truncated && project_roots.contains(*p)) else {
        return plain(kept, bytes, truncated);
    };
    let Some(inside) = agents_md_inside(project) else {
        return plain(kept, bytes, truncated);
    };
    let Some((text, read, cut)) = read_bounded(&inside, limit - bytes) else {
        return plain(kept, bytes, truncated);
    };
    StepRead {
        digests: kept,
        agents_md: Some(AgentsMd {
            path: Path::new(project)
                .join("AGENTS.md")
                .to_string_lossy()
                .into_owned(),
            text,
        }),
        bytes: bytes + read,
        truncated: cut,
    }
}

/// The real path of `<root>/AGENTS.md` when it resolves inside the root itself; a symlink out of the project
/// is never followed.
fn agents_md_inside(root: &str) -> Option<PathBuf> {
    let real_root = fs::canonicalize(root).ok()?;
    let real_file = fs::canonicalize(Path::new(root).join("AGENTS.md")).ok()?;
    let relative = real_file.strip_prefix(&real_root).ok()?;
    let first = relative.components().next()?;
    (!matches!(
        first,
        Component::ParentDir | Component::RootDir | Component::Prefix(_)
    ))
    .then_some(real_file)
}

fn read_bounded(path: &Path, limit: usize) -> Option<(String, usize, bool)> {
    let file = fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let size = usize::try_from(meta.len()).unwrap_or(usize::MAX);
    let mut buffer = Vec::new();
    file.take(u64::try_from(limit.min(size)).unwrap_or(u64::MAX))
        .read_to_end(&mut buffer)
        .ok()?;
    Some((
        String::from_utf8_lossy(&buffer).into_owned(),
        buffer.len(),
        size > limit,
    ))
}

pub fn step_payload(
    read: &StepRead,
    project: Option<&str>,
    today: &str,
    min_sessions: usize,
) -> String {
    let where_ = project.map_or_else(
        || "across several projects".to_owned(),
        |p| format!("in {p}"),
    );
    let with_agents = if read.agents_md.is_some() {
        ", and the project's AGENTS.md"
    } else {
        ""
    };
    let mut lines = vec![
        format!(
            "Onboarding: digests of the user's recent coding-agent sessions {where_}{with_agents}. Today is {today}."
        ),
        format!(
            "Record only durable facts about the user or their projects that at least {min_sessions} different sessions below support. Never record secrets, credentials, or one-off task details."
        ),
        String::new(),
        "<session_digests>".to_owned(),
    ];
    lines.extend(read.digests.iter().map(SessionDigest::to_json_string));
    lines.push("</session_digests>".to_owned());
    if let Some(agents) = &read.agents_md {
        lines.push(format!(
            "<agents_md path={}>",
            Value::String(agents.path.clone())
        ));
        lines.push(agents.text.clone());
        lines.push("</agents_md>".to_owned());
    }
    lines.join("\n")
}

pub struct RunStepInput<'a> {
    pub model: &'a UnderstandingModel,
    pub payload: String,
    pub deadline_ms: u64,
    pub max_calls: u32,
    pub on_call: Arc<dyn Fn() + Send + Sync>,
    pub control: &'a OnboardingUnderstandingControl,
    pub projects: HashSet<String>,
}

fn read_records(path: &Path) -> Vec<FactsExtractionRecord> {
    let raw = fs::read_to_string(path).unwrap_or_default();
    raw.split('\n')
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .filter_map(|(index, line)| {
            parse_facts_extraction_record(&serde_json::from_str::<Value>(line).ok()?, index).ok()
        })
        .collect()
}

/// One model call through the facts extractor's own launch path and `record_fact` tool, in a scratch run
/// directory outside memory that is removed afterwards. The records are returned; nothing is applied.
pub fn run_step(input: RunStepInput<'_>) -> StepResult {
    let run_id = format!(
        "omo-onboarding-understand-{}",
        uuid::Uuid::new_v4().simple()
    );
    let run_dir = std::env::temp_dir().join(&run_id);
    let cleanup = |dir: &Path| {
        let _ = fs::remove_dir_all(dir);
    };
    if let Err(error) = fs::create_dir_all(&run_dir) {
        return StepResult::Failed {
            reason: error.to_string(),
            records: Vec::new(),
        };
    }
    let result = (|| {
        let ledger = json!({"version": 1, "runId": run_id, "kind": "facts"}).to_string();
        if let Err(error) = fs::write(run_dir.join("ledger.json"), ledger) {
            return StepResult::Failed {
                reason: error.to_string(),
                records: Vec::new(),
            };
        }
        let projects = input.projects.clone();
        let control = input.control;
        let register = move |cancel: Cancel| control.attach(projects.clone(), cancel);
        let launched = input.model.launcher.launch(&FactsStepLaunch {
            run_id: &run_id,
            run_dir: &run_dir,
            payload: &input.payload,
            model: &input.model.model,
            model_registry: &input.model.model_registry,
            env: &input.model.env,
            deadline_ms: input.deadline_ms,
            max_calls: input.max_calls,
            on_call: Arc::clone(&input.on_call),
            register_cancel: &register,
        });
        input.control.detach();
        let cancelled = match launched {
            Ok(cancelled) => cancelled,
            Err(reason) => {
                return StepResult::Failed {
                    reason,
                    records: Vec::new(),
                };
            }
        };
        let records = read_records(&run_dir.join("extraction.jsonl"));
        if cancelled {
            return StepResult::Cancelled(records);
        }
        let outcome = fs::read_to_string(run_dir.join("outcome.json"))
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
        if outcome.as_ref().and_then(|o| o["timedOut"].as_bool()) == Some(true) {
            return StepResult::Timeout(records);
        }
        if outcome
            .as_ref()
            .and_then(|o| o["childExit"]["code"].as_i64())
            == Some(0)
        {
            return StepResult::Completed(records);
        }
        StepResult::Failed {
            reason: "the model call failed".into(),
            records,
        }
    })();
    // The launch call returns only once its children are released, so one removal covers the scratch dir.
    cleanup(&run_dir);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_history::AgentSessionSource;

    fn digest(key: &str, cwd: &str) -> SessionDigest {
        SessionDigest {
            session_key: key.into(),
            source: AgentSessionSource::Codex,
            provider_session_id: key.into(),
            cwd: cwd.into(),
            title: "t".into(),
            model: None,
            started_at: "2026-01-01T00:00:00.000Z".into(),
            last_active_at: "2026-01-01T00:00:00.000Z".into(),
            message_count: 1,
            first_request: "hi".into(),
            tools_used: vec![],
            files_changed: vec![],
            outcome: String::new(),
            truncated: false,
        }
    }

    #[test]
    fn digests_are_kept_while_the_cumulative_size_fits() {
        let digests = [digest("a", "/p"), digest("b", "/p"), digest("c", "/p")];
        let one = digest_byte_length(&digests[0]);
        let read = read_step(&digests, None, one * 2, &HashSet::new());
        assert_eq!(
            (read.digests.len(), read.bytes, read.truncated),
            (2, one * 2, true)
        );
        let all = read_step(&digests, None, one * 3, &HashSet::new());
        assert!(!all.truncated && all.digests.len() == 3);
    }

    #[test]
    fn agents_md_is_read_only_inside_a_listed_project_and_never_through_a_symlink_out()
    -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("proj");
        fs::create_dir_all(&project)?;
        fs::write(project.join("AGENTS.md"), "rules")?;
        let root = project.to_string_lossy().into_owned();
        let roots: HashSet<String> = [root.clone()].into_iter().collect();
        let digests = [digest("a", &root), digest("b", &root)];
        let read = read_step(&digests, Some(&root), 100_000, &roots);
        assert_eq!(
            read.agents_md.as_ref().map(|a| a.text.as_str()),
            Some("rules")
        );
        assert!(
            read_step(&digests, Some(&root), 100_000, &HashSet::new())
                .agents_md
                .is_none()
        );
        #[cfg(unix)]
        {
            let outside = dir.path().join("secret.md");
            fs::write(&outside, "secret")?;
            fs::remove_file(project.join("AGENTS.md"))?;
            std::os::unix::fs::symlink(&outside, project.join("AGENTS.md"))?;
            assert!(
                read_step(&digests, Some(&root), 100_000, &roots)
                    .agents_md
                    .is_none()
            );
        }
        Ok(())
    }

    #[test]
    fn payload_carries_the_instruction_digests_and_agents_block() {
        let read = StepRead {
            digests: vec![digest("a", "/p")],
            agents_md: Some(AgentsMd {
                path: "/p/AGENTS.md".into(),
                text: "rules".into(),
            }),
            bytes: 0,
            truncated: false,
        };
        let payload = step_payload(&read, Some("/p"), "2026-10-22", 2);
        assert!(payload.starts_with("Onboarding: digests of the user's recent coding-agent sessions in /p, and the project's AGENTS.md. Today is 2026-10-22.\nRecord only durable facts about the user or their projects that at least 2 different sessions below support."));
        assert!(
            payload.contains("<session_digests>\n")
                && payload.contains(
                    "</session_digests>\n<agents_md path=\"/p/AGENTS.md\">\nrules\n</agents_md>"
                )
        );
        let none = step_payload(&StepRead::default(), None, "d", 2);
        assert!(none.contains("across several projects. Today is d."));
    }

    struct Launcher(bool);
    impl FactsStepLauncher for Launcher {
        fn launch(&self, launch: &FactsStepLaunch<'_>) -> Result<bool, String> {
            assert!(launch.run_dir.join("ledger.json").exists());
            fs::write(
                launch.run_dir.join("extraction.jsonl"),
                "{\"scope\":\"project\",\"text\":\"uses rust\",\"date\":\"2026-10-22\"}\n",
            )
            .map_err(|e| e.to_string())?;
            fs::write(
                launch.run_dir.join("outcome.json"),
                "{\"childExit\":{\"code\":0}}",
            )
            .map_err(|e| e.to_string())?;
            (launch.on_call)();
            Ok(self.0)
        }
    }

    struct NoRegistry;
    impl ModelRegistry for NoRegistry {
        fn get_all(&self) -> Vec<maho_ai::model::Model> {
            Vec::new()
        }
        fn get_available(&self) -> Vec<maho_ai::model::Model> {
            Vec::new()
        }
        fn find(&self, _: &str, _: &str) -> Option<maho_ai::model::Model> {
            None
        }
        fn has_configured_auth(&self, _: &maho_ai::model::Model) -> bool {
            false
        }
        fn get_api_key_for_provider<'a>(
            &'a self,
            _: &'a str,
        ) -> maho_ext_api::ExtensionFuture<'a, Option<String>> {
            Box::pin(async { Ok(None) })
        }
    }

    #[test]
    fn a_step_returns_records_and_removes_its_scratch_dir() {
        for (cancelled, expect_cancelled) in [(false, false), (true, true)] {
            let model = UnderstandingModel {
                model: "p/m".into(),
                model_registry: Arc::new(NoRegistry),
                env: BTreeMap::new(),
                launcher: Arc::new(Launcher(cancelled)),
            };
            let control = OnboardingUnderstandingControl::new();
            let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
            let counted = Arc::clone(&calls);
            let result = run_step(RunStepInput {
                model: &model,
                payload: "x".into(),
                deadline_ms: 1000,
                max_calls: 4,
                on_call: Arc::new(move || {
                    counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }),
                control: &control,
                projects: HashSet::new(),
            });
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
            match (result, expect_cancelled) {
                (StepResult::Completed(records), false)
                | (StepResult::Cancelled(records), true) => assert_eq!(records.len(), 1),
                (other, _) => panic!("unexpected {other:?}"),
            }
        }
    }
}
