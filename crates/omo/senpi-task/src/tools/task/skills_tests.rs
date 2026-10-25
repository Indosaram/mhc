//! `tools/task/skills.test.ts`

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use crate::tools::task::skills::{
    FsSkillLoaderOptions, SkillDiscoveryOptions, build_skill_prepend, create_fs_skill_loader,
    load_skills_from_dir,
};
use crate::tools::task::types::LoadedSkill;

fn scratch(roots: &mut Vec<TempDir>) -> PathBuf {
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().to_path_buf();
    roots.push(root);
    path
}

fn write_skill(root: &Path, name: &str, body: &str) -> PathBuf {
    let skill_dir = root.join(name);
    fs::create_dir_all(&skill_dir).expect("mkdir");
    let skill_path = skill_dir.join("SKILL.md");
    fs::write(
        &skill_path,
        format!("---\nname: {name}\ndescription: {name} test skill\n---\n{body}\n"),
    )
    .expect("write");
    skill_path
}

fn names(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn as_str(path: &Path) -> &str {
    path.to_str().expect("utf8 path")
}

fn index_of(haystack: &str, needle: &str) -> usize {
    haystack.find(needle).expect("needle present")
}

#[test]
fn build_skill_prepend_wraps_each_skill_before_the_prompt() {
    // given
    let skills = vec![
        LoadedSkill {
            name: "alpha".to_string(),
            content: "ALPHA BODY".to_string(),
            location: None,
        },
        LoadedSkill {
            name: "beta".to_string(),
            content: "BETA BODY".to_string(),
            location: None,
        },
    ];

    // when
    let combined = build_skill_prepend(&skills, "the original prompt");

    // then
    assert!(combined.contains("ALPHA BODY"));
    assert!(combined.contains("BETA BODY"));
    assert!(index_of(&combined, "ALPHA BODY") < index_of(&combined, "the original prompt"));
    assert!(combined.ends_with("the original prompt"));
}

#[test]
fn build_skill_prepend_without_skills_returns_prompt_unchanged() {
    // when
    let combined = build_skill_prepend(&[], "just the prompt");

    // then
    assert_eq!(combined, "just the prompt");
}

#[test]
fn project_skill_dir_resolves_and_prepends_skill_content() {
    // given
    let mut roots = Vec::new();
    let cwd = scratch(&mut roots);
    let skill_dir = cwd.join(".senpi").join("skills").join("reviewer");
    fs::create_dir_all(&skill_dir).expect("mkdir");
    fs::write(skill_dir.join("SKILL.md"), "REVIEWER DIRECTIVE").expect("write");
    let loader = create_fs_skill_loader(FsSkillLoaderOptions {
        home_dir: Some(scratch(&mut roots)),
        ..FsSkillLoaderOptions::default()
    });

    // when
    let resolution = loader(&names(&["reviewer"]), as_str(&cwd));

    // then
    assert_eq!(resolution.resolved, names(&["reviewer"]));
    assert_eq!(resolution.missing, Vec::<String>::new());
    assert!(resolution.prepend.contains("REVIEWER DIRECTIVE"));
}

#[test]
fn missing_skill_is_reported_missing_with_empty_prepend() {
    // given
    let mut roots = Vec::new();
    let cwd = scratch(&mut roots);
    let loader = create_fs_skill_loader(FsSkillLoaderOptions {
        home_dir: Some(scratch(&mut roots)),
        ..FsSkillLoaderOptions::default()
    });

    // when
    let resolution = loader(&names(&["ghost"]), as_str(&cwd));

    // then
    assert_eq!(resolution.resolved, Vec::<String>::new());
    assert_eq!(resolution.missing, names(&["ghost"]));
    assert_eq!(resolution.prepend, "");
}

#[test]
fn extra_search_dir_resolves_skill() {
    // given
    let mut roots = Vec::new();
    let cwd = scratch(&mut roots);
    let plugin_root = scratch(&mut roots);
    let skill_dir = plugin_root
        .join("packages")
        .join("shared-skills")
        .join("commit");
    fs::create_dir_all(&skill_dir).expect("mkdir");
    fs::write(skill_dir.join("SKILL.md"), "COMMIT DIRECTIVE").expect("write");
    let loader = create_fs_skill_loader(FsSkillLoaderOptions {
        home_dir: Some(scratch(&mut roots)),
        extra_dirs: vec![plugin_root.join("packages").join("shared-skills")],
        ..FsSkillLoaderOptions::default()
    });

    // when
    let resolution = loader(&names(&["commit"]), as_str(&cwd));

    // then
    assert_eq!(resolution.resolved, names(&["commit"]));
    assert!(resolution.prepend.contains("COMMIT DIRECTIVE"));
}

#[test]
fn native_project_user_and_package_locations_resolve_in_request_order() {
    // given
    let mut roots = Vec::new();
    let project_root = scratch(&mut roots);
    fs::create_dir(project_root.join(".git")).expect("mkdir .git");
    let cwd = project_root.join("packages").join("app");
    fs::create_dir_all(&cwd).expect("mkdir cwd");
    let home_dir = scratch(&mut roots);
    let agent_dir = scratch(&mut roots);
    let package_dir = scratch(&mut roots);
    write_skill(
        &cwd.join(".senpi").join("skills"),
        "project-local",
        "PROJECT LOCAL",
    );
    write_skill(
        &project_root.join(".agents").join("skills"),
        "project-agent",
        "PROJECT AGENT",
    );
    write_skill(
        &cwd.join(".pi").join("skills"),
        "legacy-project",
        "LEGACY PROJECT",
    );
    write_skill(
        &agent_dir.join("skills"),
        "canonical-agent",
        "CANONICAL AGENT",
    );
    write_skill(
        &home_dir.join(".agents").join("skills"),
        "global-agent",
        "GLOBAL AGENT",
    );
    write_skill(&package_dir, "packaged", "PACKAGED");
    let loader = create_fs_skill_loader(FsSkillLoaderOptions {
        home_dir: Some(home_dir),
        agent_dir: Some(agent_dir),
        extra_dirs: vec![package_dir],
        ..FsSkillLoaderOptions::default()
    });
    let requested = names(&[
        "packaged",
        "global-agent",
        "canonical-agent",
        "legacy-project",
        "project-agent",
        "project-local",
    ]);

    // when
    let resolution = loader(&requested, as_str(&cwd));

    // then
    assert_eq!(resolution.resolved, requested);
    assert_eq!(resolution.missing, Vec::<String>::new());
    assert!(
        index_of(&resolution.prepend, "PACKAGED") < index_of(&resolution.prepend, "GLOBAL AGENT")
    );
    assert!(
        index_of(&resolution.prepend, "GLOBAL AGENT")
            < index_of(&resolution.prepend, "PROJECT LOCAL")
    );
}

#[test]
fn direct_markdown_skill_injects_only_body_and_location() {
    // given
    let mut roots = Vec::new();
    let cwd = scratch(&mut roots);
    let skills_dir = cwd.join(".senpi").join("skills");
    fs::create_dir_all(&skills_dir).expect("mkdir");
    let skill_path = skills_dir.join("direct.md");
    fs::write(
        &skill_path,
        "---\nname: direct\ndescription: Direct root skill\n---\nDIRECT BODY\n",
    )
    .expect("write");
    let loader = create_fs_skill_loader(FsSkillLoaderOptions {
        home_dir: Some(scratch(&mut roots)),
        ..FsSkillLoaderOptions::default()
    });

    // when
    let resolution = loader(&names(&["direct"]), as_str(&cwd));

    // then
    assert_eq!(resolution.resolved, names(&["direct"]));
    assert!(resolution.prepend.contains(&format!(
        "<skill name=\"direct\" location=\"{}\">",
        skill_path.display()
    )));
    assert!(resolution.prepend.contains(&format!(
        "References are relative to {}.",
        skills_dir.display()
    )));
    assert!(resolution.prepend.contains("DIRECT BODY"));
    assert!(
        !resolution
            .prepend
            .contains("description: Direct root skill")
    );
}

#[test]
fn indirect_skill_names_discover_each_existing_directory_once() {
    // given
    let mut roots = Vec::new();
    let project_root = scratch(&mut roots);
    fs::create_dir(project_root.join(".git")).expect("mkdir .git");
    let cwd = project_root.join("app");
    fs::create_dir(&cwd).expect("mkdir cwd");
    let project_skills = cwd.join(".senpi").join("skills");
    let home_dir = scratch(&mut roots);
    let global_skills = home_dir.join(".agents").join("skills");
    let agent_dir = scratch(&mut roots);
    let agent_skills = agent_dir.join("skills");
    let package_skills = scratch(&mut roots);
    let aliased_dir = package_skills.join("nested");
    for dir in [&project_skills, &global_skills, &agent_skills, &aliased_dir] {
        fs::create_dir_all(dir).expect("mkdir");
    }
    fs::write(
        aliased_dir.join("SKILL.md"),
        "---\nname: aliased\ndescription: Aliased nested skill\n---\nALIASED BODY\n",
    )
    .expect("write");
    let scans: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
    let recorded = Arc::clone(&scans);
    let loader = create_fs_skill_loader(FsSkillLoaderOptions {
        home_dir: Some(home_dir),
        agent_dir: Some(agent_dir),
        extra_dirs: vec![package_skills.clone()],
        load_skills_from_dir: Some(Arc::new(move |options: &SkillDiscoveryOptions<'_>| {
            recorded
                .lock()
                .expect("scans lock")
                .push(options.dir.to_path_buf());
            load_skills_from_dir(options)
        })),
    });

    // when
    let resolution = loader(
        &names(&["missing-one", "aliased", "missing-two"]),
        as_str(&cwd),
    );

    // then
    assert_eq!(resolution.resolved, names(&["aliased"]));
    assert_eq!(resolution.missing, names(&["missing-one", "missing-two"]));
    assert_eq!(
        scans.lock().expect("scans lock").clone(),
        vec![agent_skills, global_skills, package_skills]
    );
}

