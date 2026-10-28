//! Tests written from docs/cleanroom/specs/skills.md §7.

use maho_ext_api::{
    EventBus, EventKind, Extension, ExtensionApi, ExtensionRuntime, ExtensionSessionProfile,
    LoadedExtension, Skill, SourceInfo, SourceScope,
};
use maho_skills::{
    SkillsExtension, bundled_skills_root_from, detail_text, filter_prompt, list_text,
    run_skill_tool, strip_front_matter,
};
use serde_json::json;
use std::path::{Path, PathBuf};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

fn skill_dir(root: &Path, name: &str, description: &str, extra: &str) -> Skill {
    let dir = root.join(name);
    let file = dir.join("SKILL.md");
    write(
        &file,
        &format!(
            "---\nname: {name}\ndescription: {description}\n{extra}---\n\n# {name}\n\nDo the {name} thing. See scripts/run.sh.\n"
        ),
    );
    write(&dir.join("scripts/run.sh"), "echo run\n");
    write(&dir.join("references/notes.md"), "notes body\n");
    Skill {
        name: name.into(),
        description: description.into(),
        file_path: file.to_string_lossy().into_owned(),
        base_dir: dir.to_string_lossy().into_owned(),
        source_info: SourceInfo {
            scope: SourceScope::User,
            ..SourceInfo::default()
        },
        disable_model_invocation: extra.contains("disable-model-invocation: true"),
    }
}

fn core_skill(skill: &Skill) -> maho_core::skills::Skill {
    maho_core::skills::Skill {
        name: skill.name.clone(),
        description: skill.description.clone(),
        file_path: skill.file_path.clone(),
        base_dir: skill.base_dir.clone(),
        source_info: maho_core::source_info::SourceInfo::default(),
        disable_model_invocation: skill.disable_model_invocation,
    }
}

#[test]
fn bundled_root_prefers_the_env_then_the_executable_sibling() {
    let dir = tempfile::tempdir().expect("dir");
    let env_root = dir.path().join("env-skills");
    std::fs::create_dir_all(&env_root).expect("mkdir");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(bin.join("skills")).expect("mkdir");
    let exe = bin.join("mhc");
    assert_eq!(
        bundled_skills_root_from(Some(&env_root), Some(&exe)),
        Some(env_root.clone())
    );
    assert_eq!(
        bundled_skills_root_from(Some(Path::new("")), Some(&exe)),
        Some(bin.join("skills"))
    );
    assert_eq!(
        bundled_skills_root_from(Some(&dir.path().join("missing")), Some(&exe)),
        Some(bin.join("skills"))
    );
    assert_eq!(
        bundled_skills_root_from(None, Some(&dir.path().join("mhc"))),
        None
    );
    assert_eq!(bundled_skills_root_from(None, None), None);
}

#[test]
fn front_matter_is_stripped() {
    assert_eq!(strip_front_matter("---\nname: a\n---\n\nbody\n"), "body\n");
    assert_eq!(strip_front_matter("---\r\nname: a\r\n---\r\nbody"), "body");
    assert_eq!(strip_front_matter("no front matter"), "no front matter");
    assert_eq!(strip_front_matter("---\nunterminated"), "---\nunterminated");
}

#[test]
fn disabled_entries_leave_the_prompt_listing_and_nothing_else() {
    let dir = tempfile::tempdir().expect("dir");
    let alpha = skill_dir(dir.path(), "alpha", "First skill", "");
    let beta = skill_dir(dir.path(), "beta", "Second skill", "");
    let listing = maho_core::skills::format_skills_for_prompt(
        &[core_skill(&alpha), core_skill(&beta)],
        maho_core::skills::FileReadTool::Read,
    );
    let prompt = format!("You are helpful.{listing}\n\nTrailer line.");
    assert_eq!(filter_prompt(&prompt, &[]), None);
    assert_eq!(filter_prompt(&prompt, &["gamma".into()]), None);
    let filtered = filter_prompt(&prompt, &["alpha".into()]).expect("changed");
    assert!(!filtered.contains("<name>alpha</name>"));
    assert!(filtered.contains("<name>beta</name>"));
    // Exactly the alpha entry (5 lines) was removed.
    let alpha_entry: String = prompt
        .split_inclusive('\n')
        .skip_while(|line| line.trim() != "<skill>")
        .take(5)
        .collect();
    assert!(alpha_entry.contains("<name>alpha</name>"));
    assert_eq!(filtered, prompt.replacen(&alpha_entry, "", 1));
    assert!(filtered.ends_with("\n\nTrailer line."));
}

#[test]
fn skill_tool_loads_the_body_in_the_invocation_wrapper() {
    let dir = tempfile::tempdir().expect("dir");
    let alpha = skill_dir(dir.path(), "alpha", "First skill", "");
    let out = run_skill_tool(std::slice::from_ref(&alpha), &[], &json!({"name": "alpha"}));
    assert!(!out.is_error, "{}", out.text);
    assert_eq!(
        out.text,
        format!(
            "<skill name=\"alpha\" location=\"{}\">\nReferences are relative to {}.\n\n# alpha\n\nDo the alpha thing. See scripts/run.sh.\n</skill>",
            alpha.file_path, alpha.base_dir
        )
    );
    assert_eq!(out.details["baseDir"], json!(alpha.base_dir));
    let with_args = run_skill_tool(
        &[alpha],
        &[],
        &json!({"name": "alpha", "arguments": " focus on tests "}),
    );
    assert!(with_args.text.ends_with("</skill>\n\nUser: focus on tests"));
}

