mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use maho_ext_api::{ResourcesDiscoverEvent, SessionReason, SlashCommandInfo, SourceInfo};
use maho_omo_bundled_skills::{
    ContributedSkill, HostCommandInfo, ResolveContributedSkillOptions, host_command_info,
    read_discover_cwd, resolve_contributed_skill,
};

fn skill_command(name: &str, owner_path: Option<&Path>) -> HostCommandInfo {
    HostCommandInfo {
        name: format!("skill:{name}"),
        description: Some("loaded".to_owned()),
        source: "skill".to_owned(),
        source_info_path: owner_path.map(|path| path.to_string_lossy().into_owned()),
    }
}

fn resolve(name: &str, path: &Path, home: &Path, env: &BTreeMap<String, String>, commands: Option<&[HostCommandInfo]>) -> ContributedSkill {
    resolve_contributed_skill(ResolveContributedSkillOptions {
        commands,
        name,
        path: path.to_path_buf(),
        cwd: home.to_path_buf(),
        env,
    })
}

#[test]
fn given_disabled_skills_hides_the_name_when_resolved_then_the_skill_is_disabled() {
    let root = tempfile::tempdir().expect("temp");
    let home = root.path().join("home");
    std::fs::create_dir_all(home.join(".maho")).expect("config dir");
    std::fs::write(home.join(".maho/omo.jsonc"), "{\"disabled_skills\":[\"computer-use\"]}").expect("config");
    let env = support::env_for_home(&home);
    let path = root.path().join("computer-use/SKILL.md");

    assert_eq!(resolve("computer-use", &path, &home, &env, None), ContributedSkill::Disabled);
}

#[test]
fn given_our_own_copy_is_already_loaded_when_resolved_then_it_stays_contributed() {
    let root = tempfile::tempdir().expect("temp");
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let env = support::env_for_home(&home);
    let path = root.path().join("x-search/SKILL.md");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("skill dir");
    std::fs::write(&path, "---\nname: x-search\n---\n").expect("skill file");
    let commands = [skill_command("x-search", Some(&path))];

    assert_eq!(
        resolve("x-search", &path, &home, &env, Some(commands.as_slice())),
        ContributedSkill::Contributed { path }
    );
}

#[test]
fn given_a_same_name_skill_is_loaded_from_elsewhere_when_resolved_then_it_yields_the_owner_path() {
    let root = tempfile::tempdir().expect("temp");
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let env = support::env_for_home(&home);
    let ours = root.path().join("ours/x-search/SKILL.md");
    let theirs = root.path().join("theirs/x-search/SKILL.md");
    let commands = [skill_command("x-search", Some(&theirs))];

    assert_eq!(
        resolve("x-search", &ours, &home, &env, Some(commands.as_slice())),
        ContributedSkill::Yielded { owner_path: Some(theirs.to_string_lossy().into_owned()) }
    );
}

#[test]
fn given_a_loaded_skill_command_without_a_source_path_when_resolved_then_it_yields_without_an_owner() {
    let root = tempfile::tempdir().expect("temp");
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let env = support::env_for_home(&home);
    let path = root.path().join("ours/x-search/SKILL.md");
    let commands = [skill_command("x-search", None)];

    assert_eq!(
        resolve("x-search", &path, &home, &env, Some(commands.as_slice())),
        ContributedSkill::Yielded { owner_path: None }
    );
}

#[test]
fn given_a_host_without_get_commands_when_resolved_then_the_skill_is_contributed() {
    let root = tempfile::tempdir().expect("temp");
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let env = support::env_for_home(&home);
    let path = root.path().join("x-search/SKILL.md");

    assert_eq!(
        resolve("x-search", &path, &home, &env, None),
        ContributedSkill::Contributed { path }
    );
}

#[test]
fn given_no_skill_command_for_the_name_when_resolved_then_the_skill_is_contributed() {
    let root = tempfile::tempdir().expect("temp");
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let env = support::env_for_home(&home);
    let path = root.path().join("x-search/SKILL.md");
    let commands = [HostCommandInfo { name: "tasks".to_owned(), description: None, source: "extension".to_owned(), source_info_path: None }];

    assert_eq!(
        resolve("x-search", &path, &home, &env, Some(commands.as_slice())),
        ContributedSkill::Contributed { path }
    );
}

#[test]
fn given_a_host_command_when_mapped_then_name_description_source_and_path_are_carried() {
    let command = SlashCommandInfo {
        name: "skill:ulw-plan".to_owned(),
        description: Some("Plans first.".to_owned()),
        argument_hint: Some("[request]".to_owned()),
        source_info: Some(SourceInfo { path: "/skills/ulw-plan/SKILL.md".to_owned(), source: "skill".to_owned(), ..Default::default() }),
    };

    assert_eq!(
        host_command_info(&command),
        HostCommandInfo {
            name: "skill:ulw-plan".to_owned(),
            description: Some("Plans first.".to_owned()),
            source: "skill".to_owned(),
            source_info_path: Some("/skills/ulw-plan/SKILL.md".to_owned()),
        }
    );
}

#[test]
fn given_a_discover_event_when_the_cwd_is_read_then_a_set_cwd_is_returned_and_an_empty_one_is_absent() {
    let set = ResourcesDiscoverEvent { cwd: PathBuf::from("/project"), reason: SessionReason::Startup, scoped_entries: true };
    assert_eq!(read_discover_cwd(&set), Some(PathBuf::from("/project")));

    let empty = ResourcesDiscoverEvent { cwd: PathBuf::new(), reason: SessionReason::Reload, scoped_entries: true };
    assert_eq!(read_discover_cwd(&empty), None);
}
