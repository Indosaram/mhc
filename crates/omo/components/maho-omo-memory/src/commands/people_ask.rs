//! Dialectic-lite: a quick child that answers one question about one person from
//! the evidence the command already gathered (card, observations, /search hits).
//! Port of `components/memory/commands/people-ask.ts` at pin 77f3067f1.

use std::{collections::BTreeMap, sync::Arc};

use senpi_task::host::SenpiModelRegistry;
use serde_json::Value;

use crate::worker::resolve_model::{ReflectionModelResolution, resolve_reflection_model};

use super::types::BoxFuture;

const QUICK_CATEGORY: &str = "quick";
const DEFAULT_DEADLINE_MS: u64 = 120_000;
const MAX_OUTPUT_BYTES: usize = 32 * 1024;

pub const ABSTENTION_LINE: &str = "I don't know: the memory holds no evidence that answers this.";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeopleAskEvidence {
    pub card: Vec<String>,
    pub observations: Vec<String>,
    pub search_hits: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeopleAskRequest {
    pub slug: String,
    pub display_name: String,
    pub question: String,
    pub evidence: PeopleAskEvidence,
}

pub type PeopleAskRunner =
    Arc<dyn Fn(PeopleAskRequest) -> BoxFuture<'static, Result<String, String>> + Send + Sync>;

/// True when nothing in the memory can support an answer, so the child is never launched.
pub fn has_no_evidence(evidence: &PeopleAskEvidence) -> bool {
    evidence.card.is_empty() && evidence.observations.is_empty() && evidence.search_hits.is_empty()
}

const PERSONA: &str = "You answer one question about one person using ONLY the evidence supplied below.\n\
A confident \"I don't know\" is always correct: when the evidence does not settle the question, say so plainly and stop.\n\
Never invent a fact, never infer a trait the evidence does not state, and never speculate about intent.\n\
Cite the evidence you relied on by quoting its line. Keep the answer under 120 words.\n\
Explicit observations are stated facts; deductive, inductive, and contradiction observations are weaker and MUST be labelled as such when used.";

pub fn build_ask_prompt(request: &PeopleAskRequest) -> String {
    let mut sections = vec![
        format!("Person: {} ({})", request.display_name, request.slug),
        format!("Question: {}", request.question),
        String::new(),
        "CARD".to_owned(),
    ];
    if request.evidence.card.is_empty() {
        sections.push("(none)".to_owned());
    } else {
        sections.extend(request.evidence.card.iter().cloned());
    }
    sections.push(String::new());
    sections.push("OBSERVATIONS".to_owned());
    if request.evidence.observations.is_empty() {
        sections.push("(none)".to_owned());
    } else {
        sections.extend(request.evidence.observations.iter().cloned());
    }
    sections.push(String::new());
    sections.push("SEARCH HITS".to_owned());
    if request.evidence.search_hits.is_empty() {
        sections.push("(none)".to_owned());
    } else {
        sections.extend(request.evidence.search_hits.iter().cloned());
    }
    sections.join("\n")
}

/// The command launcher a `senpi -p` child runs through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeopleAskLauncher {
    pub command: String,
    pub prefix_args: Vec<String>,
}

pub struct PeopleAskOptions {
    pub config: Value,
    pub registry: Option<Arc<dyn SenpiModelRegistry>>,
    pub env: BTreeMap<String, String>,
    pub launcher: Option<PeopleAskLauncher>,
    pub deadline_ms: Option<u64>,
}

/// Production runner: a `senpi -p` quick child, deadline-bounded and tool-free.
/// Returns the abstention line when no quick model is available, because a
/// missing model is exactly a state in which nothing is known.
pub fn create_people_ask_runner(options: PeopleAskOptions) -> PeopleAskRunner {
    Arc::new(move |request: PeopleAskRequest| {
        let config = options.config.clone();
        let registry = options.registry.clone();
        let env = options.env.clone();
        let launcher = options.launcher.clone();
        let deadline_ms = options.deadline_ms.unwrap_or(DEFAULT_DEADLINE_MS);
        Box::pin(async move {
            let resolution = resolve_reflection_model(
                QUICK_CATEGORY,
                &config,
                registry.as_deref(),
                None,
            )
            .map_err(|error| error.to_string())?;
            let ReflectionModelResolution::Resolved { model, thinking, .. } = resolution else {
                return Ok(ABSTENTION_LINE.to_owned());
            };
            let Some(launcher) = launcher else {
                return Ok(ABSTENTION_LINE.to_owned());
            };

            let mut args: Vec<String> = launcher.prefix_args.clone();
            args.extend([
                "-p".to_owned(),
                "--system-prompt".to_owned(),
                PERSONA.to_owned(),
                "--tools".to_owned(),
                "none".to_owned(),
                "--no-extensions".to_owned(),
                "--no-skills".to_owned(),
                "--no-prompt-templates".to_owned(),
                "--no-context-files".to_owned(),
                "--model".to_owned(),
                model,
            ]);
            if let Some(thinking) = thinking {
                args.push("--thinking".to_owned());
                args.push(thinking);
            }
            args.push(build_ask_prompt(&request));

            run_child(&launcher.command, &args, &env, deadline_ms).await
        })
    })
}

async fn run_child(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    deadline_ms: u64,
) -> Result<String, String> {
    let mut child = tokio::process::Command::new(command)
        .args(args)
        .envs(env)
        .env("SENPI_PTY_FORCE_PIPE", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;

    let deadline = std::time::Duration::from_millis(deadline_ms);
    match tokio::time::timeout(deadline, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            if !output.status.success() {
                return Ok(ABSTENTION_LINE.to_owned());
            }
            let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
            if stdout.len() > MAX_OUTPUT_BYTES {
                stdout.truncate(MAX_OUTPUT_BYTES);
            }
            let answer = stdout.trim().to_owned();
            if answer.is_empty() {
                Ok(ABSTENTION_LINE.to_owned())
            } else {
                Ok(answer)
            }
        }
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => {
            let _ = child.start_kill();
            Ok(ABSTENTION_LINE.to_owned())
        }
    }
}
