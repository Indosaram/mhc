//! Port of senpi packages/coding-agent/src/core/resource-loader.ts (the project-context and
//! prompt-input layer).
//!
//! DefaultResourceLoader itself is the TS extension and resource-discovery host: extensions are
//! native Rust crates registered statically in maho-cli (plan D-M5), so its extension half has no
//! counterpart here, and the resource half (skills, prompts, themes) is ported in skills.rs,
//! prompt_templates.rs and package_manager.rs. find_git_paths mirrors footer-data-provider.ts,
//! whose git metadata lookup this module needs.

use std::collections::HashMap;
use std::path::Path;

use crate::diagnostics::{ResourceCollision, ResourceCollisionType, ResourceDiagnostic, ResourceDiagnosticType};
use crate::package_identity::dedupe_paths_by_package_identity;
use crate::package_manager::{DefaultPackageManager, PackageManagerOptions, PathMetadata, ResolveSourcesOptions, ResolvedPaths, ResolvedResource};
use crate::paths::{canonicalize_path, is_local_path, lexical_resolve, resolve_path, PathInputOptions};
use crate::prompt_templates::{load_prompt_templates, LoadPromptTemplatesOptions, PromptTemplate};
use crate::resolved_paths_memo::{ResolvedPathsMemoKeyInput, memoize_resolved_paths, resolved_paths_memo_key};
use crate::settings_manager::{InMemorySettingsStorage, SettingsManager, SettingsScope, SettingsStorage};
use crate::skills::{LoadSkillsOptions, LoadSkillsResult};
use crate::source_info::{create_source_info, SourceInfo, SourceOrigin, SourceScope};
use crate::system_prompt::ContextFile;
use crate::text::strip_bom;

/// senpi loadContextFileFromDir candidates.
pub const AGENTS_FILE_CANDIDATES: [&str; 5] = ["AGENTS.override.md", "AGENTS.md", "AGENTS.MD", "CLAUDE.md", "CLAUDE.MD"];

/// senpi resolvePromptInput.
pub fn resolve_prompt_input(input: Option<&str>, description: &str) -> Option<String> {
    let input = input.filter(|input| !input.is_empty())?;

    if Path::new(input).exists() {
        match std::fs::read_to_string(input) {
            Ok(content) => return Some(strip_bom(&content).to_string()),
            Err(error) => {
                crate::output_guard::maho_write_stderr(&format!("Warning: Could not read {description} file {input}: {error}\n"));
                return Some(input.to_string());
            }
        }
    }

    Some(input.to_string())
}

/// senpi loadContextFileFromDir.
pub fn load_context_file_from_dir(dir: &str) -> Option<ContextFile> {
    for filename in AGENTS_FILE_CANDIDATES {
        let file_path = Path::new(dir).join(filename).to_string_lossy().into_owned();
        if !Path::new(&file_path).exists() {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&file_path) else {
            crate::output_guard::maho_write_stderr(&format!("Warning: Could not read {file_path}\n"));
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        match std::fs::read_to_string(&file_path) {
            Ok(content) => return Some(ContextFile { path: file_path, content: strip_bom(&content).to_string() }),
            Err(error) => { crate::output_guard::maho_write_stderr(&format!("Warning: Could not read {file_path}: {error}\n")); }
        }
    }
    None
}

/// senpi GitPaths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPaths {
    pub repo_dir: String,
    pub common_git_dir: String,
    pub head_path: String,
}

