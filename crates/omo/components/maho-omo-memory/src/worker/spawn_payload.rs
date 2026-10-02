use std::{collections::BTreeMap,path::{Path, PathBuf}};
use memory_core::facts::{FactsPayload,serialize_facts_payload,load_facts_persona};
use super::{run_artifacts::RunAttempt,spawn_types::{FactsSpawnArgs,FactsSpawnPaths},model_preflight::Launcher};
use memory_core::reflection::{ReservedRun, ReflectionTrigger, ReflectionWorktree, load_dream_persona, load_reflection_persona};
use super::spawn_types::{DreamPeoplePolicy, ReflectionSpawnArgs, ReflectionSpawnPaths};

pub struct PrepareReflectionSpawnInput<'a> {
    pub parent_session_file: Option<&'a Path>,
    pub parent_cwd: Option<&'a Path>,
    pub run: &'a ReservedRun,
    pub worktree: &'a ReflectionWorktree,
    pub reflection_sessions_dir: &'a Path,
    pub category: &'a str,
    pub model: &'a str,
    pub thinking: Option<&'a str>,
    pub attempt: Option<u32>,
    pub hard_deadline_at: Option<f64>,
    pub next_attempt: Option<RunAttempt>,
    pub env: BTreeMap<String, String>,
    pub merge_policy: &'a str,
    pub skills_usage_source: &'a Path,
    pub dream_state_source: &'a Path,
    pub people_policy: &'a DreamPeoplePolicy,
    pub launch: Launcher,
    pub now_ms: f64,
}

fn invalid(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message)
}

fn safe_run_id(run_id: &str) -> Result<String, std::io::Error> {
    let basename = run_id.trim().rsplit('/').next().unwrap_or_default();
    let mut safe = String::new();
    let mut replacing = false;
    for c in basename.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            safe.push(c);
            replacing = false;
        } else if !replacing {
            safe.push('-');
            replacing = true;
        }
    }
    let safe = safe.trim_matches('-');
    if matches!(safe, "" | "." | "..") {
        return Err(invalid("runId must contain a safe identifier"));
    }
    Ok(safe.chars().take(80).collect())
}

fn resolve_dream_target(worktree: &Path, target: &str) -> Result<PathBuf, std::io::Error> {
    use std::path::Component;
    let bytes = target.as_bytes();
    if Path::new(target).is_absolute()
        || (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'/' | b'\\'))
        || !target.ends_with(".md")
    {
        return Err(invalid("dream target must be a memory-repo-relative .md document"));
    }
    let mut relative = PathBuf::new();
    for component in Path::new(target).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !relative.pop() {
                    return Err(invalid("dream target escapes the memory worktree"));
                }
            }
            Component::Normal(name) => relative.push(name),
            Component::RootDir | Component::Prefix(_) => return Err(invalid("dream target escapes the memory worktree")),
        }
    }
    if relative.as_os_str().is_empty() || relative.components().any(|c| c.as_os_str() == ".git") {
        return Err(invalid("dream target escapes the memory worktree"));
    }
    Ok(worktree.join(relative))
}

fn task_prompt(run: &ReservedRun, worktree: &Path, transcript: &Path) -> String {
    let mut lines = vec!["# Reflection mechanics".into(), format!("MEMORY_DIR={}", worktree.display()),
        format!("TRANSCRIPT_PATH={}", transcript.display()),
        "Read the transcript payload, update only files under MEMORY_DIR, and commit every intended memory change.".into()];
    if let Some(target) = &run.request.target_doc {
        lines.extend([format!("Document maintenance target: {target}"), "Modify no memory document except this target.".into()]);
    }
    lines.push("Do not modify Git administration files. Finish with a clean worktree.".into());
    let trigger = match run.request.trigger {
        ReflectionTrigger::StepCount => "step-count", ReflectionTrigger::Compaction => "compaction",
        ReflectionTrigger::Manual => "manual", ReflectionTrigger::Dream => "dream",
    };
    let focus = run.request.focus.as_deref().filter(|s| !s.is_empty()).map(|s| format!("\nFocus: {s}")).unwrap_or_default();
    lines.push(format!("Trigger: {trigger}{focus}"));
    lines.join("\n")
}