#[test]
fn skill_tool_errors() {
    let dir = tempfile::tempdir().expect("dir");
    let alpha = skill_dir(dir.path(), "alpha", "First skill", "");
    let beta = skill_dir(
        dir.path(),
        "beta",
        "Second",
        "disable-model-invocation: true\n",
    );
    let gamma = skill_dir(dir.path(), "gamma", "Third", "");
    let skills = [alpha, beta, gamma];
    let disabled = vec!["gamma".to_owned()];
    let unknown = run_skill_tool(&skills, &disabled, &json!({"name": "nope"}));
    assert!(unknown.is_error);
    assert_eq!(unknown.text, "Unknown skill: nope\nAvailable: alpha");
    let off = run_skill_tool(&skills, &disabled, &json!({"name": "gamma"}));
    assert!(off.is_error && off.text.contains("disabled by configuration"));
    let user_only = run_skill_tool(&skills, &disabled, &json!({"name": "beta"}));
    assert!(user_only.is_error && user_only.text.contains("/skill:beta"));
    let missing = run_skill_tool(&skills, &disabled, &json!({}));
    assert!(missing.is_error);
    let none = run_skill_tool(&[], &[], &json!({"name": "x"}));
    assert_eq!(none.text, "Unknown skill: x\nAvailable: none");
}

#[test]
fn skill_tool_reads_and_lists_inside_the_skill_only() {
    let dir = tempfile::tempdir().expect("dir");
    let alpha = skill_dir(dir.path(), "alpha", "First skill", "");
    let skills = [alpha.clone()];
    let file = run_skill_tool(&skills, &[], &json!({"name": "alpha/references/notes.md"}));
    assert!(!file.is_error, "{}", file.text);
    assert_eq!(file.text, "[alpha/references/notes.md]\nnotes body\n");
    let listing = run_skill_tool(&skills, &[], &json!({"name": "alpha/scripts"}));
    assert_eq!(listing.text, "[alpha/scripts/]\nrun.sh");
    let root = run_skill_tool(&skills, &[], &json!({"name": "alpha/."}));
    assert_eq!(root.text, "[alpha/./]\nSKILL.md\nreferences/\nscripts/");
    for bad in [
        "alpha/../beta",
        "alpha/references/../../x",
        "alpha//etc/passwd",
    ] {
        let out = run_skill_tool(&skills, &[], &json!({"name": bad}));
        assert!(out.is_error, "{bad}: {}", out.text);
    }
    let absent = run_skill_tool(&skills, &[], &json!({"name": "alpha/none.md"}));
    assert_eq!(absent.text, "Not found: alpha/none.md");
    #[cfg(unix)]
    {
        write(&dir.path().join("secret.txt"), "secret");
        std::os::unix::fs::symlink(
            dir.path().join("secret.txt"),
            PathBuf::from(&alpha.base_dir).join("link.txt"),
        )
        .expect("symlink");
        let escaped = run_skill_tool(&skills, &[], &json!({"name": "alpha/link.txt"}));
        assert!(escaped.is_error);
        assert_eq!(escaped.text, "Path resolves outside the skill directory");
    }
    let big = PathBuf::from(&alpha.base_dir).join("big.bin");
    std::fs::write(&big, vec![b'x'; 300 * 1024]).expect("write");
    let refused = run_skill_tool(&skills, &[], &json!({"name": "alpha/big.bin"}));
    assert!(refused.is_error && refused.text.contains("307200 bytes"));
}

#[test]
fn command_text_lists_and_details() {
    let dir = tempfile::tempdir().expect("dir");
    let bundled = dir.path().join("bundled");
    let alpha = skill_dir(dir.path(), "alpha", "First skill", "");
    let beta = skill_dir(
        &bundled,
        "beta",
        "Second",
        "disable-model-invocation: true\n",
    );
    let skills = [alpha.clone(), beta];
    let disabled = vec!["alpha".to_owned()];
    let list = list_text(&skills, &disabled, Some(&bundled));
    assert_eq!(
        list.lines().collect::<Vec<_>>()[..2],
        [
            "Skills (2):",
            format!("  alpha (user) [disabled] - {}", alpha.file_path).as_str()
        ]
    );
    assert!(list.contains("  beta (bundled) [user-only] - "));
    assert_eq!(list_text(&[], &[], None), "No skills loaded.");
    let detail = detail_text(&skills, &disabled, None, "alpha");
    assert!(detail.starts_with("alpha\nFirst skill\nScope: user\nStatus: disabled\n"));
    assert!(detail.ends_with("Run: /skill:alpha [instructions]"));
    assert_eq!(
        detail_text(&skills, &[], None, "zzz"),
        "Unknown skill: zzz\nAvailable: alpha, beta"
    );
}

#[test]
fn extension_registers_tool_command_and_handlers() {
    let dir = tempfile::tempdir().expect("dir");
    let mut api = ExtensionApi::new(
        LoadedExtension::new("<builtin:skills>", dir.path().into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    SkillsExtension {
        bundled_root: Some(dir.path().to_path_buf()),
    }
    .register(&mut api);
    assert!(
        api.registered
            .tools
            .iter()
            .any(|t| t.definition.name == "skill")
    );
    assert!(api.registered.commands.iter().any(|c| c.name == "skill"));
    assert_eq!(
        api.registered.handlers[&EventKind::ResourcesDiscover].len(),
        1
    );
    assert_eq!(
        api.registered.handlers[&EventKind::BeforeAgentStart].len(),
        1
    );
}