/// senpi findGitPaths: walks up from cwd through both a .git directory and a worktree .git file.
pub fn find_git_paths(cwd: &str) -> Option<GitPaths> {
    let mut dir = cwd.to_string();
    loop {
        let git_path = Path::new(&dir).join(".git");
        if git_path.exists() {
            let Ok(metadata) = std::fs::metadata(&git_path) else {
                return None;
            };
            let git_path = git_path.to_string_lossy().into_owned();
            if metadata.is_file() {
                let Ok(content) = std::fs::read_to_string(&git_path) else {
                    return None;
                };
                let content = content.trim().to_string();
                if let Some(target) = content.strip_prefix("gitdir: ") {
                    let git_dir = lexical_resolve(&Path::new(&dir).join(target.trim()).to_string_lossy());
                    let head_path = Path::new(&git_dir).join("HEAD").to_string_lossy().into_owned();
                    if !Path::new(&head_path).exists() {
                        return None;
                    }
                    let common_dir_path = Path::new(&git_dir).join("commondir").to_string_lossy().into_owned();
                    let common_git_dir = match std::fs::read_to_string(&common_dir_path) {
                        Ok(common) => lexical_resolve(&Path::new(&git_dir).join(common.trim()).to_string_lossy()),
                        Err(_) => git_dir,
                    };
                    return Some(GitPaths { repo_dir: dir, common_git_dir, head_path });
                }
            } else if metadata.is_dir() {
                let head_path = Path::new(&git_path).join("HEAD").to_string_lossy().into_owned();
                if !Path::new(&head_path).exists() {
                    return None;
                }
                return Some(GitPaths { repo_dir: dir, common_git_dir: git_path, head_path });
            }
        }
        let parent = Path::new(&dir).parent().map(|parent| parent.to_string_lossy().into_owned());
        match parent {
            Some(parent) if parent != dir => dir = parent,
            _ => return None,
        }
    }
}

/// senpi findShadowedContextFile: the main repo's context file a nested linked worktree shadows.
pub fn find_shadowed_context_file(cwd: &str) -> Option<String> {
    let git_paths = find_git_paths(cwd)?;
    let common_git_dir = canonicalize_path(&git_paths.common_git_dir);
    let worktree_root = canonicalize_path(&git_paths.repo_dir);
    let main_repo_root = Path::new(&common_git_dir).parent().map(|parent| parent.to_string_lossy().into_owned())?;
    if !worktree_root.starts_with(&format!("{main_repo_root}{}", std::path::MAIN_SEPARATOR)) {
        return None;
    }
    if canonicalize_path(&Path::new(&main_repo_root).join(".git").to_string_lossy()) != common_git_dir {
        return None;
    }
    let worktree_context_file = load_context_file_from_dir(&worktree_root)?;
    let file_name = Path::new(&worktree_context_file.path).file_name().map(|name| name.to_string_lossy().into_owned())?;
    Some(Path::new(&main_repo_root).join(file_name).to_string_lossy().into_owned())
}

/// senpi loadProjectContextFiles: the agent-dir file first, then the nearest ancestor files.
pub fn load_project_context_files(cwd: &str, agent_dir: &str) -> Vec<ContextFile> {
    let process_cwd = std::env::current_dir().map(|path| path.to_string_lossy().into_owned()).unwrap_or_default();
    let resolved_cwd = resolve_path(cwd, &process_cwd, &PathInputOptions::default());
    let resolved_agent_dir = resolve_path(agent_dir, &process_cwd, &PathInputOptions::default());

    let mut context_files: Vec<ContextFile> = Vec::new();
    let mut seen_paths: Vec<String> = Vec::new();

    if let Some(global_context) = load_context_file_from_dir(&resolved_agent_dir) {
        seen_paths.push(global_context.path.clone());
        context_files.push(global_context);
    }

    let mut ancestor_context_files: Vec<ContextFile> = Vec::new();
    let shadowed_context_file = find_shadowed_context_file(&resolved_cwd);
    let mut current_dir = resolved_cwd;

    loop {
        let context_file = load_context_file_from_dir(&current_dir);
        let is_shadowed = shadowed_context_file
            .as_ref()
            .is_some_and(|shadowed| canonicalize_path(context_file.as_ref().map(|file| file.path.as_str()).unwrap_or("")) == *shadowed);
        if let Some(context_file) = context_file
            && !is_shadowed
            && !seen_paths.contains(&context_file.path)
        {
            seen_paths.push(context_file.path.clone());
            ancestor_context_files.insert(0, context_file);
        }

        let parent = Path::new(&current_dir).parent().map(|parent| parent.to_string_lossy().into_owned());
        match parent {
            Some(parent) if parent != current_dir => current_dir = parent,
            _ => break,
        }
    }

    context_files.extend(ancestor_context_files);
    context_files
}