pub fn prepare_reflection_spawn(input: PrepareReflectionSpawnInput<'_>) -> Result<ReflectionSpawnArgs, std::io::Error> {
    let session_dir = input.reflection_sessions_dir.join(safe_run_id(&input.run.run_id)?);
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)] { use std::os::unix::fs::DirBuilderExt; builder.mode(0o700); }
    builder.create(&session_dir)?;
    let is_dream = input.run.request.trigger == ReflectionTrigger::Dream;
    let transcript = session_dir.join("transcript-payload.json");
    let persona = session_dir.join("reflection-persona.md");
    let prompt = session_dir.join("reflection-task.md");
    let skills_usage = is_dream.then(|| session_dir.join("skills-usage.json"));
    let dream_state = is_dream.then(|| session_dir.join("dream-state.json"));
    let dream_policy = is_dream.then(|| session_dir.join("dream-policy.json"));
    let payload_paths: Vec<&Path> = [&transcript, &persona, &prompt].into_iter().map(PathBuf::as_path)
        .chain(skills_usage.as_deref()).chain(dream_state.as_deref()).chain(dream_policy.as_deref()).collect();
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        for path in &payload_paths {
            match std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
                Ok(()) => {}, Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}, Err(e) => return Err(e),
            }
        }
    }
    std::fs::write(&transcript, format!("{}\n", serde_json::to_string_pretty(&serde_json::json!({"schemaVersion":1,"runId":input.run.run_id,"request":input.run.request}))?))?;
    std::fs::write(&persona, if is_dream { load_dream_persona().markdown } else { load_reflection_persona().markdown })?;
    std::fs::write(&prompt, task_prompt(input.run, &input.worktree.dir, &transcript))?;
    if let (Some(skills), Some(state), Some(policy)) = (&skills_usage, &dream_state, &dream_policy) {
        for (source, destination) in [(input.skills_usage_source, skills), (input.dream_state_source, state)] {
            let content = match std::fs::read_to_string(source) {
                Ok(content) => content, Err(e) if e.kind() == std::io::ErrorKind::NotFound => "{}\n".into(), Err(e) => return Err(e),
            };
            std::fs::write(destination, content)?;
        }
        std::fs::write(policy, format!("{}\n", serde_json::to_string_pretty(&serde_json::json!({"version":1,"people":{"enabled":input.people_policy.enabled,"max_entries":input.people_policy.max_entries,"max_entry_chars":input.people_policy.max_entry_chars}}))?))?;
    }
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        for path in &payload_paths { std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o400))?; }
    }
    let dream_target = if is_dream { input.run.request.target_doc.as_deref().map(|target| resolve_dream_target(&input.worktree.dir, target)).transpose()? } else { None };
    let mut env = input.env;
    for (key, value) in [("MEMORY_DIR", &input.worktree.dir), ("TRANSCRIPT_PATH", &transcript)] { env.insert(key.into(), value.to_string_lossy().into_owned()); }
    for (key, value) in [("SKILLS_USAGE_PATH", &skills_usage), ("DREAM_STATE_PATH", &dream_state), ("DREAM_POLICY_PATH", &dream_policy), ("DREAM_TARGET_PATH", &dream_target)] {
        if let Some(value) = value { env.insert(key.into(), value.to_string_lossy().into_owned()); }
    }
    env.insert("SENPI_MEMORY_REFLECTION".into(), "1".into());
    env.insert("SENPI_PTY_FORCE_PIPE".into(), "1".into());
    let mut args = input.launch.prefix_args;
    args.extend(["-p".into(), "--system-prompt".into(), persona.to_string_lossy().into_owned(), "--tools".into(), "bash,edit".into(), "--no-extensions".into(), "--no-skills".into(), "--no-prompt-templates".into(), "--no-context-files".into(), "--session-dir".into(), session_dir.to_string_lossy().into_owned(), "--model".into(), input.model.into()]);
    if let Some(thinking) = input.thinking { args.extend(["--thinking".into(), thinking.into()]); }
    args.push(format!("@{}", prompt.display()));
    let trigger = match input.run.request.trigger { ReflectionTrigger::StepCount => "step-count", ReflectionTrigger::Compaction => "compaction", ReflectionTrigger::Manual => "manual", ReflectionTrigger::Dream => "dream" };
    let origin = if is_dream { input.run.request.origin.map(|origin| match origin { memory_core::reflection::DreamOrigin::Manual => "manual", memory_core::reflection::DreamOrigin::Idle => "idle", memory_core::reflection::DreamOrigin::Shutdown => "shutdown" }.into()) } else { None };
    Ok(ReflectionSpawnArgs { parent_session_file: None, run_id: Some(input.run.run_id.clone()), attempt: input.attempt.unwrap_or(1), hard_deadline_at: input.hard_deadline_at.unwrap_or(input.now_ms + 900_000.0), category: input.category.into(), conversation_ids: input.run.request.conversation_ids.clone(), model: input.model.into(), thinking: input.thinking.map(str::to_owned), next_attempt: input.next_attempt, kind: Some(if is_dream { "dream" } else { "reflection" }.into()), trigger: Some(trigger.into()), origin, merge_policy: Some(input.merge_policy.into()), target_doc: input.run.request.target_doc.clone(), worktree: Some(input.worktree.clone()), command: input.launch.command, args, cwd: input.worktree.dir.clone(), env, paths: ReflectionSpawnPaths { session_dir, worktree: input.worktree.dir.clone(), git_common_dir: input.worktree.common_config_path.parent().unwrap_or_else(|| Path::new(".")).into(), transcript, persona, prompt, skills_usage, dream_state, dream_policy, dream_target } })
}

