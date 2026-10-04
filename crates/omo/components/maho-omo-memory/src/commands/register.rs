use std::sync::Arc;
use maho_ext_api::{ExtensionApi, ExtensionContext, ExtensionFailure, NotificationType};
use memory_core::reflection::ReflectionEvent;
use crate::{context::MemoryIdentityContext, facts_wiring::FactsExtractorWork};

/// The reflection runner the command layer calls.
pub type Reflect = Arc<dyn Fn(&str, ReflectionEvent) -> Result<(String, String), String> + Send + Sync>;
pub struct MemoryCommandDeps {
    pub resolve_context: crate::prompt::PromptContextResolver,
    pub settings: Arc<dyn Fn() -> Result<serde_json::Value, String> + Send + Sync>,
    pub actions: Arc<dyn maho_ext_api::ExtensionActions>,
    pub prompt: Arc<crate::prompt::MemoryPromptHandler>,
    pub sessions_dir: std::path::PathBuf,
    pub reflect: Reflect,
    pub dream: Arc<crate::dream_trigger::DreamTriggerWiring>,
    pub facts_retry: Arc<dyn Fn(String) -> FactsExtractorWork + Send + Sync>,
}

pub fn register_memory_commands(api: &mut ExtensionApi, deps: MemoryCommandDeps) {
    let deps = Arc::new(deps);
    for name in ["memory", "remember", "init", "recompile", "search", "reflect", "dream", "facts", "sleeptime"] {
        let deps = deps.clone();
        api.register_command(name, Some(format!("Memory {name}")), None, Arc::new(move |args, context| {
            let deps = deps.clone(); Box::pin(async move {
                let result = run_command(name, args, context, &deps).await;
                match result {
                    Ok(text) => { context.ui.notify(&text, NotificationType::Info); Ok(()) },
                    Err(error) => { context.ui.notify(&error, NotificationType::Error); Err(ExtensionFailure::new(error)) },
                }
            })
        }));
    }
}