/// The CLI inputs senpi `DefaultResourceLoader` keeps across reloads: `-e` sources
/// (`additionalExtensionPaths`), `--skill`, `--prompt-template` and hook paths, and the
/// `--no-skills` / `--no-prompt-templates` switches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourceLoaderInputs {
    pub additional_extension_paths: Vec<String>,
    pub additional_skill_paths: Vec<String>,
    pub additional_prompt_template_paths: Vec<String>,
    pub additional_hook_paths: Vec<String>,
    /// Hook resources an SDK caller passes directly (`CreateAgentSessionOptions::hook_resources`).
    pub hook_resources: Vec<ResolvedResource>,
    pub no_skills: bool,
    pub no_prompt_templates: bool,
}

/// The skill, prompt and hook half of one senpi `DefaultResourceLoader.reload()`.
#[derive(Debug, Clone, Default)]
pub struct LoadedResources {
    pub skills: LoadSkillsResult,
    pub prompts: Vec<PromptTemplate>,
    pub prompt_diagnostics: Vec<ResourceDiagnostic>,
    /// Enabled user- and project-scope hook resources (senpi `globalHookSourcePaths` /
    /// `projectHookSourcePaths`).
    pub hook_resources: Vec<ResolvedResource>,
    /// senpi `preSessionHookSourcePaths`: `-e` package hooks, then the additional hook paths.
    pub pre_session_hook_paths: Vec<String>,
}

/// A detached copy of the settings the package manager resolves against, so resolution never
/// holds the session's settings lock across an await.
pub fn snapshot_settings(settings: &SettingsManager) -> SettingsManager {
    let storage = InMemorySettingsStorage::default();
    for (scope, values) in [(SettingsScope::Global, settings.get_global()), (SettingsScope::Project, settings.get_project())] {
        let content = serde_json::to_string(values).ok();
        if let Err(error) = storage.with_lock(scope, &mut |_| content.clone()) {
            crate::output_guard::maho_write_stderr(&format!("Warning: Could not snapshot settings: {error}\n"));
        }
    }
    SettingsManager::from_storage(Box::new(storage), settings.is_project_trusted())
}

fn error_diagnostic(message: String, path: Option<String>) -> ResourceDiagnostic {
    ResourceDiagnostic { category: None, diagnostic_type: ResourceDiagnosticType::Error, message, path, collision: None }
}

/// senpi `mergePaths`: resolve against cwd, keep the first canonical occurrence.
fn merge_paths(cwd: &str, primary: Vec<String>, additional: &[String]) -> Vec<String> {
    let mut merged = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for path in primary.iter().chain(additional) {
        let resolved = resolve_path(path, cwd, &PathInputOptions { trim: true, ..Default::default() });
        if seen.insert(canonicalize_path(&resolved)) {
            merged.push(resolved);
        }
    }
    merged
}

/// senpi `getEnabledResources` + `getEnabledPaths`: record first-seen metadata, keep enabled paths.
fn enabled_paths(resources: &[ResolvedResource], metadata_by_path: &mut HashMap<String, PathMetadata>) -> Vec<String> {
    for resource in resources {
        metadata_by_path.entry(resource.path.clone()).or_insert_with(|| resource.metadata.clone());
    }
    resources.iter().filter(|resource| resource.enabled).map(|resource| resource.path.clone()).collect()
}

/// senpi `mapSkillPath`: an auto/package skill directory with a `SKILL.md` loads that file.
fn map_skill_path(resource: &ResolvedResource, metadata_by_path: &mut HashMap<String, PathMetadata>) -> String {
    if resource.metadata.source != "auto" && resource.metadata.origin != SourceOrigin::Package {
        return resource.path.clone();
    }
    if !Path::new(&resource.path).is_dir() {
        return resource.path.clone();
    }
    let skill_file = Path::new(&resource.path).join("SKILL.md");
    if skill_file.exists() {
        let skill_file = skill_file.to_string_lossy().into_owned();
        metadata_by_path.entry(skill_file.clone()).or_insert_with(|| resource.metadata.clone());
        return skill_file;
    }
    resource.path.clone()
}

/// senpi `findSourceInfoForPath` (metadata half): exact path, then the containing resource root.
fn find_source_info(resource_path: &str, metadata_by_path: &HashMap<String, PathMetadata>) -> Option<SourceInfo> {
    if resource_path.is_empty() || resource_path.starts_with('<') {
        return None;
    }
    let normalized = lexical_resolve(resource_path);
    if let Some(exact) = metadata_by_path.get(&normalized).or_else(|| metadata_by_path.get(resource_path)) {
        return Some(create_source_info(resource_path, exact));
    }
    metadata_by_path.iter().find(|(source_path, _)| {
        let root = lexical_resolve(source_path);
        normalized == root || normalized.starts_with(&format!("{root}{}", std::path::MAIN_SEPARATOR))
    }).map(|(_, metadata)| create_source_info(resource_path, metadata))
}