#[cfg(unix)]
#[test]
fn project_links_are_rejected_by_lexical_name_but_user_links_are_allowed() {
    use std::os::unix::fs::symlink;
    let project = tempfile::tempdir().expect("project");
    let home = tempfile::tempdir().expect("home");
    let outside = tempfile::tempdir().expect("outside");
    fs::create_dir(project.path().join(".git")).expect("git");
    let project_skills = project.path().join(".senpi/skills");
    fs::create_dir_all(&project_skills).expect("skills");
    let external = write_skill(outside.path(), "escape", "outside");
    symlink(
        external.parent().expect("parent"),
        project_skills.join("escape"),
    )
    .expect("link");
    let loader = create_fs_skill_loader(FsSkillLoaderOptions {
        home_dir: Some(home.path().to_path_buf()),
        ..Default::default()
    });
    let resolution = loader(&names(&["escape", "unrelated"]), as_str(project.path()));
    assert_eq!(resolution.missing, names(&["unrelated"]));
    assert_eq!(
        resolution.rejected.as_ref().expect("rejected")[0].name,
        "escape"
    );
    assert!(resolution.resolved.is_empty());
    let user = home.path().join(".agents/skills");
    fs::create_dir_all(&user).expect("user");
    symlink(external.parent().expect("parent"), user.join("escape")).expect("user link");
    let resolved = loader(&names(&["escape"]), as_str(project.path()));
    assert_eq!(resolved.resolved, names(&["escape"]));
    assert!(resolved.rejected.expect("rejected").is_empty());
}

