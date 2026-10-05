//! `/memory` — committed memory tree and file viewer.
//! Port of `components/memory/commands/memory.ts` at pin 77f3067f1.

use std::sync::Arc;

use maho_ext_api::ExtensionApi;
use memory_core::memfs::parse_memory_file;

use super::repo::{has_git_repo, open_repo, short_sha};
use super::types::{
    command_context_from, finish, require_identity, respond, CommandContext, CommandResponse,
    MemoryCommandDeps, MemoryCommandIdentity, NotifyLevel,
};

fn is_system_markdown(path: &str) -> bool {
    path.starts_with("system/") && path.ends_with(".md")
}

fn is_skill_path(path: &str) -> bool {
    path.starts_with("skills/")
}

fn render_system_section(
    repo: &memory_core::git::GitMemoryRepo,
    head: &str,
    paths: &[String],
) -> Vec<String> {
    let mut system_paths: Vec<&String> =
        paths.iter().filter(|path| is_system_markdown(path)).collect();
    system_paths.sort();
    if system_paths.is_empty() {
        return vec![
            "## System memory (committed)".to_owned(),
            String::new(),
            "_no system memory files committed_".to_owned(),
        ];
    }

    let mut lines = vec!["## System memory (committed)".to_owned()];
    for path in system_paths {
        let raw = repo.show(head, path).unwrap_or_default();
        let (heading, body) = match parse_memory_file(&raw) {
            Ok(parsed) => (
                format!("### {path} — {}", parsed.frontmatter.description),
                parsed.body,
            ),
            Err(_) => (format!("### {path} — (invalid frontmatter)"), raw),
        };
        lines.push(String::new());
        lines.push(heading);
        lines.push(String::new());
        lines.push(body.trim_end().to_owned());
    }
    lines
}

fn render_name_section(title: &str, paths: &[String]) -> Vec<String> {
    if paths.is_empty() {
        return Vec::new();
    }
    let mut sorted: Vec<&String> = paths.iter().collect();
    sorted.sort();
    let mut lines = vec![String::new(), format!("## {title}")];
    lines.extend(sorted.into_iter().map(|path| format!("- {path}")));
    lines
}

pub async fn handle_memory(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
    _args: &str,
) -> CommandResponse {
    let identity = match require_identity(deps, ctx) {
        Ok(identity) => identity,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };

    if !has_git_repo(&identity) {
        return respond(
            ctx,
            format!(
                "no memory repository for {} at {}; run /memfs init or /init to create one",
                identity.identity,
                identity.identity_paths.repo.display()
            ),
            NotifyLevel::Error,
        );
    }

    render_memory_view(deps, ctx, &identity)
}

fn render_memory_view(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
    identity: &MemoryCommandIdentity,
) -> CommandResponse {
    let repo = match open_repo(deps, identity) {
        Ok(repo) => repo,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };
    let head = match repo.head() {
        Ok(Some(head)) => head,
        Ok(None) => {
            return respond(
                ctx,
                format!(
                    "memory repository at {} has no commits yet; run /init",
                    identity.identity_paths.repo.display()
                ),
                NotifyLevel::Error,
            );
        }
        Err(error) => return respond(ctx, error.to_string(), NotifyLevel::Error),
    };

    let porcelain: Vec<String> = repo
        .status(&[] as &[&str])
        .unwrap_or_default()
        .split('\n')
        .map(|line| line.trim_end_matches('\r').to_owned())
        .filter(|line| !line.trim().is_empty())
        .collect();
    let paths = repo.ls_tree(Some(&head), None).unwrap_or_default();
    let external_paths: Vec<String> = paths
        .iter()
        .filter(|path| !is_system_markdown(path) && !is_skill_path(path))
        .cloned()
        .collect();
    let skill_paths: Vec<String> = paths
        .iter()
        .filter(|path| is_skill_path(path))
        .cloned()
        .collect();

    let dirty = if porcelain.is_empty() {
        "clean".to_owned()
    } else {
        format!(
            "dirty — {} entr{}",
            porcelain.len(),
            if porcelain.len() == 1 { "y" } else { "ies" }
        )
    };
    let mut lines = vec![
        format!("# Memory: {}", identity.identity),
        format!("HEAD: {} ({dirty})", short_sha(&head)),
        format!("Repository: {}", identity.identity_paths.repo.display()),
        String::new(),
    ];
    lines.extend(render_system_section(&repo, &head, &paths));
    lines.extend(render_name_section("External memory (committed)", &external_paths));
    lines.extend(render_name_section("Skills (committed)", &skill_paths));

    if !porcelain.is_empty() {
        lines.push(String::new());
        lines.push("## Uncommitted changes".to_owned());
        lines.extend(porcelain);
    }

    respond(ctx, lines.join("\n"), NotifyLevel::Info)
}

