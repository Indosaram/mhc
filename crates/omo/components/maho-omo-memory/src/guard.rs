use crate::{
    context::MemoryIdentityContext,
    policy_guard::{canonicalize_from_nearest_existing, resolve_path, stable_roots, within},
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
#[derive(Debug, PartialEq, Eq)]
pub enum MemoryGuardDecision {
    Allow,
    Block { reason: String },
    AdviseBash,
}
#[derive(Default)]
pub struct MemoryGuard {
    warned_bash_sessions: BTreeSet<String>,
}
pub type GuardWarning = std::sync::Arc<dyn Fn(&str) + Send + Sync>;
pub fn register_memory_guard(
    api: &mut maho_ext_api::ExtensionApi,
    resolve: crate::prompt::PromptContextResolver,
    resolve_cwd: std::sync::Arc<dyn Fn() -> std::path::PathBuf + Send + Sync>,
    captured_tools: std::sync::Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    warn: GuardWarning,
) {
    api.on(maho_ext_api::EventKind::ToolResult, std::sync::Arc::new(|event, _context| {
        Box::pin(async move {
            if let maho_ext_api::ExtensionEvent::ToolResult(result) = event {
                let payload = serde_json::json!({"type":"tool_result", "toolName":result.tool_name, "isError":result.is_error, "content":result.content});
                if crate::receipt_read_guard::unverified_receipt_notice(&payload).is_some() {
                    result.content.push(maho_ext_api::ToolContent::text(crate::receipt_read_guard::UNVERIFIED_RECEIPT_NOTICE));
                }
            }
            Ok(maho_ext_api::EventResult::None)
        })
    }));
    let guard = std::sync::Arc::new(std::sync::Mutex::new(MemoryGuard::default()));
    api.on(maho_ext_api::EventKind::ToolCall,std::sync::Arc::new(move |event,context| {
        let result=if let maho_ext_api::ExtensionEvent::ToolCall(call)=event {
            let id=context.session_manager.session_id();
            guard.lock().unwrap_or_else(std::sync::PoisonError::into_inner).check(resolve(id).as_ref(),&resolve_cwd(),&call.tool_name,&call.input,Some(id),&captured_tools())
        } else {Ok(MemoryGuardDecision::Allow)};
        let warn=warn.clone();
        Box::pin(async move {
            Ok(match result.map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))? {
                MemoryGuardDecision::Allow=>maho_ext_api::EventResult::None,
                MemoryGuardDecision::Block{reason}=>maho_ext_api::EventResult::ToolCall(maho_ext_api::ToolCallEventResult{block:Some(true),reason:Some(reason),terminate:None}),
                MemoryGuardDecision::AdviseBash=>{warn("omo-senpi memory guard advisory: bash command references another memory identity; shell text is not blocked because this soft guard is not a security boundary");maho_ext_api::EventResult::None},
            })
        })
    }));
}
fn normalized(name: &str) -> String {
    name.trim().to_lowercase().replace('-', "_")
}
fn matches_tool(name: &str, expected: &str) -> bool {
    let name = normalized(name);
    name == expected
        || ["_", ":", "/"]
            .iter()
            .any(|separator| name.ends_with(&format!("{separator}{expected}")))
}
fn target_paths(input: &serde_json::Value, patch: bool) -> Vec<String> {
    let mut paths = Vec::new();
    for key in ["path", "filePath", "file_path", "target"] {
        if let Some(value) = input
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
        {
            paths.push(value.to_owned());
        }
    }
    if let Some(values) = input.get("paths").and_then(serde_json::Value::as_array) {
        paths.extend(
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        );
    }
    if patch && let Some(patch) = input.get("input").and_then(serde_json::Value::as_str) {
        for prefixes in [
            &["*** Add File:", "*** Update File:", "*** Delete File:"][..],
            &["*** Move to:"][..],
        ] {
            for line in patch.lines() {
                for prefix in prefixes {
                    if let Some(path) = line
                        .strip_prefix(prefix)
                        .map(str::trim)
                        .filter(|path| !path.is_empty())
                    {
                        paths.push(path.to_owned());
                    }
                }
            }
        }
    }
    paths
}
impl MemoryGuard {
    pub fn check(
        &mut self,
        context: Option<&MemoryIdentityContext>,
        cwd: &Path,
        tool_name: &str,
        input: &serde_json::Value,
        session_id: Option<&str>,
        captured_tools: &[String],
    ) -> std::io::Result<MemoryGuardDecision> {
        let Some(context) = context else {
            return Ok(MemoryGuardDecision::Allow);
        };
        if !input.is_object() {
            return Ok(MemoryGuardDecision::Allow);
        }
        // Structural, not enumerated (issue #7765): the verdict is "inside a memory area but outside
        // your own root", so no directory is listed and identities not yet on disk are denied too.
        // Both areas are denied because a transient run's own root lives under `transient-runs/`.
        let own = resolve_path(&context.identity_paths.root)?;
        let durable_agents = resolve_path(&context.durable_root)?
            .parent()
            .unwrap_or(Path::new("/"))
            .to_path_buf();
        let memory_root = durable_agents
            .parent()
            .unwrap_or(Path::new("/"))
            .to_path_buf();
        let mut denied = Vec::new();
        for root in [
            own.parent().unwrap_or(Path::new("/")).to_path_buf(),
            durable_agents,
            memory_root.join(crate::transient_identity::TRANSIENT_DIRNAME),
        ] {
            if !denied.contains(&root) {
                denied.push(root);
            }
        }
        if matches_tool(tool_name, "bash") {
            if let Some(command) = input.get("command").and_then(serde_json::Value::as_str) {
                let session = session_id
                    .filter(|id| !id.is_empty())
                    .unwrap_or("unknown-session");
                if denied
                    .iter()
                    .any(|root| command.contains(root.to_string_lossy().as_ref()))
                    && self.warned_bash_sessions.insert(session.into())
                {
                    return Ok(MemoryGuardDecision::AdviseBash);
                }
            }
            return Ok(MemoryGuardDecision::Allow);
        }
        let operation = ["read", "write", "edit", "ls", "find", "grep", "glob"]
            .into_iter()
            .find(|name| matches_tool(tool_name, name));
        let registered_patch = captured_tools.iter().any(|name| {
            name.eq_ignore_ascii_case(tool_name)
                && normalized(name)
                    .split(|character: char| !character.is_ascii_alphanumeric())
                    .any(|part| part == "patch")
        });
        let patch =
            operation.is_none() && (matches_tool(tool_name, "apply_patch") || registered_patch);
        if operation.is_none() && !patch {
            return Ok(MemoryGuardDecision::Allow);
        }
        let recursive = matches!(operation, Some("ls" | "find" | "grep" | "glob"));
        let denied_roots = stable_roots(&denied);
        let own_roots = stable_roots(&[own]);
        for raw in target_paths(input, patch) {
            if raw.contains('\0') {
                continue;
            }
            // Built-ins open resolved targets; suffix-classified external tools may open raw strings.
            let name = normalized(tool_name);
            let builtin = Some(name.as_str()) == operation || name == "apply_patch";
            let target = if builtin {
                resolve_path(&cwd.join(&raw))?
            } else if Path::new(&raw).is_absolute() {
                PathBuf::from(&raw)
            } else {
                PathBuf::from(format!("{}/{}", cwd.display(), raw))
            };
            if operation == Some("read")
                && crate::project_memory_grants::is_granted_project_memory_read(context, &target)
            {
                continue;
            }
            let lexical = resolve_path(&target)?;
            let mut candidates = vec![lexical];
            if let Some(canonical) = canonicalize_from_nearest_existing(&target)
                && !candidates.contains(&canonical)
            {
                candidates.push(canonical);
            }
            for candidate in candidates {
                if own_roots.iter().any(|root| within(root, &candidate)) {
                    continue;
                }
                if denied_roots.iter().any(|root| within(root, &candidate))
                    || (recursive && denied_roots.iter().any(|root| within(&candidate, root)))
                {
                    return Ok(MemoryGuardDecision::Block {
                        reason: format!(
                            "cross-identity memory access denied: {tool_name} to {raw} belongs to another memory identity"
                        ),
                    });
                }
            }
        }
        Ok(MemoryGuardDecision::Allow)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    fn fixture() -> (tempfile::TempDir, MemoryIdentityContext, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(dir.path(), "own");
        let foreign = paths.root.parent().unwrap().join("foreign/repo");
        std::fs::create_dir_all(&foreign).unwrap();
        let context = MemoryIdentityContext::new(
            "own".into(),
            paths,
            crate::binding::MemorySessionBinding {
                identity: "own".into(),
                repo_path_hash: "hash".into(),
                bound_at: 1.0,
            },
        );
        (dir, context, foreign)
    }
    #[test]
    fn granted_project_notes_allow_only_reads() {
        let (dir, context, foreign) = fixture();
        let note = foreign.join("notes/fixture.md");
        std::fs::create_dir_all(note.parent().unwrap()).unwrap();
        std::fs::write(&note, "fixture").unwrap();
        crate::project_memory_grants::grant_project_memory_read(&context, &foreign);
        let mut guard = MemoryGuard::default();
        assert_eq!(
            guard
                .check(
                    Some(&context),
                    dir.path(),
                    "read",
                    &serde_json::json!({"path":note}),
                    None,
                    &[]
                )
                .unwrap(),
            MemoryGuardDecision::Allow
        );
        for tool in ["write", "edit", "grep", "glob", "ls"] {
            assert!(matches!(
                guard
                    .check(
                        Some(&context),
                        dir.path(),
                        tool,
                        &serde_json::json!({"path":note}),
                        None,
                        &[]
                    )
                    .unwrap(),
                MemoryGuardDecision::Block { .. }
            ));
        }
        crate::project_memory_grants::revoke_project_memory_reads(&context);
    }
    #[test]
    fn foreign_read_has_stable_reason() {
        let (dir, context, foreign) = fixture();
        let raw = foreign.join("persona.md");
        assert_eq!(
            MemoryGuard::default()
                .check(
                    Some(&context),
                    dir.path(),
                    "read",
                    &serde_json::json!({"path":raw}),
                    None,
                    &[]
                )
                .unwrap(),
            MemoryGuardDecision::Block {
                reason: format!(
                    "cross-identity memory access denied: read to {} belongs to another memory identity",
                    raw.display()
                )
            }
        );
    }
    #[test]
    fn own_worktree_edit_is_allowed() {
        let (dir, context, _) = fixture();
        assert_eq!(MemoryGuard::default().check(Some(&context),dir.path(),"edit",&serde_json::json!({"file_path":context.identity_paths.worktrees.join("reflection/notes.md")}),None,&[]).unwrap(),MemoryGuardDecision::Allow);
    }
    #[test]
    fn unbound_foreign_read_is_allowed() {
        let (dir, _, foreign) = fixture();
        assert_eq!(
            MemoryGuard::default()
                .check(
                    None,
                    dir.path(),
                    "read",
                    &serde_json::json!({"path":foreign}),
                    None,
                    &[]
                )
                .unwrap(),
            MemoryGuardDecision::Allow
        );
    }
    #[test]
    fn relative_traversal_from_workspace_is_blocked() {
        let (dir, context, foreign) = fixture();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let relative = format!("../{}", foreign.strip_prefix(dir.path()).unwrap().display());
        assert!(matches!(
            MemoryGuard::default()
                .check(
                    Some(&context),
                    &workspace,
                    "vendor_READ",
                    &serde_json::json!({"target":relative}),
                    None,
                    &[]
                )
                .unwrap(),
            MemoryGuardDecision::Block { .. }
        ));
    }
    #[test]
    fn agents_root_and_ancestors_deny_enumeration() {
        let (dir, context, _) = fixture();
        let agents = context.identity_paths.root.parent().unwrap();
        for tool in ["ls", "find", "grep", "glob"] {
            for path in [agents, dir.path()] {
                assert!(matches!(
                    MemoryGuard::default()
                        .check(
                            Some(&context),
                            dir.path(),
                            tool,
                            &serde_json::json!({"path":path}),
                            None,
                            &[]
                        )
                        .unwrap(),
                    MemoryGuardDecision::Block { .. }
                ));
            }
        }
    }
    #[test]
    fn captured_patch_tool_blocks_foreign_move() {
        let (dir, context, foreign) = fixture();
        let tool = "custom_patch_tool";
        assert!(matches!(MemoryGuard::default().check(Some(&context),dir.path(),tool,&serde_json::json!({"input":format!("*** Begin Patch\n*** Update File: own.md\n*** Move to: {}/new.md\n*** End Patch",foreign.display())}),None,&[tool.into()]).unwrap(),MemoryGuardDecision::Block{..}));
    }
    #[test]
    fn file_paths_and_patch_directives_block() {
        let (dir, context, foreign) = fixture();
        let mut guard = MemoryGuard::default();
        for (tool, input) in [
            (
                "vendor_READ",
                serde_json::json!({"target":foreign.join("a.md")}),
            ),
            (
                "glob",
                serde_json::json!({"paths":[context.identity_paths.repo,foreign]}),
            ),
            (
                "vendor_APPLY_PATCH",
                serde_json::json!({"input":format!("*** Begin Patch\n*** Move to: {}/new.md\n*** End Patch",foreign.display())}),
            ),
        ] {
            assert!(matches!(
                guard
                    .check(Some(&context), dir.path(), tool, &input, None, &[])
                    .unwrap(),
                MemoryGuardDecision::Block { .. }
            ));
        }
    }
    #[test]
    fn own_unbound_unknown_and_no_path_allowed() {
        let (dir, context, foreign) = fixture();
        let mut guard = MemoryGuard::default();
        for (bound, tool, input) in [
            (
                Some(&context),
                "write",
                serde_json::json!({"path":context.identity_paths.repo}),
            ),
            (None, "read", serde_json::json!({"path":foreign})),
            (Some(&context), "query", serde_json::json!({"path":foreign})),
            (Some(&context), "read", serde_json::json!({"offset":1})),
        ] {
            assert_eq!(
                guard
                    .check(bound, dir.path(), tool, &input, None, &[])
                    .unwrap(),
                MemoryGuardDecision::Allow
            );
        }
    }
    #[test]
    fn enumeration_ancestors_block_but_reads_allowed() {
        let (dir, context, _) = fixture();
        let mut guard = MemoryGuard::default();
        let input = serde_json::json!({"path":dir.path()});
        assert!(matches!(
            guard
                .check(Some(&context), dir.path(), "grep", &input, None, &[])
                .unwrap(),
            MemoryGuardDecision::Block { .. }
        ));
        assert_eq!(
            guard
                .check(Some(&context), dir.path(), "read", &input, None, &[])
                .unwrap(),
            MemoryGuardDecision::Allow
        );
    }
    #[test]
    fn bash_advisory_once_per_session() {
        let (dir, context, foreign) = fixture();
        let mut guard = MemoryGuard::default();
        let input = serde_json::json!({"command":format!("ls {}",foreign.display())});
        for (session, result) in [
            ("a", MemoryGuardDecision::AdviseBash),
            ("a", MemoryGuardDecision::Allow),
            ("b", MemoryGuardDecision::AdviseBash),
        ] {
            assert_eq!(
                guard
                    .check(
                        Some(&context),
                        dir.path(),
                        "bash",
                        &input,
                        Some(session),
                        &[]
                    )
                    .unwrap(),
                result
            );
        }
    }
    #[test]
    fn an_identity_not_on_disk_is_blocked_without_enumerating_the_agents_root() {
        let (dir, context, _) = fixture();
        let ghost = context
            .identity_paths
            .root
            .parent()
            .unwrap()
            .join("never-created/repo/system/persona.md");
        assert_eq!(
            MemoryGuard::default()
                .check(
                    Some(&context),
                    dir.path(),
                    "read",
                    &serde_json::json!({"path":ghost}),
                    None,
                    &[]
                )
                .unwrap(),
            MemoryGuardDecision::Block {
                reason: format!(
                    "cross-identity memory access denied: read to {} belongs to another memory identity",
                    ghost.display()
                )
            }
        );
    }
    #[test]
    fn a_transient_run_reaches_only_its_own_transient_root() {
        let dir = tempfile::tempdir().unwrap();
        let durable = memory_core::identity::layout::build_identity_paths(dir.path(), "own");
        let foreign = memory_core::identity::layout::build_identity_paths(dir.path(), "foreign");
        let run = memory_core::identity::layout::build_identity_paths(
            &dir.path()
                .join(crate::transient_identity::TRANSIENT_DIRNAME)
                .join("token-1"),
            "own",
        );
        let other = memory_core::identity::layout::build_identity_paths(
            &dir.path()
                .join(crate::transient_identity::TRANSIENT_DIRNAME)
                .join("token-2"),
            "other",
        );
        let durable_foreign_file = foreign.repo.join("system/persona.md");
        let other_run_file = other.transcripts.join("stream/transcript.jsonl");
        for file in [
            &durable_foreign_file,
            &other_run_file,
            &run.repo.join("system/persona.md"),
        ] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "fixture").unwrap();
        }
        let context = MemoryIdentityContext::new(
            "own".into(),
            run.clone(),
            crate::binding::MemorySessionBinding {
                identity: "own".into(),
                repo_path_hash: "hash".into(),
                bound_at: 1.0,
            },
        )
        .with_durable_root(durable.root);
        let mut guard = MemoryGuard::default();
        assert_eq!(
            guard
                .check(
                    Some(&context),
                    dir.path(),
                    "write",
                    &serde_json::json!({"path":run.transcripts.join("stream/transcript.jsonl")}),
                    None,
                    &[]
                )
                .unwrap(),
            MemoryGuardDecision::Allow
        );
        for file in [durable_foreign_file, other_run_file] {
            assert_eq!(
                guard
                    .check(
                        Some(&context),
                        dir.path(),
                        "read",
                        &serde_json::json!({"path":file}),
                        None,
                        &[]
                    )
                    .unwrap(),
                MemoryGuardDecision::Block {
                    reason: format!(
                        "cross-identity memory access denied: read to {} belongs to another memory identity",
                        file.display()
                    )
                }
            );
        }
    }
    #[cfg(unix)]
    #[test]
    fn missing_symlink_descendant_blocked() {
        let (dir, context, foreign) = fixture();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(foreign, &alias).unwrap();
        assert!(matches!(
            MemoryGuard::default()
                .check(
                    Some(&context),
                    dir.path(),
                    "write",
                    &serde_json::json!({"path":alias.join("new/file.md")}),
                    None,
                    &[]
                )
                .unwrap(),
            MemoryGuardDecision::Block { .. }
        ));
    }
}