/// senpi `dedupePrompts`: the first template per name wins; later ones are collisions.
pub fn dedupe_prompts(prompts: Vec<PromptTemplate>) -> (Vec<PromptTemplate>, Vec<ResourceDiagnostic>) {
    let mut kept: Vec<PromptTemplate> = Vec::new();
    let mut diagnostics = Vec::new();
    for prompt in prompts {
        if let Some(existing) = kept.iter().find(|existing| existing.name == prompt.name) {
            diagnostics.push(ResourceDiagnostic {
                category: None,
                diagnostic_type: ResourceDiagnosticType::Collision,
                message: format!("name \"/{}\" collision", prompt.name),
                path: Some(prompt.file_path.clone()),
                collision: Some(ResourceCollision {
                    resource_type: ResourceCollisionType::Prompt,
                    name: prompt.name.clone(),
                    winner_path: existing.file_path.clone(),
                    loser_path: prompt.file_path.clone(),
                    winner_source: None,
                    loser_source: None,
                }),
            });
        } else {
            kept.push(prompt);
        }
    }
    (kept, diagnostics)
}

/// senpi `DefaultResourceLoader.reload()` for skills, prompt templates and hooks: resolve packages
/// and auto-discovered resources (`packageManager.resolve()`), resolve the `-e` sources as temporary
/// packages, then load the enabled paths CLI-first and in package-manager precedence order.
/// Like upstream (`resolve()` / `installParsedSource` have no catch), a package or `-e` source that
/// cannot be resolved or installed fails the whole load.
pub async fn load_resources(cwd: &str, agent_dir: &str, settings: SettingsManager, inputs: &ResourceLoaderInputs) -> Result<LoadedResources, String> {
    load_resources_with_refresh(cwd, agent_dir, settings, inputs, false).await
}

