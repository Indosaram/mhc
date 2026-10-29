use lsp_core::post_edit::*;
use std::fs::{create_dir_all, write};
use std::path::Path;
use tempfile::TempDir;

async fn collect_missing(
    paths: &[String],
    cache: &PostEditNotConfiguredCache,
    location: Option<PostEditFileLocation>,
    decision_tool: bool,
    decision: Option<&str>,
) -> PostEditDiagnosticsResult {
    let locate = |_: &str| location.expect("locator supplied");
    collect_post_edit_diagnostics(CollectPostEditDiagnosticsInput {
        file_paths: paths,
        cache: Some(cache),
        max_concurrency: None,
        locate_file: location.map(|_| &locate as &(dyn Fn(&str) -> PostEditFileLocation + Sync)),
        run_diagnostics: |path: String| {
            let outcome = PostEditNotInstalledOutcome {
                server_id: if path.ends_with(".json") { "biome" } else { "typescript" }.into(),
                install_decision_tool: decision_tool,
                decision: decision.map(str::to_string),
                text: "sentinel".into(),
            };
            async move { Ok(PostEditDiagnosticsOutcome::NotInstalled(outcome)) }
        },
    }).await
}

#[tokio::test]
async fn non_project_locations_suppress_undecided_guidance() {
    for location in [PostEditFileLocation::OutsideProject, PostEditFileLocation::AgentConfig, PostEditFileLocation::Temp] {
        let result = collect_missing(&["a.ts".into()], &create_post_edit_not_configured_cache(), Some(location), false, None).await;
        assert!(result.blocks.is_empty());
        assert_eq!(result.observations[0].kind, PostEditObservationKind::NotInstalled);
    }
}

#[tokio::test]
async fn project_guidance_is_nonblocking() {
    let result = collect_missing(&["a.ts".into()], &create_post_edit_not_configured_cache(), Some(PostEditFileLocation::Project), false, None).await;
    assert_eq!(result.blocks, vec![PostEditDiagnosticsBlock { file_path: "a.ts".into(), diagnostics: "sentinel".into(), blocking: false }]);
}

#[tokio::test]
async fn each_missing_server_is_nudged_once_until_reset() {
    let cache = create_post_edit_not_configured_cache();
    let first = collect_missing(&["a.ts".into(), "b.ts".into()], &cache, Some(PostEditFileLocation::Project), false, None).await;
    let second = collect_missing(&["c.ts".into(), "d.json".into()], &cache, Some(PostEditFileLocation::Project), false, None).await;
    let third = collect_missing(&["e.json".into(), "f.ts".into()], &cache, Some(PostEditFileLocation::Project), false, None).await;
    assert_eq!(first.blocks.iter().map(|block| block.file_path.as_str()).collect::<Vec<_>>(), vec!["a.ts"]);
    assert_eq!(second.blocks.iter().map(|block| block.file_path.as_str()).collect::<Vec<_>>(), vec!["d.json"]);
    assert!(third.blocks.is_empty());
    reset_post_edit_not_configured_cache(&cache);
    assert_eq!(collect_missing(&["g.ts".into()], &cache, Some(PostEditFileLocation::Project), false, None).await.blocks.len(), 1);
}

#[tokio::test]
async fn decision_tool_keeps_guidance_on_repeated_edits() {
    let cache = create_post_edit_not_configured_cache();
    for path in ["a.ts", "b.ts"] {
        assert_eq!(collect_missing(&[path.into()], &cache, Some(PostEditFileLocation::Project), true, None).await.blocks.len(), 1);
    }
}

#[tokio::test]
async fn recorded_decisions_keep_their_text_everywhere() {
    for decision in ["declined", "allowed"] {
        let cache = create_post_edit_not_configured_cache();
        for location in [PostEditFileLocation::Project, PostEditFileLocation::OutsideProject, PostEditFileLocation::Project] {
            let result = collect_missing(&["a.ts".into()], &cache, Some(location), false, Some(decision)).await;
            assert_eq!(result.blocks[0].diagnostics, "sentinel");
            assert!(!result.blocks[0].blocking);
        }
    }
}

#[tokio::test]
async fn no_locator_keeps_nonblocking_guidance() {
    let result = collect_missing(&["a.ts".into()], &create_post_edit_not_configured_cache(), None, false, None).await;
    assert_eq!(result.blocks.len(), 1);
    assert!(!result.blocks[0].blocking);
}

fn temp_root(prefix: &str) -> TempDir { tempfile::Builder::new().prefix(prefix).tempdir().expect("tempdir") }
fn write_exec(path: &Path) { create_dir_all(path.parent().expect("parent")).expect("mkdir"); write(path, "sentinel").expect("write"); }

#[test]
fn test_classify_no_marker_is_outside_project() {
    let root = temp_root("lsp-postedit-noproject-");
    let file = root.path().join("scratch/note.mjs");
    write_exec(&file);
    let opts = PostEditFileLocationOptions {
        cwd: &root.path().to_string_lossy(),
        agent_dirs: &[],
        temp_dirs: Some(&[]),
        home_dirs: None,
    };
    assert_eq!(classify_post_edit_file_location(&file.to_string_lossy(), &opts), PostEditFileLocation::OutsideProject);
}