pub fn prepare_reflection_fork_spawn(input: PrepareReflectionSpawnInput<'_>) -> Result<ReflectionSpawnArgs, std::io::Error> {
    let parent_session_file = input.parent_session_file.map(Path::to_path_buf);
    let parent_cwd = input.parent_cwd.map(Path::to_path_buf);
    let mut base = prepare_reflection_spawn(input)?;
    let parent = parent_session_file.ok_or_else(|| invalid("fork-mode reflection requires the parent session file"))?;
    let mut args = vec!["-p".into(), "--fork".into(), parent.to_string_lossy().into_owned(), "--session-dir".into(), base.paths.session_dir.to_string_lossy().into_owned(), "--model".into(), base.model.clone()];
    if let Some(thinking) = &base.thinking { args.extend(["--thinking".into(), thinking.clone()]); }
    args.push(format!("@{}", base.paths.prompt.display()));
    base.parent_session_file = Some(parent);
    base.args = args;
    if let Some(cwd) = parent_cwd { base.cwd = cwd; }
    Ok(base)
}
pub struct PrepareFactsSpawnInput<'a>{pub run_id:&'a str,pub run_dir:&'a Path,pub payload:&'a FactsPayload,pub model:&'a str,pub thinking:Option<&'a str>,pub attempt:Option<u32>,pub hard_deadline_at:Option<f64>,pub next_attempt:Option<RunAttempt>,pub env:BTreeMap<String,String>,pub launch:Launcher,pub now_ms:f64}
pub fn prepare_facts_spawn(input:PrepareFactsSpawnInput<'_>)->Result<FactsSpawnArgs,std::io::Error>{
    let mut builder=std::fs::DirBuilder::new();builder.recursive(true);
    #[cfg(unix)]{use std::os::unix::fs::DirBuilderExt;builder.mode(0o700);}
    builder.create(input.run_dir)?;
    let payload=input.run_dir.join("facts-payload.json");let extraction=input.run_dir.join("extraction.jsonl");
    #[cfg(unix)]{use std::os::unix::fs::PermissionsExt;match std::fs::set_permissions(&payload,std::fs::Permissions::from_mode(0o600)){Ok(())=>{},Err(e)if e.kind()==std::io::ErrorKind::NotFound=>{},Err(e)=>return Err(e)}}
    std::fs::write(&payload,serialize_facts_payload(input.payload))?;
    #[cfg(unix)]{use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(&payload,std::fs::Permissions::from_mode(0o400))?;}
    let mut env=input.env;env.insert("FACTS_PAYLOAD_PATH".into(),payload.to_string_lossy().into_owned());env.insert("FACTS_EXTRACTION_PATH".into(),extraction.to_string_lossy().into_owned());env.insert("SENPI_MEMORY_FACTS".into(),"1".into());env.insert("SENPI_PTY_FORCE_PIPE".into(),"1".into());
    let mut args=input.launch.prefix_args;args.extend(["-p".into(),"--system-prompt".into(),load_facts_persona().into(),"--tools".into(),"read,write".into(),"--no-extensions".into(),"--no-skills".into(),"--no-prompt-templates".into(),"--no-context-files".into(),"--session-dir".into(),input.run_dir.to_string_lossy().into_owned(),"--model".into(),input.model.into()]);
    if let Some(thinking)=input.thinking{args.extend(["--thinking".into(),thinking.into()]);}
    args.push(format!("Read {} and write only {} according to the system prompt.",payload.display(),extraction.display()));
    Ok(FactsSpawnArgs{run_id:input.run_id.into(),attempt:input.attempt.unwrap_or(1),hard_deadline_at:input.hard_deadline_at.unwrap_or(input.now_ms+900_000.0),model:input.model.into(),thinking:input.thinking.map(str::to_owned),next_attempt:input.next_attempt,command:input.launch.command,args,cwd:input.run_dir.into(),env,paths:FactsSpawnPaths{run_dir:input.run_dir.into(),payload,extraction}})
}
#[cfg(test)]mod tests{
    use super::*;
    fn reflection_fixture(root: &Path) -> (ReservedRun, ReflectionWorktree, DreamPeoplePolicy) {
        let paths = memory_core::identity::layout::build_identity_paths(root, "agent");
        let engine = crate::engine_session::prepare_memory_engine_session("agent", &paths, Default::default()).unwrap();
        let worktree = ReflectionWorktree { parent: engine.repo, dir: root.join("worktree"), branch: "memory/run".into(), base_commit_sha: "sha".into(), git_file_path: root.join("worktree/.git"), git_file_snapshot: "gitdir: missing".into(), common_config_path: root.join("repo/.git/config"), common_config_snapshot: None, exec: memory_core::git::exec::create_git_exec(Default::default()) };
        let run = ReservedRun { run_id: "run".into(), request: memory_core::reflection::ReflectionRequest { trigger: ReflectionTrigger::Manual, origin: None, conversation_ids: vec!["conversation".into()], snapshots: vec![], focus: None, recent_n: None, target_doc: None }, reserved_at: None, launcher_pid: None, launcher_hostname: None, launcher_process_start: None };
        (run, worktree, DreamPeoplePolicy { enabled: true, max_entries: 3, max_entry_chars: 500 })
    }
    fn reflection_input<'a>(root: &'a Path, run: &'a ReservedRun, worktree: &'a ReflectionWorktree, policy: &'a DreamPeoplePolicy) -> PrepareReflectionSpawnInput<'a> {
        PrepareReflectionSpawnInput { parent_session_file: None, parent_cwd: None, run, worktree, reflection_sessions_dir: root, category: "quick", model: "p/m", thinking: Some("low"), attempt: None, hard_deadline_at: None, next_attempt: None, env: Default::default(), merge_policy: "auto", skills_usage_source: root, dream_state_source: root, people_policy: policy, launch: Launcher { command: "mhc".into(), prefix_args: vec!["prefix".into()] }, now_ms: 42.0 }
    }
    #[test]
    fn reflection_payload_metadata_and_retry() {
        let root = tempfile::tempdir().unwrap();
        let (mut run, worktree, policy) = reflection_fixture(root.path());
        let first = prepare_reflection_spawn(reflection_input(root.path(), &run, &worktree, &policy)).unwrap();
        assert_eq!(first.args[0], "prefix");
        assert_eq!(first.cwd, worktree.dir);
        assert_eq!(first.hard_deadline_at, 900042.0);
        assert_eq!(first.env["MEMORY_DIR"], worktree.dir.to_string_lossy());
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&first.paths.transcript).unwrap()).unwrap();
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["request"]["conversationIds"][0], "conversation");
        run.request.conversation_ids.push("second".into());
        let second = prepare_reflection_spawn(reflection_input(root.path(), &run, &worktree, &policy)).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&second.paths.transcript).unwrap()).unwrap();
        assert_eq!(value["request"]["conversationIds"].as_array().unwrap().len(), 2);
        assert!(second.paths.skills_usage.is_none());
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; assert_eq!(std::fs::metadata(second.paths.transcript).unwrap().permissions().mode() & 0o777, 0o400); }
    }
    #[test]
    fn dream_copies_source_bytes_and_defaults_missing_json() {
        let root = tempfile::tempdir().unwrap();
        let (mut run, worktree, policy) = reflection_fixture(root.path());
        run.request.trigger = ReflectionTrigger::Dream;
        run.request.origin = Some(memory_core::reflection::DreamOrigin::Idle);
        run.request.target_doc = Some("reference/../system/persona.md".into());
        let skills = root.path().join("source-skills.json");
        std::fs::write(&skills, "{\"raw\":true}\n").unwrap();
        let missing = root.path().join("missing.json");
        let mut input = reflection_input(root.path(), &run, &worktree, &policy);
        input.skills_usage_source = &skills;
        input.dream_state_source = &missing;
        let args = prepare_reflection_spawn(input).unwrap();
        assert_eq!(std::fs::read_to_string(args.paths.skills_usage.unwrap()).unwrap(), "{\"raw\":true}\n");
        assert_eq!(std::fs::read_to_string(args.paths.dream_state.unwrap()).unwrap(), "{}\n");
        let policy_json: serde_json::Value = serde_json::from_slice(&std::fs::read(args.paths.dream_policy.unwrap()).unwrap()).unwrap();
        assert_eq!(policy_json["people"]["max_entries"], 3);
        assert_eq!(args.paths.dream_target.unwrap(), worktree.dir.join("system/persona.md"));
        assert_eq!(args.origin.as_deref(), Some("idle"));
        assert_eq!(args.env["SENPI_MEMORY_REFLECTION"], "1");
    }
    #[test]
    fn fork_preserves_parent_prefix_identity() {
        let root = tempfile::tempdir().unwrap();
        let (run, worktree, policy) = reflection_fixture(root.path());
        let session = root.path().join("parent.jsonl");
        let mut input = reflection_input(root.path(), &run, &worktree, &policy);
        input.parent_session_file = Some(&session);
        input.parent_cwd = Some(root.path());
        let args = prepare_reflection_fork_spawn(input).unwrap();
        assert_eq!(args.cwd, root.path());
        assert_eq!(args.parent_session_file.as_deref(), Some(session.as_path()));
        assert_eq!(&args.args[..2], &["-p", "--fork"]);
        assert!(!args.args.iter().any(|arg| arg == "prefix" || arg == "--tools" || arg.starts_with("--no-")));
        assert!(prepare_reflection_fork_spawn(reflection_input(root.path(), &run, &worktree, &policy)).is_err());
    }
    #[test]
    fn dream_target_and_run_id_boundaries() {
        for target in ["/absolute.md", "C:\\escape.md", "../escape.md", "notes.txt", ".git/config.md"] {
            assert!(resolve_dream_target(Path::new("/work"), target).is_err(), "{target}");
        }
        assert_eq!(resolve_dream_target(Path::new("/work"), "x/../notes.md").unwrap(), Path::new("/work/notes.md"));
        assert_eq!(safe_run_id(" /parent/run name ").unwrap(), "run-name");
        assert_eq!(safe_run_id(&"a".repeat(100)).unwrap().len(), 80);
        for id in ["", "..", ".", "---", "☃"] { assert!(safe_run_id(id).is_err()); }
    }
    fn payload()->FactsPayload{FactsPayload{version:1,identity:"agent".into(),today:"2026-10-02".into(),known_people:vec![],primary_human:memory_core::facts::FactsPrimaryHuman{slug:"human".into(),aliases:vec![]},entries:vec![]}}
    #[test]fn shared_serializer_and_launch_flags(){let root=tempfile::tempdir().unwrap();let payload=payload();let args=prepare_facts_spawn(PrepareFactsSpawnInput{run_id:"facts",run_dir:root.path(),payload:&payload,model:"p/m",thinking:Some("low"),attempt:None,hard_deadline_at:None,next_attempt:None,env:BTreeMap::from([("KEEP".into(),"yes".into())]),launch:Launcher{command:"maho".into(),prefix_args:vec!["prefix".into()]},now_ms:42.0}).unwrap();assert_eq!(std::fs::read_to_string(&args.paths.payload).unwrap(),serialize_facts_payload(&payload));assert_eq!(args.args[0],"prefix");assert_eq!(args.attempt,1);assert_eq!(args.hard_deadline_at,900042.0);assert_eq!(args.env["KEEP"],"yes");assert_eq!(args.env["SENPI_PTY_FORCE_PIPE"],"1");assert!(args.args.windows(2).any(|a|a==["--tools","read,write"]));assert!(!args.paths.extraction.exists());}
    #[test]fn retry_rewrites_readonly_payload(){let root=tempfile::tempdir().unwrap();let mut payload=payload();for id in ["first","second"]{payload.identity=id.into();let args=prepare_facts_spawn(PrepareFactsSpawnInput{run_id:"facts",run_dir:root.path(),payload:&payload,model:"p/m",thinking:None,attempt:Some(2),hard_deadline_at:Some(123.0),next_attempt:None,env:Default::default(),launch:Launcher{command:"maho".into(),prefix_args:vec![]},now_ms:0.0}).unwrap();assert_eq!(std::fs::read_to_string(&args.paths.payload).unwrap(),serialize_facts_payload(&payload));assert_eq!(args.hard_deadline_at,123.0);
        #[cfg(unix)]{use std::os::unix::fs::PermissionsExt;assert_eq!(std::fs::metadata(&args.paths.payload).unwrap().permissions().mode()&0o777,0o400);}
    }}
}