#[test]
fn root_skill_stops_discovery_and_direct_reference_lookup() {
    let project = tempfile::tempdir().expect("project");
    let home = tempfile::tempdir().expect("home");
    fs::create_dir(project.path().join(".git")).expect("git");
    let dir = project.path().join(".senpi/skills");
    fs::create_dir_all(&dir).expect("skills");
    fs::write(dir.join("SKILL.md"), "---\nname: root\n---\nroot body").expect("root skill");
    write_skill(&dir, "reference", "not a skill");
    fs::write(dir.join("README.md"), "not a skill").expect("readme");
    let loader = create_fs_skill_loader(FsSkillLoaderOptions {
        home_dir: Some(home.path().to_path_buf()),
        ..Default::default()
    });
    let result = loader(
        &names(&["root", "reference", "readme"]),
        as_str(project.path()),
    );
    assert_eq!(result.resolved, names(&["root"]));
    assert_eq!(result.missing, names(&["reference", "readme"]));
}

#[cfg(unix)]
#[test]
fn checked_scan_preserves_original_io_error_separately_from_blocked_links() {
    use super::checked_project_skills::{SkillReadError, read_project_skills};
    use std::os::unix::fs::symlink;
    let project = tempfile::tempdir().expect("project");
    let skills = project.path().join("skills");
    fs::create_dir(&skills).expect("skills");
    symlink(project.path().join("absent"), skills.join("broken")).expect("dangling link");
    let scan = read_project_skills(&skills, project.path(), &[]);
    assert!(scan.blocked.is_empty());
    assert!(
        matches!(&scan.errors[0], SkillReadError::Io { error, .. } if error.kind() == std::io::ErrorKind::NotFound)
    );
}