#[test]
fn test_classify_agent_dir_wins_in_git_repo() {
    let root = temp_root("lsp-postedit-agentdir-");
    create_dir_all(root.path().join(".git")).expect("mkdir");
    let agent_dir = root.path().join(".omo/agent");
    let file = agent_dir.join("models.json");
    write_exec(&file);
    let agent_dirs = vec![agent_dir.to_string_lossy().into_owned()];
    let opts = PostEditFileLocationOptions {
        cwd: &root.path().to_string_lossy(),
        agent_dirs: &agent_dirs,
        temp_dirs: Some(&[]),
        home_dirs: None,
    };
    assert_eq!(classify_post_edit_file_location(&file.to_string_lossy(), &opts), PostEditFileLocation::AgentConfig);
}

#[test]
fn test_classify_temp_dir_with_stray_package_json_is_temp() {
    let temp = temp_root("lsp-postedit-temp-");
    write(temp.path().join("package.json"), "{}\n").expect("write");
    let file = temp.path().join("rules_test.mjs");
    write_exec(&file);
    let temp_dirs = vec![temp.path().to_string_lossy().into_owned()];
    let opts = PostEditFileLocationOptions {
        cwd: &temp.path().to_string_lossy(),
        agent_dirs: &[],
        temp_dirs: Some(&temp_dirs),
        home_dirs: None,
    };
    assert_eq!(classify_post_edit_file_location(&file.to_string_lossy(), &opts), PostEditFileLocation::Temp);
}

#[test]
fn test_classify_os_temp_file_matches_temp() {
    let root = temp_root("lsp-postedit-ostemp-");
    let file = root.path().join("scratch.mjs");
    write_exec(&file);
    let opts = PostEditFileLocationOptions {
        cwd: &root.path().to_string_lossy(),
        agent_dirs: &[],
        temp_dirs: None,
        home_dirs: None,
    };
    assert_eq!(classify_post_edit_file_location(&file.to_string_lossy(), &opts), PostEditFileLocation::Temp);
}

#[test]
fn test_classify_markers_directly_in_home_are_not_project() {
    for marker in [".git", "package.json"] {
        let home = temp_root("lsp-postedit-home-");
        if marker == ".git" {
            create_dir_all(home.path().join(".git")).expect("mkdir");
        } else {
            write(home.path().join(marker), "{}\n").expect("write");
        }
        let top_level = home.path().join("notes.ts");
        write_exec(&top_level);
        let nested = home.path().join("scratch/draft.ts");
        write_exec(&nested);

        let home_dirs = vec![home.path().to_string_lossy().into_owned()];
        let opts = PostEditFileLocationOptions {
            cwd: &home.path().to_string_lossy(),
            agent_dirs: &[],
            temp_dirs: Some(&[]),
            home_dirs: Some(&home_dirs),
        };
        assert_eq!(classify_post_edit_file_location(&top_level.to_string_lossy(), &opts), PostEditFileLocation::OutsideProject);
        assert_eq!(classify_post_edit_file_location(&nested.to_string_lossy(), &opts), PostEditFileLocation::OutsideProject);
    }
}

#[test]
fn test_classify_marked_home_with_project_under_it_is_project() {
    let home = temp_root("lsp-postedit-homeproject-");
    create_dir_all(home.path().join(".git")).expect("mkdir");
    let app = home.path().join("work/app");
    create_dir_all(&app).expect("mkdir");
    write(app.join("package.json"), "{}\n").expect("write");
    let file = app.join("src/a.ts");
    write_exec(&file);

    let home_dirs = vec![home.path().to_string_lossy().into_owned()];
    let opts = PostEditFileLocationOptions {
        cwd: &home.path().to_string_lossy(),
        agent_dirs: &[],
        temp_dirs: Some(&[]),
        home_dirs: Some(&home_dirs),
    };
    assert_eq!(classify_post_edit_file_location(&file.to_string_lossy(), &opts), PostEditFileLocation::Project);
}

#[test]
fn test_classify_project_inside_temp_dir_is_project() {
    let temp = temp_root("lsp-postedit-tempproject-");
    let project = temp.path().join("checkout");
    create_dir_all(&project).expect("mkdir");
    write(project.join("package.json"), "{}\n").expect("write");
    let file = project.join("src/a.ts");
    write_exec(&file);

    let temp_dirs = vec![temp.path().to_string_lossy().into_owned()];
    let opts = PostEditFileLocationOptions {
        cwd: &project.to_string_lossy(),
        agent_dirs: &[],
        temp_dirs: Some(&temp_dirs),
        home_dirs: None,
    };
    assert_eq!(classify_post_edit_file_location(&file.to_string_lossy(), &opts), PostEditFileLocation::Project);
    assert_eq!(classify_post_edit_file_location("src/a.ts", &opts), PostEditFileLocation::Project);
}