/// Resolution refresh for an explicit reload signal when package manifests may have changed.
pub async fn load_resources_with_refresh(cwd: &str, agent_dir: &str, settings: SettingsManager, inputs: &ResourceLoaderInputs, refresh: bool) -> Result<LoadedResources, String> {
    let key = resolved_paths_memo_key(&ResolvedPathsMemoKeyInput {
        agent_dir: resolve_path(agent_dir, cwd, &PathInputOptions::default()),
        cwd: resolve_path(cwd, cwd, &PathInputOptions::default()),
        project_trusted: settings.is_project_trusted(),
        global_settings: serde_json::Value::Object(settings.get_global().clone()),
        project_settings: serde_json::Value::Object(settings.get_project().clone()),
        additional_extension_paths: inputs.additional_extension_paths.clone(),
    });
    let cwd_owned = cwd.to_owned();
    let agent_dir_owned = agent_dir.to_owned();
    let sources = inputs.additional_extension_paths.clone();
    let (resolved, cli) = memoize_resolved_paths(key, move || async move {
        let mut snapshot = snapshot_settings(&settings);
        let manager = DefaultPackageManager::new(PackageManagerOptions { cwd: &cwd_owned, agent_dir: &agent_dir_owned, settings_manager: &mut snapshot });
        let resolved = manager.resolve(None).await.map_err(|error| error.to_string())?;
        let cli = if sources.is_empty() { ResolvedPaths::default() } else {
            manager.resolve_extension_sources(&sources, Some(ResolveSourcesOptions { local: false, temporary: true })).await.map_err(|error| error.to_string())?
        };
        Ok((resolved, cli))
    }, refresh).await?;

    let mut metadata_by_path: HashMap<String, PathMetadata> = HashMap::new();
    let enabled_prompts = enabled_paths(&resolved.prompts, &mut metadata_by_path);
    for resource in &resolved.skills {
        metadata_by_path.entry(resource.path.clone()).or_insert_with(|| resource.metadata.clone());
    }
    let enabled_skills: Vec<String> = resolved.skills.iter().filter(|resource| resource.enabled)
        .map(|resource| map_skill_path(resource, &mut metadata_by_path)).collect();
    // Explicit -e resources keep CLI precedence even when they resolve through a package manifest.
    for resource in cli.skills.iter().chain(&cli.prompts).chain(&cli.hooks) {
        let metadata = if resource.metadata.scope == SourceScope::System {
            PathMetadata { skill_provenance: Some(crate::skill_access::SkillProvenance { kind:crate::skill_access::SkillProvenanceKind::User, selected_root:resource.metadata.skill_provenance.as_ref().map_or_else(|| resource.path.clone(), |p| p.selected_root.clone()), project_root:None }),  source: "cli".into(), scope: SourceScope::System, origin: SourceOrigin::TopLevel, base_dir: resource.metadata.base_dir.clone() }
        } else {
            PathMetadata { skill_provenance: Some(crate::skill_access::SkillProvenance { kind:crate::skill_access::SkillProvenanceKind::User, selected_root:resource.metadata.skill_provenance.as_ref().map_or_else(|| resource.path.clone(), |p| p.selected_root.clone()), project_root:None }),  source: "cli".into(), scope: SourceScope::Temporary, origin: SourceOrigin::TopLevel, base_dir: None }
        };
        metadata_by_path.insert(resource.path.clone(), metadata);
    }
    let cli_skills = enabled_paths(&cli.skills, &mut metadata_by_path);
    let cli_prompts = enabled_paths(&cli.prompts, &mut metadata_by_path);
    let cli_hooks = enabled_paths(&cli.hooks, &mut metadata_by_path);

    let hook_resources = inputs.hook_resources.iter().filter(|resource| resource.enabled).cloned()
        .chain(resolved.hooks.into_iter().filter(|resource| resource.enabled && matches!(resource.metadata.scope, SourceScope::User | SourceScope::Project)))
        .collect();
    let pre_session_hook_paths = merge_paths(cwd, cli_hooks, &inputs.additional_hook_paths);

    let skill_candidates = if inputs.no_skills { cli_skills } else { [cli_skills, enabled_skills].concat() };
    let skill_paths = dedupe_paths_by_package_identity(&merge_paths(cwd, skill_candidates, &inputs.additional_skill_paths));
    let mut user_skill_roots: Vec<_> = resolved.skill_roots.iter().chain(&cli.skill_roots).filter(|root| root.kind == crate::skill_access::SkillProvenanceKind::User).map(|root| root.selected_root.clone()).collect();
    user_skill_roots.extend(skill_paths.iter().filter(|path| metadata_by_path.get(*path).is_none_or(|metadata| metadata.scope != SourceScope::Project)).cloned());
    let provenance_by_path = skill_paths.iter().map(|path| (path.clone(), metadata_by_path.get(path).and_then(|m| m.skill_provenance.clone()).unwrap_or(crate::skill_access::SkillProvenance { kind:crate::skill_access::SkillProvenanceKind::User, selected_root:path.clone(), project_root:None }))).collect();
    let mut skills = if inputs.no_skills && skill_paths.is_empty() { LoadSkillsResult::default() } else {
        crate::skills::load_skills_with_provenance(&LoadSkillsOptions { cwd: cwd.to_owned(), agent_dir: agent_dir.to_owned(), skill_paths, include_defaults: false }, &provenance_by_path, &user_skill_roots)
    };
    for skill in &mut skills.skills {
        if let Some(info) = find_source_info(&skill.file_path, &metadata_by_path) { skill.source_info = info; }
    }
    for path in inputs.additional_skill_paths.iter().filter(|path| is_local_path(path)) {
        let resolved_path = resolve_path(path, cwd, &PathInputOptions { trim: true, ..Default::default() });
        if !Path::new(&resolved_path).exists() && !skills.diagnostics.iter().any(|diagnostic| diagnostic.path.as_deref() == Some(resolved_path.as_str())) {
            skills.diagnostics.push(error_diagnostic("Skill path does not exist".into(), Some(resolved_path)));
        }
    }

    let prompt_candidates = if inputs.no_prompt_templates { cli_prompts } else { [cli_prompts, enabled_prompts].concat() };
    let prompt_paths = merge_paths(cwd, prompt_candidates, &inputs.additional_prompt_template_paths);
    let (mut prompts, mut prompt_diagnostics) = if inputs.no_prompt_templates && prompt_paths.is_empty() { (Vec::new(), Vec::new()) } else {
        let (loaded, mut diagnostics) = load_prompt_templates(&LoadPromptTemplatesOptions {
            cwd: cwd.to_owned(), agent_dir: agent_dir.to_owned(), prompt_paths, include_defaults: false,
        });
        let (prompts, mut collisions) = dedupe_prompts(loaded);
        diagnostics.append(&mut collisions);
        (prompts, diagnostics)
    };
    for prompt in &mut prompts {
        if let Some(info) = find_source_info(&prompt.file_path, &metadata_by_path) { prompt.source_info = info; }
    }
    for path in inputs.additional_prompt_template_paths.iter().filter(|path| is_local_path(path)) {
        let resolved_path = resolve_path(path, cwd, &PathInputOptions { trim: true, ..Default::default() });
        if !Path::new(&resolved_path).exists() && !prompt_diagnostics.iter().any(|diagnostic| diagnostic.path.as_deref() == Some(resolved_path.as_str())) {
            prompt_diagnostics.push(error_diagnostic("Prompt template path does not exist".into(), Some(resolved_path)));
        }
    }

    Ok(LoadedResources { skills, prompts, prompt_diagnostics, hook_resources, pre_session_hook_paths })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, content).expect("write");
    }

    #[test]
    fn a_prompt_input_that_names_a_file_is_read_and_stripped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("prompt.md");
        write(&path, "\u{feff}file prompt");
        assert_eq!(resolve_prompt_input(Some(&path.to_string_lossy()), "system prompt").as_deref(), Some("file prompt"));
    }

    #[test]
    fn a_literal_prompt_input_passes_through_and_an_empty_one_is_none() {
        assert_eq!(resolve_prompt_input(Some("be brief"), "system prompt").as_deref(), Some("be brief"));
        assert_eq!(resolve_prompt_input(Some(""), "system prompt"), None);
        assert_eq!(resolve_prompt_input(None, "system prompt"), None);
    }

    #[test]
    fn context_files_follow_the_candidate_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(&dir.path().join("AGENTS.md"), "agents");
        write(&dir.path().join("CLAUDE.md"), "claude");
        let loaded = load_context_file_from_dir(&dir.path().to_string_lossy()).expect("context file");
        assert!(loaded.path.ends_with("AGENTS.md"));
        assert_eq!(loaded.content, "agents");

        write(&dir.path().join("AGENTS.override.md"), "override");
        let loaded = load_context_file_from_dir(&dir.path().to_string_lossy()).expect("context file");
        assert!(loaded.path.ends_with("AGENTS.override.md"));
    }

    #[test]
    fn a_directory_named_like_a_context_file_is_not_loaded() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("AGENTS.md")).expect("mkdir");
        assert!(load_context_file_from_dir(&dir.path().to_string_lossy()).is_none());
    }

    #[test]
    fn git_paths_are_found_from_a_regular_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(&dir.path().join(".git").join("HEAD"), "ref: refs/heads/main\n");
        let nested = dir.path().join("a").join("b");
        std::fs::create_dir_all(&nested).expect("mkdir");
        let paths = find_git_paths(&nested.to_string_lossy()).expect("git paths");
        assert_eq!(paths.repo_dir, dir.path().to_string_lossy());
        assert!(paths.common_git_dir.ends_with(".git"));
        assert!(paths.head_path.ends_with("HEAD"));
    }

    #[test]
    fn git_paths_are_found_through_a_worktree_git_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let git_dir = dir.path().join("main").join(".git").join("worktrees").join("feat");
        write(&git_dir.join("HEAD"), "ref: refs/heads/feat\n");
        let worktree = dir.path().join("feat");
        std::fs::create_dir_all(&worktree).expect("mkdir");
        write(&worktree.join(".git"), &format!("gitdir: {}\n", git_dir.to_string_lossy()));
        let paths = find_git_paths(&worktree.to_string_lossy()).expect("git paths");
        assert_eq!(paths.common_git_dir, git_dir.to_string_lossy());
        assert_eq!(paths.repo_dir, worktree.to_string_lossy());
    }

    #[test]
    fn git_paths_are_absent_outside_a_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(find_git_paths(&dir.path().to_string_lossy()).is_none());
    }

    #[test]
    fn the_agent_context_file_comes_first_then_ancestors_from_the_root_down() {
        let dir = crate::test_support::isolated_tempdir();
        let agent_dir = dir.path().join("agent");
        let project = dir.path().join("project");
        let nested = project.join("sub");
        write(&agent_dir.join("AGENTS.md"), "agent rules");
        write(&project.join("AGENTS.md"), "project rules");
        write(&nested.join("AGENTS.md"), "nested rules");

        let files = load_project_context_files(&nested.to_string_lossy(), &agent_dir.to_string_lossy());
        let contents: Vec<&str> = files.iter().map(|file| file.content.as_str()).collect();
        assert_eq!(contents, vec!["agent rules", "project rules", "nested rules"]);
    }

    fn trusted_settings(global: serde_json::Value) -> SettingsManager {
        let mut settings = SettingsManager::from_storage(Box::<InMemorySettingsStorage>::default(), true);
        if let serde_json::Value::Object(values) = global { settings.set(SettingsScope::Global, &values).expect("global settings"); }
        settings
    }

    fn skill(dir: &Path, name: &str) {
        write(&dir.join(name).join("SKILL.md"), &format!("---\nname: {name}\ndescription: fixture\n---\nbody"));
    }

    #[tokio::test]
    async fn package_skills_prompts_and_hooks_are_loaded() {
        let dir = crate::test_support::isolated_tempdir();
        let cwd = dir.path().join("project");
        let agent = dir.path().join("agent");
        skill(&dir.path().join("pkg").join("skills"), "pkg-skill");
        write(&dir.path().join("pkg").join("prompts").join("pkg-prompt.md"), "package prompt");
        write(&dir.path().join("pkg").join("hooks").join("hooks.json"), "{}");
        write(&agent.join("hooks").join("user.json"), "{}");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let settings = trusted_settings(serde_json::json!({"packages": ["../pkg"]}));
        let loaded = load_resources(&cwd.to_string_lossy(), &agent.to_string_lossy(), settings, &ResourceLoaderInputs::default()).await.expect("load resources");
        let package_skill = loaded.skills.skills.iter().find(|skill| skill.name == "pkg-skill").expect("package skill");
        assert_eq!(package_skill.source_info.origin, SourceOrigin::Package);
        assert!(loaded.prompts.iter().any(|prompt| prompt.name == "pkg-prompt"));
        let hooks: Vec<&str> = loaded.hook_resources.iter().map(|resource| resource.path.as_str()).collect();
        assert!(hooks.iter().any(|path| path.replace('\\', "/").ends_with("pkg/hooks/hooks.json")), "{hooks:?}");
        assert!(hooks.iter().any(|path| path.replace('\\', "/").ends_with("agent/hooks/user.json")), "{hooks:?}");
    }

    #[tokio::test]
    async fn a_project_skill_and_prompt_win_a_name_collision_with_the_user_ones() {
        let dir = crate::test_support::isolated_tempdir();
        let cwd = dir.path().join("project");
        let agent = dir.path().join("agent");
        let project = cwd.join(crate::config::config_dir_name());
        skill(&agent.join("skills"), "dup-skill");
        skill(&project.join("skills"), "dup-skill");
        write(&agent.join("prompts").join("dup.md"), "user prompt");
        write(&project.join("prompts").join("dup.md"), "project prompt");
        let loaded = load_resources(&cwd.to_string_lossy(), &agent.to_string_lossy(), trusted_settings(serde_json::json!({})), &ResourceLoaderInputs::default()).await.expect("load resources");
        let winner = loaded.skills.skills.iter().find(|skill| skill.name == "dup-skill").expect("skill");
        assert!(winner.file_path.starts_with(&*project.to_string_lossy()), "{}", winner.file_path);
        let prompt = loaded.prompts.iter().find(|prompt| prompt.name == "dup").expect("prompt");
        assert_eq!(prompt.content.trim(), "project prompt");
        assert_eq!(loaded.prompt_diagnostics.len(), 1);
    }

    #[tokio::test]
    async fn explicit_refresh_rediscovers_package_resources() {
        let dir = crate::test_support::isolated_tempdir();
        let cwd = dir.path().join("project");
        let agent = dir.path().join("agent");
        let package = dir.path().join("pkg");
        std::fs::create_dir_all(&cwd).expect("cwd");
        write(&package.join("prompts").join("first.md"), "first");
        let settings = || trusted_settings(serde_json::json!({"packages": ["../pkg"]}));
        let inputs = ResourceLoaderInputs::default();
        let cwd = cwd.to_string_lossy();
        let agent = agent.to_string_lossy();
        let first = load_resources(&cwd, &agent, settings(), &inputs).await.expect("initial");
        assert!(first.prompts.iter().any(|prompt| prompt.name == "first"));
        write(&package.join("prompts").join("second.md"), "second");
        let cached = load_resources(&cwd, &agent, settings(), &inputs).await.expect("cached");
        assert!(!cached.prompts.iter().any(|prompt| prompt.name == "second"));
        let refreshed = load_resources_with_refresh(&cwd, &agent, settings(), &inputs, true).await.expect("refresh");
        assert!(refreshed.prompts.iter().any(|prompt| prompt.name == "second"));
    }

    #[tokio::test]
    async fn cli_skill_paths_load_first_and_no_skills_drops_discovered_ones() {
        let dir = crate::test_support::isolated_tempdir();
        let cwd = dir.path().join("project");
        let agent = dir.path().join("agent");
        skill(&agent.join("skills"), "user-skill");
        skill(&dir.path().join("cli"), "cli-skill");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let inputs = ResourceLoaderInputs {
            additional_skill_paths: vec![dir.path().join("cli").to_string_lossy().into_owned()], no_skills: true, ..Default::default()
        };
        let loaded = load_resources(&cwd.to_string_lossy(), &agent.to_string_lossy(), trusted_settings(serde_json::json!({})), &inputs).await.expect("load resources");
        let names: Vec<&str> = loaded.skills.skills.iter().map(|skill| skill.name.as_str()).collect();
        assert_eq!(names, ["cli-skill"]);
    }

    #[tokio::test]
    async fn prompt_parse_diagnostics_reach_loaded_resources() {
        let dir = crate::test_support::isolated_tempdir();
        let cwd = dir.path().join("project");
        let agent = dir.path().join("agent");
        let prompts = dir.path().join("prompts");
        let invalid = prompts.join("invalid.md");
        std::fs::create_dir_all(&cwd).expect("cwd");
        write(&invalid, "---\ndescription: [unterminated\n---\nbody");
        write(&prompts.join("valid.md"), "valid body");
        let inputs = ResourceLoaderInputs {
            additional_prompt_template_paths: vec![prompts.to_string_lossy().into_owned()],
            ..Default::default()
        };
        let loaded = load_resources(&cwd.to_string_lossy(), &agent.to_string_lossy(), trusted_settings(serde_json::json!({})), &inputs).await.expect("load resources");
        assert_eq!(loaded.prompts.iter().map(|prompt| prompt.name.as_str()).collect::<Vec<_>>(), ["valid"]);
        assert_eq!(loaded.prompt_diagnostics.len(), 1);
        assert_eq!(loaded.prompt_diagnostics[0].diagnostic_type, ResourceDiagnosticType::Warning);
        assert_eq!(loaded.prompt_diagnostics[0].path.as_deref(), Some(invalid.to_string_lossy().as_ref()));
    }

    #[test]
    fn duplicate_prompt_names_keep_the_first_and_report_a_collision() {
        let prompt = |path: &str| PromptTemplate {
            name: "review".into(), description: String::new(), argument_hint: None, requires_arguments: false, content: String::new(),
            source_info: SourceInfo::default(), file_path: path.into(),
        };
        let (kept, diagnostics) = dedupe_prompts(vec![prompt("/a/review.md"), prompt("/b/review.md")]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].file_path, "/a/review.md");
        assert_eq!(diagnostics[0].message, "name \"/review\" collision");
        let collision = diagnostics[0].collision.as_ref().expect("collision");
        assert_eq!(collision.resource_type, ResourceCollisionType::Prompt);
        assert_eq!((collision.winner_path.as_str(), collision.loser_path.as_str()), ("/a/review.md", "/b/review.md"));
    }

    #[test]
    fn a_repeated_context_file_is_loaded_once() {
        let dir = crate::test_support::isolated_tempdir();
        let agent_dir = dir.path().to_string_lossy().into_owned();
        write(&dir.path().join("AGENTS.md"), "rules");
        let files = load_project_context_files(&agent_dir, &agent_dir);
        assert_eq!(files.len(), 1);
    }
}
