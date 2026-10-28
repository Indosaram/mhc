use crate::projection_pin::{
    NativeProjectionPins, ProjectionAdvanceInput, ProjectionPinRecord, ProjectionPins,
    ProjectionTurn, render_projected_changes_line,
};
use memory_core::{
    compile::{
        cache::MemoryBlockCache,
        compile::{CompileError, CompileMemoryBlockOptions},
        render::{RenderError, mark_memory_block, replace_memory_block},
    },
    git::GitMemoryRepo,
};

pub const MEMORY_PROMPT_TEMPLATE: &str = "omo-senpi:before_agent_start:v3";
pub const MEMORY_NOTICE_CUSTOM_TYPE: &str = "omo-memory:notice";
pub const MEMORY_NUDGE_METADATA_TOKEN: &str = "user turns since your last memory save";
pub const MEMORY_SOUL_METADATA_TOKEN: &str = "Soul updated by";
pub const MEMORY_AGREEMENT_METADATA_TOKEN: &str = "Agreement NEEDS-HUMAN";
const MEMORY_TOOL_DISCOVERY_NOTE: &str = "The memory tools are discoverable through tool_search: run `tool_search(\"memory\")` once to activate them, then use them for every save.";

pub struct MemoryPromptSession<'a> {
    pub id: &'a str,
    pub prior_message_count: usize,
    pub branch: &'a [maho_ext_api::SessionEntry],
}
pub struct MemoryPromptInput<'a> {
    pub system_prompt: &'a str,
    pub session: MemoryPromptSession<'a>,
    pub repo: &'a GitMemoryRepo,
    pub identity: &'a str,
    pub search_exposure: bool,
    pub nudge_turns: Option<usize>,
    pub soul_sha: Option<&'a str>,
    /// Needs-human agreements consumed for this turn; empty in a preview.
    pub flagged_agreements: &'a [memory_core::agreements::AgreementItem],
}
#[derive(Debug)]
pub enum MemoryPromptError {
    Compile(CompileError),
    Render(RenderError),
}
impl std::fmt::Display for MemoryPromptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Compile(error) => error.fmt(f),
            Self::Render(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for MemoryPromptError {}
#[derive(Debug)]
pub struct MemoryPromptNotice {
    pub custom_type: &'static str,
    pub content: String,
    pub display: bool,
}
#[derive(Debug)]
pub struct MemoryPromptResult {
    pub system_prompt: String,
    pub message: MemoryPromptNotice,
}

#[derive(Default)]
pub struct MemoryPromptHandler {
    pub cache: MemoryBlockCache,
    pub pins: NativeProjectionPins,
    pub record_pin: std::sync::Mutex<Option<PromptPinRecorder>>,
    pub agreements: std::sync::Mutex<Option<AgreementsNoticeInput>>,
}
/// The optional agreements notice input (pin `resolveAgreementsNotice` + `onNoticeInputFailed`): the
/// resolver is skipped in a preview, and a failure is reported with input `"agreements"`.
#[derive(Clone)]
pub struct AgreementsNoticeInput {
    pub resolve: PromptAgreementsResolver,
    pub on_failed: Option<PromptNoticeInputFailed>,
}
impl MemoryPromptHandler {
    pub fn prebuild(&self, context: &crate::context::MemoryIdentityContext) -> Result<(), String> {
        let repo = GitMemoryRepo::open(&context.identity_paths.repo, &context.identity)
            .map_err(|error| error.to_string())?;
        self.cache
            .compile(
                &repo,
                MEMORY_PROMPT_TEMPLATE,
                &CompileMemoryBlockOptions {
                    agent_id: context.identity.clone(),
                    projection: None,
                },
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
    pub fn persistent() -> Self {
        Self {
            cache: MemoryBlockCache::persistent(
                env!("CARGO_PKG_VERSION").to_string() + ":omo-v5.1.47",
            ),
            ..Default::default()
        }
    }
    pub fn set_agreements_notice(&self, input: Option<AgreementsNoticeInput>) {
        *self
            .agreements
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = input;
    }
}
/// Persists a pin as a session entry so a resumed or restarted session reproduces the same bytes.
pub type PromptPinRecorder = std::sync::Arc<dyn Fn(&ProjectionPinRecord) + Send + Sync>;
pub type PromptContextResolver =
    std::sync::Arc<dyn Fn(&str) -> Option<crate::context::MemoryIdentityContext> + Send + Sync>;
pub type PromptNudgeResolver = std::sync::Arc<
    dyn Fn(&GitMemoryRepo, &str, &str) -> Result<Option<usize>, String> + Send + Sync,
>;
pub type PromptSoulResolver = std::sync::Arc<
    dyn Fn(&GitMemoryRepo, &str, &str) -> Result<Option<String>, String> + Send + Sync,
>;
pub type PromptAgreementsResolver = std::sync::Arc<
    dyn Fn(
            &GitMemoryRepo,
            &str,
            &str,
        ) -> Result<Option<Vec<memory_core::agreements::AgreementItem>>, String>
        + Send
        + Sync,
>;
/// Told when a notice input failed: `(input, session_id, error)`; `input` is `"agreements"` here.
pub type PromptNoticeInputFailed = std::sync::Arc<dyn Fn(&str, &str, &str) + Send + Sync>;
pub type PromptRepoFactory = std::sync::Arc<
    dyn Fn(&crate::context::MemoryIdentityContext) -> Result<GitMemoryRepo, String> + Send + Sync,
>;
#[derive(Clone)]
pub struct MemoryPromptInjectionOptions {
    pub resolve_context: PromptContextResolver,
    pub create_repo: Option<PromptRepoFactory>,
    pub search_exposure: Option<std::sync::Arc<dyn Fn() -> bool + Send + Sync>>,
    pub resolve_nudge_turns: Option<PromptNudgeResolver>,
    pub resolve_soul_notice: Option<PromptSoulResolver>,
}
pub fn register_memory_prompt_handler(
    api: &mut maho_ext_api::ExtensionApi,
    options: MemoryPromptInjectionOptions,
) {
    register_memory_prompt_handler_with_cache(
        api,
        options,
        std::sync::Arc::new(MemoryPromptHandler::default()),
    );
}
pub fn register_memory_prompt_handler_with_cache(
    api: &mut maho_ext_api::ExtensionApi,
    options: MemoryPromptInjectionOptions,
    handler: std::sync::Arc<MemoryPromptHandler>,
) {
    use maho_ext_api::{
        BeforeAgentStartEventResult, CustomMessage, EventKind, EventResult, ExtensionEvent,
        ExtensionFailure, ToolContent,
    };
    let options = std::sync::Arc::new(options);
    // Pin records are session entries; the sender is captured at registration like every other
    // memory handler that appends one (`nudge_wiring.rs`, `soul_notice.rs`).
    let pin_sender = std::sync::Arc::new(maho_ext_api::ExtensionApi::new(
        maho_ext_api::LoadedExtension::new(
            "memory-projection-pin",
            api.cwd.clone(),
            Default::default(),
        ),
        api.profile.clone(),
        api.events.clone(),
        api.runtime.clone(),
    ));
    if let Ok(mut slot) = handler.record_pin.lock() {
        *slot = Some(std::sync::Arc::new(move |record: &ProjectionPinRecord| {
            if let Ok(value) = serde_json::to_value(record) {
                let _ = pin_sender.append_entry(
                    crate::projection_pin::PROJECTION_PIN_ENTRY_TYPE,
                    Some(value),
                );
            }
        }));
    }
    let prebuild = handler.clone();
    let prebuild_options = options.clone();
    api.on(
        EventKind::SessionStart,
        std::sync::Arc::new(move |_, context| {
            let prebuild = prebuild.clone();
            let options = prebuild_options.clone();
            let identity = context
                .session_manager
                .get_branch()
                .is_empty()
                .then(|| (options.resolve_context)(context.session_manager.session_id()))
                .flatten();
            Box::pin(async move {
                if let Some(identity) = identity {
                    tokio::task::spawn_blocking(move || prebuild.prebuild(&identity))
                        .await
                        .map_err(|error| ExtensionFailure::new(error.to_string()))?
                        .map_err(ExtensionFailure::new)?;
                }
                Ok(EventResult::None)
            })
        }),
    );
    let settle = handler.clone();
    api.on(
        EventKind::SessionShutdown,
        std::sync::Arc::new(move |_, _| {
            let settle = settle.clone();
            Box::pin(async move {
                settle.cache.settled();
                Ok(EventResult::None)
            })
        }),
    );
    api.on(
        EventKind::BeforeAgentStart,
        std::sync::Arc::new(move |event, context| {
            let handler = handler.clone();
            let options = options.clone();
            Box::pin(async move {
                // Repo open, nudge, soul, and inject each block on git. Own the inputs, then run that
                // work on the blocking pool so the first poll of this future can yield.
                let (system_prompt, session_id, branch, identity, preview) = {
                    let ExtensionEvent::BeforeAgentStart(event) = event else {
                        return Ok(EventResult::None);
                    };
                    let id = context.session_manager.session_id();
                    if id.is_empty() {
                        return Ok(EventResult::None);
                    }
                    let Some(identity) = (options.resolve_context)(id) else {
                        return Ok(EventResult::None);
                    };
                    (
                        event.system_prompt.clone(),
                        id.to_owned(),
                        context.session_manager.get_branch(),
                        identity,
                        event.preview,
                    )
                };
                match tokio::task::spawn_blocking(move || {
                    let repo = match &options.create_repo {
                        Some(factory) => factory(&identity),
                        None => {
                            GitMemoryRepo::open(&identity.identity_paths.repo, &identity.identity)
                                .map_err(|error| error.to_string())
                        }
                    }
                    .map_err(ExtensionFailure::new)?;
                    let nudge = options
                        .resolve_nudge_turns
                        .as_ref()
                        .map(|resolve| resolve(&repo, &session_id, &identity.identity))
                        .transpose()
                        .map_err(ExtensionFailure::new)?
                        .flatten();
                    let soul = options
                        .resolve_soul_notice
                        .as_ref()
                        .map(|resolve| resolve(&repo, &session_id, &identity.identity))
                        .transpose()
                        .map_err(ExtensionFailure::new)?
                        .flatten();
                    let agreements = handler
                        .agreements
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    let flagged = match agreements.as_ref().filter(|_| !preview).map(|input| {
                        (
                            (input.resolve)(&repo, &session_id, &identity.identity),
                            input.on_failed.clone(),
                        )
                    }) {
                        Some((Ok(flagged), _)) => flagged.unwrap_or_default(),
                        Some((Err(error), failed)) => {
                            if let Some(failed) = failed {
                                failed("agreements", &session_id, &error);
                            }
                            Vec::new()
                        }
                        None => Vec::new(),
                    };
                    let input = MemoryPromptInput {
                        system_prompt: &system_prompt,
                        session: MemoryPromptSession {
                            id: &session_id,
                            prior_message_count: branch.len(),
                            branch: &branch,
                        },
                        repo: &repo,
                        identity: &identity.identity,
                        search_exposure: options
                            .search_exposure
                            .as_ref()
                            .is_some_and(|resolve| resolve()),
                        nudge_turns: nudge,
                        soul_sha: soul.as_deref(),
                        flagged_agreements: &flagged,
                    };
                    let result = handler
                        .inject(Some(&input))
                        .map_err(|error| ExtensionFailure::new(error.to_string()))?;
                    Ok(match result {
                        None => EventResult::None,
                        Some(result) => {
                            EventResult::BeforeAgentStart(BeforeAgentStartEventResult {
                                system_prompt_parts: None,
                                system_prompt: Some(result.system_prompt),
                                message: Some(CustomMessage {
                                    custom_type: result.message.custom_type.into(),
                                    content: vec![ToolContent::Text {
                                        text: result.message.content,
                                        audience: None,
                                    }],
                                    display: result.message.display,
                                    details: None,
                                }),
                            })
                        }
                    })
                })
                .await
                {
                    Ok(result) => result,
                    Err(error) => Err(ExtensionFailure::new(error.to_string())),
                }
            })
        }),
    );
}
impl MemoryPromptHandler {
    pub fn inject(
        &self,
        input: Option<&MemoryPromptInput<'_>>,
    ) -> Result<Option<MemoryPromptResult>, MemoryPromptError> {
        let Some(input) = input else {
            return Ok(None);
        };
        if input.session.id.is_empty() {
            return Ok(None);
        }
        // The pin advances before the compile so a memory commit mid-session cannot change the bytes
        // (#8470): the block compiles at this session's pinned revision.
        let head = input
            .repo
            .head()
            .map_err(|error| MemoryPromptError::Compile(CompileError::Git(error.to_string())))?;
        let turn = self
            .pins
            .advance(
                ProjectionAdvanceInput {
                    repo: input.repo,
                    session_id: input.session.id,
                    branch: input.session.branch,
                    head: head.clone(),
                },
                &|record: &ProjectionPinRecord| {
                    let guard = self
                        .record_pin
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if let Some(recorder) = guard.as_ref() {
                        recorder(record);
                    }
                },
            )
            .map_err(|error| MemoryPromptError::Compile(CompileError::Git(error.to_string())))?;
        let options = CompileMemoryBlockOptions {
            agent_id: input.identity.to_owned(),
            projection: None,
        };
        let block = if turn.revision.as_deref() == head.as_deref() {
            self.cache
                .compile_at_head(
                    input.repo,
                    head.as_deref(),
                    &format!("{MEMORY_PROMPT_TEMPLATE}:{}", input.identity),
                    &options,
                )
                .map_err(MemoryPromptError::Compile)?
        } else {
            // A stale pin uses the separate pinned-revision cache entry so it never evicts the HEAD entry.
            self.cache
                .compile_at_revision(
                    input.repo,
                    turn.revision.as_deref(),
                    &format!("{MEMORY_PROMPT_TEMPLATE}:{}", input.identity),
                    &options,
                )
                .map_err(MemoryPromptError::Compile)?
        };
        let composed = if input.search_exposure {
            format!("{block}\n\n{MEMORY_TOOL_DISCOVERY_NOTE}")
        } else {
            block
        };
        let system_prompt = replace_memory_block(
            input.system_prompt,
            &mark_memory_block(input.identity, &composed),
        )
        .map_err(MemoryPromptError::Render)?;
        Ok(Some(MemoryPromptResult {
            system_prompt,
            message: MemoryPromptNotice {
                custom_type: MEMORY_NOTICE_CUSTOM_TYPE,
                content: render_memory_notice(
                    input.session.prior_message_count,
                    input.nudge_turns,
                    input.soul_sha,
                    &turn,
                    input.flagged_agreements,
                ),
                display: false,
            },
        }))
    }
}
pub fn render_memory_notice(
    previous_message_count: usize,
    nudge_turns: Option<usize>,
    soul_sha: Option<&str>,
    turn: &ProjectionTurn,
    flagged_agreements: &[memory_core::agreements::AgreementItem],
) -> String {
    let mut lines = vec![
        "<memory_notice>".to_owned(),
        format!(
            "- {previous_message_count} previous messages between you and the user are stored in recall memory"
        ),
    ];
    if let Some(turns) = nudge_turns {
        lines.push(format!("- {turns} {MEMORY_NUDGE_METADATA_TOKEN}. Save durable facts now, or decide nothing qualifies."));
    }
    if let Some(sha) = soul_sha {
        lines.push(format!(
            "- {MEMORY_SOUL_METADATA_TOKEN} reflection {} since your last run",
            sha.chars().take(7).collect::<String>()
        ));
    }
    if let Some(changes) = &turn.changes {
        lines.push(render_projected_changes_line(
            changes,
            turn.revision.as_deref(),
            &turn.sources,
        ));
    }
    for item in flagged_agreements {
        lines.push(format!("- {MEMORY_AGREEMENT_METADATA_TOKEN} (broken x{}): \"{}\" Do not break it again this session; if the task needs it, stop and ask your person.",item.n,item.text));
    }
    lines.push("</memory_notice>".to_owned());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_core::git::{GitCommitAuthor, GitSeedFile, InitializeGitRepoOptions};
    fn fixture() -> (tempfile::TempDir, GitMemoryRepo) {
        let dir = tempfile::tempdir().unwrap();
        let repo = GitMemoryRepo::open(dir.path().join("repo"), "prompt-agent").unwrap();
        repo.init(Some(InitializeGitRepoOptions {
            seed_files: vec![GitSeedFile {
                relative_path: "system/persona.md".into(),
                content: "---\ndescription: Persona\n---\nfirst\n".into(),
            }],
            ..Default::default()
        }))
        .unwrap();
        (dir, repo)
    }
    fn input(repo: &GitMemoryRepo) -> MemoryPromptInput<'_> {
        MemoryPromptInput {
            system_prompt: "BASE PROMPT",
            session: MemoryPromptSession {
                id: "session-1",
                prior_message_count: 3,
                branch: &[],
            },
            repo,
            identity: "prompt-agent",
            search_exposure: false,
            nudge_turns: None,
            soul_sha: None,
            flagged_agreements: &[],
        }
    }
    fn recording_handler() -> (
        MemoryPromptHandler,
        std::sync::Arc<std::sync::Mutex<Vec<ProjectionPinRecord>>>,
    ) {
        let handler = MemoryPromptHandler::default();
        let records = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = records.clone();
        *handler.record_pin.lock().unwrap() =
            Some(std::sync::Arc::new(move |record: &ProjectionPinRecord| {
                sink.lock().unwrap().push(record.clone())
            }));
        (handler, records)
    }
    #[test]
    fn template_is_machine_cache_key() {
        assert_eq!(MEMORY_PROMPT_TEMPLATE, "omo-senpi:before_agent_start:v3");
    }
    #[test]
    fn unbound_passes_through() {
        assert!(
            MemoryPromptHandler::default()
                .inject(None)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn missing_session_passes_through() {
        let (_dir, repo) = fixture();
        let mut input = input(&repo);
        input.session.id = "";
        assert!(
            MemoryPromptHandler::default()
                .inject(Some(&input))
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn bound_projection_and_late_metadata() {
        let (_dir, repo) = fixture();
        let result = MemoryPromptHandler::default()
            .inject(Some(&input(&repo)))
            .unwrap()
            .unwrap();
        assert!(result.system_prompt.contains("first"));
        assert!(
            result
                .system_prompt
                .contains("<!-- senpi-memory:prompt-agent:begin -->")
        );
        assert!(!result.system_prompt.contains("previous messages"));
        assert_eq!(result.message.custom_type, MEMORY_NOTICE_CUSTOM_TYPE);
        assert!(!result.message.display);
    }
    #[test]
    fn volatile_metadata_preserves_system_bytes() {
        let (_dir, repo) = fixture();
        let handler = MemoryPromptHandler::default();
        let mut input = input(&repo);
        let before = handler.inject(Some(&input)).unwrap().unwrap();
        input.nudge_turns = Some(12);
        input.soul_sha = Some("a1b2c3d4e5");
        input.session.prior_message_count = 12;
        let after = handler.inject(Some(&input)).unwrap().unwrap();
        assert_eq!(before.system_prompt, after.system_prompt);
        assert!(after.message.content.contains(MEMORY_NUDGE_METADATA_TOKEN));
        assert!(after.message.content.contains(MEMORY_SOUL_METADATA_TOKEN));
    }
    #[test]
    fn flagged_agreements_are_late_metadata() {
        let (_dir, repo) = fixture();
        let handler = MemoryPromptHandler::default();
        let plain = handler.inject(Some(&input(&repo))).unwrap().unwrap();
        let flagged = [memory_core::agreements::AgreementItem {
            id: "0123456789".into(),
            scope: memory_core::agreements::AgreementScope::Global,
            by: memory_core::agreements::AgreementAuthor::Human,
            text: "no force pushes".into(),
            n: 3,
            since: None,
            last: None,
            status: memory_core::agreements::AgreementStatus::NeedsHuman,
        }];
        let mut input = input(&repo);
        input.flagged_agreements = &flagged;
        let result = handler.inject(Some(&input)).unwrap().unwrap();
        assert_eq!(result.system_prompt, plain.system_prompt);
        assert!(result.message.content.contains("- Agreement NEEDS-HUMAN (broken x3): \"no force pushes\" Do not break it again this session; if the task needs it, stop and ask your person."));
        assert!(
            !plain
                .message
                .content
                .contains(MEMORY_AGREEMENT_METADATA_TOKEN)
        );
    }
    #[test]
    fn nudge_is_late_metadata() {
        let (_dir, repo) = fixture();
        let mut input = input(&repo);
        input.nudge_turns = Some(2);
        let result = MemoryPromptHandler::default()
            .inject(Some(&input))
            .unwrap()
            .unwrap();
        assert!(!result.system_prompt.contains(MEMORY_NUDGE_METADATA_TOKEN));
        assert!(result.message.content.contains(MEMORY_NUDGE_METADATA_TOKEN));
    }
    #[test]
    fn soul_notice_is_late_metadata() {
        let (_dir, repo) = fixture();
        let mut input = input(&repo);
        input.soul_sha = Some("a1b2c3d4e5");
        let result = MemoryPromptHandler::default()
            .inject(Some(&input))
            .unwrap()
            .unwrap();
        assert!(!result.system_prompt.contains(MEMORY_SOUL_METADATA_TOKEN));
        assert!(result.message.content.contains("reflection a1b2c3d "));
    }
    #[test]
    fn foreign_prompt_survives() {
        let (_dir, repo) = fixture();
        let mut input = input(&repo);
        input.system_prompt = "BASE PROMPT\n\nFOREIGN EXTENSION TEXT";
        assert!(
            MemoryPromptHandler::default()
                .inject(Some(&input))
                .unwrap()
                .unwrap()
                .system_prompt
                .contains("FOREIGN EXTENSION TEXT")
        );
    }
    #[test]
    fn unchanged_head_uses_cached_block() {
        let (_dir, repo) = fixture();
        let handler = MemoryPromptHandler::default();
        let first = handler.inject(Some(&input(&repo))).unwrap().unwrap();
        let second = handler.inject(Some(&input(&repo))).unwrap().unwrap();
        assert_eq!(first.system_prompt, second.system_prompt);
        assert_eq!(handler.cache.size(), 1);
    }
    #[test]
    fn a_change_after_the_pin_keeps_the_prompt_bytes_and_is_announced_once() {
        let (_dir, repo) = fixture();
        let (handler, records) = recording_handler();
        let first = handler.inject(Some(&input(&repo))).unwrap().unwrap();
        std::fs::write(
            repo.dir.join("system/persona.md"),
            "---\ndescription: Persona\n---\nsecond\n",
        )
        .unwrap();
        std::fs::create_dir_all(repo.dir.join("reference")).unwrap();
        std::fs::write(
            repo.dir.join("reference/new.md"),
            "---\ndescription: New\n---\nnew\n",
        )
        .unwrap();
        repo.commit_write(&["system/persona.md", "reference/new.md"], "record reference/new.md\n\nOmo-Writer: memory-tool\nOmo-Session: other-session\nOmo-Turn: 1", &GitCommitAuthor { agent_id: "other-agent".into(), author_name: "Other Agent".into(), author_email: None }).unwrap();
        let second = handler.inject(Some(&input(&repo))).unwrap().unwrap();
        let third = handler.inject(Some(&input(&repo))).unwrap().unwrap();
        assert!(first.system_prompt.contains("first"));
        assert_eq!(second.system_prompt, first.system_prompt);
        assert!(
            second
                .message
                .content
                .contains(crate::projection_pin::MEMORY_CHANGES_METADATA_TOKEN)
        );
        assert!(second.message.content.contains("added reference/new.md"));
        assert!(second.message.content.contains("updated system/persona.md"));
        assert!(
            !third
                .message
                .content
                .contains(crate::projection_pin::MEMORY_CHANGES_METADATA_TOKEN)
        );
        assert_eq!(records.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_commit_this_session_wrote_is_not_announced() {
        let (_dir, repo) = fixture();
        let handler = MemoryPromptHandler::default();
        let first = handler.inject(Some(&input(&repo))).unwrap().unwrap();
        std::fs::create_dir_all(repo.dir.join("reference")).unwrap();
        std::fs::write(
            repo.dir.join("reference/mine.md"),
            "---\ndescription: Mine\n---\nmine\n",
        )
        .unwrap();
        repo.commit_write(&["reference/mine.md"], "record reference/mine.md\n\nOmo-Writer: memory-tool\nOmo-Session: session-1\nOmo-Turn: 1", &GitCommitAuthor { agent_id: "prompt-agent".into(), author_name: "Prompt Agent".into(), author_email: None }).unwrap();
        let second = handler.inject(Some(&input(&repo))).unwrap().unwrap();
        assert_eq!(second.system_prompt, first.system_prompt);
        assert!(
            !second
                .message
                .content
                .contains(crate::projection_pin::MEMORY_CHANGES_METADATA_TOKEN)
        );
    }

    #[test]
    fn a_new_session_compiles_the_new_head() {
        let (_dir, repo) = fixture();
        let handler = MemoryPromptHandler::default();
        let first = handler.inject(Some(&input(&repo))).unwrap().unwrap();
        std::fs::write(
            repo.dir.join("system/persona.md"),
            "---\ndescription: Persona\n---\nsecond\n",
        )
        .unwrap();
        repo.commit_write(
            &["system/persona.md"],
            "update persona",
            &GitCommitAuthor {
                agent_id: "other-agent".into(),
                author_name: "Other Agent".into(),
                author_email: None,
            },
        )
        .unwrap();
        let mut input = input(&repo);
        input.session.id = "session-2";
        let fresh = handler.inject(Some(&input)).unwrap().unwrap();
        assert!(first.system_prompt.contains("first"));
        assert!(fresh.system_prompt.contains("second"));
    }

    #[test]
    fn a_compaction_repins_the_session_to_head() {
        let (_dir, repo) = fixture();
        let handler = MemoryPromptHandler::default();
        handler.inject(Some(&input(&repo))).unwrap().unwrap();
        std::fs::write(
            repo.dir.join("system/persona.md"),
            "---\ndescription: Persona\n---\nsecond\n",
        )
        .unwrap();
        repo.commit_write(
            &["system/persona.md"],
            "update persona",
            &GitCommitAuthor {
                agent_id: "other-agent".into(),
                author_name: "Other Agent".into(),
                author_email: None,
            },
        )
        .unwrap();
        let branch = [maho_ext_api::SessionEntry {
            id: "compaction-1".into(),
            parent_id: None,
            timestamp: String::new(),
            kind: "compaction".into(),
            data: serde_json::json!({"type": "compaction", "id": "compaction-1", "firstKeptEntryId": "m1"}),
        }];
        let mut input = input(&repo);
        input.session.branch = &branch;
        let repinned = handler.inject(Some(&input)).unwrap().unwrap();
        assert!(repinned.system_prompt.contains("second"));
        assert!(
            !repinned
                .message
                .content
                .contains(crate::projection_pin::MEMORY_CHANGES_METADATA_TOKEN)
        );
    }
    #[test]
    fn existing_block_is_replaced() {
        let (_dir, repo) = fixture();
        let handler = MemoryPromptHandler::default();
        let first = handler.inject(Some(&input(&repo))).unwrap().unwrap();
        let mut input = input(&repo);
        input.system_prompt = &first.system_prompt;
        let second = handler.inject(Some(&input)).unwrap().unwrap();
        assert_eq!(
            second
                .system_prompt
                .matches("<!-- senpi-memory:prompt-agent:begin -->")
                .count(),
            1
        );
    }
    #[test]
    fn projection_matches_pinned_ts_golden() {
        let (_dir, repo) = fixture();
        std::fs::write(
            repo.dir.join("system/human.md"),
            "---\ndescription: Human\n---\nfixture person\n",
        )
        .unwrap();
        std::fs::create_dir_all(repo.dir.join("notes/facts")).unwrap();
        std::fs::write(
            repo.dir.join("notes/facts/example.md"),
            "---\ndescription: Example\n---\nfixture fact\n",
        )
        .unwrap();
        repo.commit_write(
            &["system/human.md", "notes/facts/example.md"],
            "fixture projection",
            &GitCommitAuthor {
                agent_id: "prompt-agent".into(),
                author_name: "Prompt Agent".into(),
                author_email: None,
            },
        )
        .unwrap();
        let result = MemoryPromptHandler::default()
            .inject(Some(&input(&repo)))
            .unwrap()
            .unwrap();
        let expected = memory_core::compile::render::replace_memory_block(
            "BASE PROMPT",
            &memory_core::compile::render::mark_memory_block(
                "prompt-agent",
                include_str!("../tests/golden/projection.txt"),
            ),
        )
        .unwrap();
        assert_eq!(result.system_prompt, expected);
    }
    #[test]
    fn soul_watermark_notice_consumed_once_at_same_head() {
        use memory_core::soul::watermark::{ConsumeSoulNoticeOptions, consume_soul_notice_delta};
        let (dir, repo) = fixture();
        let options = ConsumeSoulNoticeOptions {
            notices_dir: dir.path().join("notices"),
            locks_dir: dir.path().join("locks"),
            wait_timeout_ms: None,
        };
        assert!(
            consume_soul_notice_delta(&repo, &options)
                .unwrap()
                .is_none()
        );
        std::fs::write(
            repo.dir.join("system/persona.md"),
            "---\ndescription: Persona\n---\nevolved\n",
        )
        .unwrap();
        repo.commit_write(
            &["system/persona.md"],
            "chore(reflection): merge run r1\n\nOmo-Writer: reflection",
            &GitCommitAuthor {
                agent_id: "prompt-agent".into(),
                author_name: "Prompt Agent".into(),
                author_email: None,
            },
        )
        .unwrap();
        let handler = MemoryPromptHandler::default();
        let notice = consume_soul_notice_delta(&repo, &options).unwrap().unwrap();
        let mut first_input = input(&repo);
        first_input.soul_sha = Some(&notice.sha);
        let first = handler.inject(Some(&first_input)).unwrap().unwrap();
        assert!(first.message.content.contains(MEMORY_SOUL_METADATA_TOKEN));
        for _ in 0..2 {
            assert!(
                consume_soul_notice_delta(&repo, &options)
                    .unwrap()
                    .is_none()
            );
            let later = handler.inject(Some(&input(&repo))).unwrap().unwrap();
            assert_eq!(later.system_prompt, first.system_prompt);
            assert!(!later.message.content.contains(MEMORY_SOUL_METADATA_TOKEN));
        }
    }

    #[test]
    fn before_agent_start_yields_while_repo_factory_blocks() {
        let (_dir, repo) = fixture();
        // TestSession::get_branch is empty, so the handler's prior count is 0, not input()'s 3.
        let expected_input = MemoryPromptInput {
            system_prompt: "BASE PROMPT",
            session: MemoryPromptSession {
                id: "session-1",
                prior_message_count: 0,
                branch: &[],
            },
            repo: &repo,
            identity: "prompt-agent",
            search_exposure: false,
            nudge_turns: None,
            soul_sha: None,
            flagged_agreements: &[],
        };
        let expected = MemoryPromptHandler::default()
            .inject(Some(&expected_input))
            .unwrap()
            .unwrap();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = std::sync::Arc::new(std::sync::Mutex::new(Some(release_rx)));
        let repo_for_factory = repo.clone();
        let create_repo =
            std::sync::Arc::new(move |_identity: &crate::context::MemoryIdentityContext| {
                let release_rx = release_rx.lock().unwrap().take().unwrap();
                // Blocks the caller until the test releases. On a8ac6196 this runs inside the first poll.
                let _ = release_rx.recv();
                Ok(repo_for_factory.clone())
            });
        let identity = crate::context::MemoryIdentityContext::new(
            "prompt-agent".into(),
            memory_core::identity::layout::build_identity_paths(_dir.path(), "prompt-agent"),
            crate::binding::MemorySessionBinding {
                identity: "prompt-agent".into(),
                repo_path_hash: "hash".into(),
                bound_at: 1.0,
            },
        );
        let mut api = maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("memory", Default::default(), Default::default()),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        register_memory_prompt_handler(
            &mut api,
            MemoryPromptInjectionOptions {
                resolve_context: std::sync::Arc::new(move |_| Some(identity.clone())),
                create_repo: Some(create_repo),
                search_exposure: None,
                resolve_nudge_turns: None,
                resolve_soul_notice: None,
            },
        );
        let handler =
            api.registered.handlers[&maho_ext_api::EventKind::BeforeAgentStart][0].clone();
        let context = crate::commands::test_support::extension_context(std::sync::Arc::new(
            crate::commands::test_support::RecordingUi::default(),
        ));
        let mut event =
            maho_ext_api::ExtensionEvent::BeforeAgentStart(maho_ext_api::BeforeAgentStartEvent {
                prompt: String::new(),
                trigger: "prompt".into(),
                preview: false,
                images: None,
                system_prompt: "BASE PROMPT".into(),
                system_prompt_options: Default::default(),
            });
        let mut fut = handler(&mut event, &context);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let first = runtime.block_on(std::future::poll_fn(|_| {
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            std::task::Poll::Ready(fut.as_mut().poll(&mut cx))
        }));
        assert!(
            first.is_pending(),
            "handler future must yield while the repo factory is blocked"
        );
        release_tx.send(()).unwrap();
        let yielded = runtime.block_on(fut).unwrap();
        let maho_ext_api::EventResult::BeforeAgentStart(actual) = yielded else {
            panic!("expected BeforeAgentStart");
        };
        assert_eq!(
            actual.system_prompt.as_deref(),
            Some(expected.system_prompt.as_str())
        );
        let message = actual.message.expect("memory notice");
        assert_eq!(message.custom_type, expected.message.custom_type);
        assert_eq!(message.display, expected.message.display);
        assert_eq!(message.details, None);
        assert_eq!(
            message.content,
            vec![maho_ext_api::ToolContent::Text {
                text: expected.message.content,
                audience: None
            }]
        );
    }
}