pub fn register_memory_command(api: &mut ExtensionApi, deps: Arc<MemoryCommandDeps>) {
    api.register_command(
        "memory",
        Some(
            "View committed memory: HEAD, system bodies, external names, and uncommitted changes."
                .to_owned(),
        ),
        Some(String::new()),
        Arc::new(move |args, context| {
            let deps = deps.clone();
            let context = command_context_from(context);
            let args = args.to_owned();
            Box::pin(async move { finish(handle_memory(&deps, &context, &args).await) })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::*;

    fn seeds() -> Vec<memory_core::git::GitSeedFile> {
        vec![
            seed("system/persona.md", "---\ndescription: Persona\n---\nI am the test persona.\n"),
            seed(
                "system/projects/index.md",
                "---\ndescription: Project index\n---\nSee [[external/notes]].\n",
            ),
            seed("external/notes.md", "scratch notes\n"),
            seed("skills/commit/SKILL.md", "---\nname: commit\ndescription: Commit helper\n---\n"),
        ]
    }

    #[tokio::test]
    async fn given_a_clean_committed_repo_when_invoked_then_the_viewer_lists_head_system_bodies_and_external_names(
    ) {
        let (_root, identity) = temp_identity();
        let repo = seeded_repo(&identity, seeds());
        let head = repo.head().expect("head").expect("some head");
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memory(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains(&format!("HEAD: {}", short_sha(&head))));
        assert!(response.text.contains("clean"));
        assert!(response.text.contains("### system/persona.md — Persona"));
        assert!(response.text.contains("I am the test persona."));
        assert!(response.text.contains("### system/projects/index.md — Project index"));
        assert!(response.text.contains("- external/notes.md"));
        assert!(response.text.contains("- skills/commit/SKILL.md"));
        assert!(!response.text.contains("Uncommitted"));
        assert_eq!(
            context.ui.notifications().last().map(|(message, _)| message.clone()),
            Some(response.text.clone())
        );
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Info));
    }

    #[tokio::test]
    async fn given_a_dirty_working_tree_when_invoked_then_uncommitted_changes_render_in_a_separate_section(
    ) {
        let (_root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        std::fs::write(
            identity.identity_paths.repo.join("system").join("persona.md"),
            "---\ndescription: Persona\n---\nchanged\n",
        )
        .expect("dirty persona");
        std::fs::write(identity.identity_paths.repo.join("external").join("fresh.md"), "untracked\n")
            .expect("untracked");
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memory(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("dirty"));
        assert!(response.text.contains("## Uncommitted changes"));
        assert!(response.text.contains("M system/persona.md"));
        assert!(response.text.contains("?? external/fresh.md"));
        assert!(response.text.contains("I am the test persona."));
        assert!(!response.text.contains("changed\n"));
    }

    #[tokio::test]
    async fn given_no_repository_when_invoked_then_an_actionable_error_is_returned() {
        let (_root, identity) = temp_identity();
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memory(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("no memory repository"));
        assert!(response.text.contains("/memfs init"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }

    #[tokio::test]
    async fn given_an_unbound_session_when_invoked_then_an_actionable_error_is_returned() {
        let fake = fake_deps(None, FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_memory(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("not bound"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }
}