async fn run_command(name: &str, args: &str, context: &ExtensionContext, deps: &MemoryCommandDeps) -> Result<String, String> {
    let session = context.session_manager.session_id();
    let identity = (deps.resolve_context)(session).ok_or("memory is not bound to this session; start a session with memory enabled and retry")?;
    let (positionals, flags) = super::args::parse_command_args(args, &[]);
    let conversations = flags.get("conversation").and_then(Option::as_ref).map(|ids| ids.split(',').map(str::trim).filter(|id| !id.is_empty()).map(str::to_owned).collect::<Vec<_>>());
    let focus = (!positionals.is_empty()).then(|| positionals.join(" "));
    match name {
        "memory" => {
            let repo=memory_core::git::GitMemoryRepo::open(&identity.identity_paths.repo,&identity.identity).map_err(|error|error.to_string())?;
            let head=repo.head().map_err(|error|error.to_string())?.ok_or("memory repository has no commits yet; run /init")?;
            let paths=repo.ls_tree(Some(&head),None).map_err(|error|error.to_string())?;
            let mut lines=vec![format!("# Memory: {}",identity.identity),format!("HEAD: {head}"),format!("Repository: {}",identity.identity_paths.repo.display())];
            for path in paths{if path.starts_with("system/")&&path.ends_with(".md"){lines.push(format!("\n### {path}\n{}",repo.show(&head,&path).map_err(|error|error.to_string())?));}else{lines.push(format!("- {path}"));}}
            let status=repo.status(&[] as &[&str]).map_err(|error|error.to_string())?;if !status.trim().is_empty(){lines.push(format!("\n## Uncommitted changes\n{status}"));}Ok(lines.join("\n"))
        },
        "remember" => {
            if args.trim().is_empty(){return Err("usage: /remember <text>".into());}
            context.wait_for_idle().await;
            deps.actions.send_user_message(maho_ext_api::UserMessageContent::Text(format!("[MEMORY REQUEST] Persist this as long-term memory if appropriate. Use your memory tools: choose the most appropriate memory file (create one if no relevant file exists), avoid duplicates, match the existing formatting of the file, then briefly confirm what you remembered and where you stored it.\n\n{}",args.trim())),Default::default()).map_err(|error|error.to_string())?;
            Ok(format!("memory request sent for {}",identity.identity))
        },
        "init" => {
            let repo=memory_core::git::GitMemoryRepo::open(&identity.identity_paths.repo,&identity.identity).map_err(|error|error.to_string())?;
            if identity.identity_paths.repo.join(".git").exists()&&repo.head().map_err(|error|error.to_string())?.is_some(){return Err(format!("memory already initialized for {}; use /memory to view or /doctor to audit",identity.identity));}
            crate::engine_session::prepare_memory_engine_session(&identity.identity,&identity.identity_paths,Default::default()).map_err(|error|error.to_string())?;
            context.wait_for_idle().await;
            deps.actions.send_user_message(maho_ext_api::UserMessageContent::Text(format!("[MEMORY INITIALIZATION]\nThe user invoked /init. Your memory repository was just initialized at {} and is projected on the local filesystem. Inspect it before writing. Create initial system/persona.md and system/human.md memory with description frontmatter. Store durable, generalizable knowledge, not transient session state. Do not overwrite existing files; extend them.",identity.identity_paths.repo.display())),Default::default()).map_err(|error|error.to_string())?;
            Ok(format!("initialized memory repository at {}; initialization turn sent",identity.identity_paths.repo.display()))
        },
        "recompile" => {deps.prompt.cache.clear();Ok(format!("memory prompt cache cleared for {}; the next agent run recompiles from HEAD (the current run keeps its prompt)",identity.identity))},
        "search" => {
            let settings=(deps.settings)()?;if settings["search"]["enabled"]==false{return Err("memory search is disabled".into());}
            let provider=memory_core::search::senpi_session_provider::SenpiSessionProvider::new(memory_core::search::senpi_session_provider::SenpiSessionProviderOptions{sessions_dir:deps.sessions_dir.clone(),excluded_dirs:None,hidden_marker_file:None,is_hidden:None});
            let results=memory_core::search::search_transcripts(&provider,&positionals.join(" "),&memory_core::search::SearchOptions{conversation_id:flags.get("conversation").and_then(Clone::clone),limit:flags.get("limit").and_then(Option::as_ref).and_then(|value|value.parse().ok()),include_hidden:Some(flags.contains_key("include-hidden")),..Default::default()});
            serde_json::to_string_pretty(&results).map_err(|error|error.to_string())
        },
        "reflect" => {
            if !crate::trigger_wiring::resolve_reflection_trigger_config(&(deps.settings)()?, Some(&identity.identity))?.enabled { return Err("reflection is disabled; set reflection.enabled to true in your omo config to enable it".into()); }
            let recent_n = flags.get("recent").map(|value| value.as_deref().and_then(|value| value.parse::<usize>().ok()).filter(|value| *value > 0).ok_or("--recent expects a positive integer, for example /reflect --recent 5")).transpose()?;
            let (status, run) = (deps.reflect)(session, ReflectionEvent::Manual { focus, recent_n, conversation_ids: conversations })?;
            Ok(if status == "active" { format!("reflection run {run} reserved; it starts at the next idle boundary") } else { format!("reflection request queued as {run}; a run is already active and this request runs next") })
        },
        "dream" => deps.dream.request_manual_dream(crate::dream_trigger_fire::ManualDreamRequest {
            focus, conversation_ids: conversations, target_doc: flags.get("target").and_then(Clone::clone), deadline_at: None,
        }).await.map(|result| format!("{result:?}")),
        "facts" => facts(&identity, &positionals, &flags, deps).await,
        "sleeptime" => {
            let settings = (deps.settings)()?;
            let mut effective = settings.clone();
            for section in ["reflection", "nudge", "facts", "dream", "people", "soul"] {
                if section == "reflection" { effective[section] = crate::reflection_settings::resolve_agent_reflection_settings(Some(&settings), &identity.identity)?; }
                else if let Some(overrides) = settings["agents"][&identity.identity][section].as_object()
                    && let Some(values) = effective[section].as_object_mut() { values.extend(overrides.clone()); }
            }
            serde_json::to_string_pretty(&effective).map_err(|error| error.to_string())
        },
        _ => unreachable!(),
    }
}

async fn facts(identity: &MemoryIdentityContext, positionals: &[String], flags: &std::collections::BTreeMap<String, Option<String>>, deps: &MemoryCommandDeps) -> Result<String, String> {
    let store = memory_core::facts::failures_store::FactsFailureStore::new(memory_core::facts::failures_store::FactsFailureStoreOptions { identity_paths: identity.identity_paths.clone(), now: None, lock_wait_ms: None });
    let failures = store.read_failures().map_err(|error| format!("failure ledger is UNREADABLE: {error}"))?;
    if positionals.first().map(String::as_str) == Some("retry") {
        let conversation_id = flags.get("conversation").and_then(Clone::clone);
        let matching = failures.entries.iter().filter(|entry| conversation_id.as_ref().is_none_or(|id| id == &entry.conversation_id)).count();
        if matching == 0 { return Ok("no failure records to clear; nothing was retried".into()); }
        let removed = store.clear_for_retry(&memory_core::facts::failures_backoff::FactsFailureFilter { conversation_id, end_message_id: None }).map_err(|error| error.to_string())?;
        (deps.facts_retry)(identity.identity.clone()).await?;
        return Ok(format!("cleared {removed} records; one launch attempt was triggered"));
    }
    if !positionals.is_empty() { return Err(format!("unknown /facts subcommand {:?}; use /facts or /facts retry", positionals[0])); }
    let queue = memory_core::facts::queue::FactsQueue::new(memory_core::facts::queue::FactsQueueOptions { identity_paths: identity.identity_paths.clone(), now: None, on_publish: None });
    let pending = queue.list_pending().map_err(|error| error.to_string())?;
    Ok(format!("facts {}: {} queued endpoints\n{}", identity.identity, pending.len(), serde_json::to_string_pretty(&failures).map_err(|error| error.to_string())?))
}