#[cfg(unix)]
#[test]
fn escaping_folder_file_and_root_links_are_checked_before_read() {
    use std::os::unix::fs::symlink;
    for kind in ["folder", "file", "root"] {
        let project = tempfile::tempdir().expect("project");
        let home = tempfile::tempdir().expect("home");
        let outside = tempfile::tempdir().expect("outside");
        fs::create_dir(project.path().join(".git")).expect("git");
        let dir = project.path().join(".senpi/skills");
        fs::create_dir_all(dir.parent().expect("parent")).expect("parent");
        let external = write_skill(outside.path(), "escape", "external");
        match kind {
            "root" => symlink(outside.path(), &dir).expect("root link"),
            "folder" => {
                fs::create_dir(&dir).expect("skills");
                symlink(external.parent().expect("parent"), dir.join("escape"))
                    .expect("folder link");
            }
            _ => {
                fs::create_dir_all(dir.join("escape")).expect("skill");
                symlink(&external, dir.join("escape/SKILL.md")).expect("file link");
            }
        }
        let loader = create_fs_skill_loader(FsSkillLoaderOptions {
            home_dir: Some(home.path().to_path_buf()),
            ..Default::default()
        });
        let result = loader(&names(&["escape"]), as_str(project.path()));
        assert!(result.resolved.is_empty(), "{kind}");
        assert!(result.missing.is_empty(), "{kind}");
        assert_eq!(
            result.rejected.expect("rejections")[0].name,
            "escape",
            "{kind}"
        );
    }
}

#[cfg(unix)]
#[test]
fn nested_repository_links_are_allowed_cycles_stop_and_blocked_aliases_are_not_attributed() {
    use std::os::unix::fs::symlink;
    let project = tempfile::tempdir().expect("project");
    let home = tempfile::tempdir().expect("home");
    let outside = tempfile::tempdir().expect("outside");
    fs::create_dir(project.path().join(".git")).expect("git");
    let cwd = project.path().join("packages/app");
    let dir = cwd.join(".senpi/skills");
    fs::create_dir_all(&dir).expect("skills");
    let shared = write_skill(&project.path().join("shared"), "inside", "internal");
    symlink(shared.parent().expect("parent"), dir.join("inside")).expect("internal link");
    symlink(&dir, dir.join("cycle")).expect("cycle");
    let escaped = write_skill(outside.path(), "alias", "external");
    symlink(escaped.parent().expect("parent"), dir.join("lexical")).expect("escape link");
    let loader = create_fs_skill_loader(FsSkillLoaderOptions {
        home_dir: Some(home.path().to_path_buf()),
        ..Default::default()
    });
    let result = loader(&names(&["inside", "alias", "lexical"]), as_str(&cwd));
    assert_eq!(result.resolved, names(&["inside"]));
    assert_eq!(result.missing, names(&["alias"]));
    assert_eq!(result.rejected.expect("rejected")[0].name, "lexical");
}
