use std::collections::BTreeSet;

use maho_omo_bundled_skills::HostCommandInfo;
use maho_omo_skill_commands::{
    BareSkillCommand, BareSkillCommandResolution, parse_bare_skill_command, read_bundled_skill_names,
    resolve_bare_skill_command,
};

fn names() -> BTreeSet<String> {
    ["ulw-execute", "ulw-loop", "2fa-setup"].iter().map(|name| (*name).to_owned()).collect()
}

fn command(name: &str, source: &str) -> HostCommandInfo {
    HostCommandInfo { name: name.to_owned(), description: None, source: source.to_owned(), source_info_path: None }
}

#[test]
fn given_a_bundled_skill_command_when_parsed_then_the_name_and_every_argument_are_split() {
    assert_eq!(
        parse_bare_skill_command("/ulw-execute demo-plan --make-pr", &names()),
        Some(BareSkillCommand { name: "ulw-execute".to_owned(), rest: " demo-plan --make-pr".to_owned() })
    );
    assert_eq!(
        parse_bare_skill_command("/ulw-loop fix it\nsecond line", &names()),
        Some(BareSkillCommand { name: "ulw-loop".to_owned(), rest: " fix it\nsecond line".to_owned() })
    );
    assert_eq!(
        parse_bare_skill_command("/ulw-execute", &names()),
        Some(BareSkillCommand { name: "ulw-execute".to_owned(), rest: String::new() })
    );
    assert_eq!(
        parse_bare_skill_command("/2fa-setup now", &names()),
        Some(BareSkillCommand { name: "2fa-setup".to_owned(), rest: " now".to_owned() })
    );
}

#[test]
fn given_a_text_that_is_not_a_bare_bundled_skill_command_when_parsed_then_it_is_not_a_command() {
    for text in [
        "/ulw-executex plan",
        "/ulw-execute_extra plan",
        "/ULW-EXECUTE plan",
        " /ulw-execute plan",
        "run /ulw-execute plan",
        "/skill:ulw-execute plan",
        "/tasks",
        "/-x plan",
        "/",
    ] {
        assert_eq!(parse_bare_skill_command(text, &names()), None, "{text}");
    }
}

#[test]
fn given_a_loaded_bundled_skill_when_resolved_then_the_command_expands_to_the_skill_form() {
    let commands = [command("skill:ulw-execute", "skill")];
    assert_eq!(
        resolve_bare_skill_command("/ulw-execute demo-plan", &names(), Some(commands.as_slice())),
        BareSkillCommandResolution::Expand { text: "/skill:ulw-execute demo-plan".to_owned() }
    );
}

#[test]
fn given_another_command_owns_the_name_when_resolved_then_the_alias_is_shadowed() {
    let commands = [command("ulw-execute", "prompt"), command("skill:ulw-execute", "skill")];
    assert_eq!(
        resolve_bare_skill_command("/ulw-execute demo-plan", &names(), Some(commands.as_slice())),
        BareSkillCommandResolution::Shadowed
    );
}

#[test]
fn given_the_skill_command_is_not_loaded_when_resolved_then_it_is_unavailable() {
    let commands = [command("tasks", "extension")];
    assert_eq!(
        resolve_bare_skill_command("/ulw-execute demo-plan", &names(), Some(commands.as_slice())),
        BareSkillCommandResolution::Unavailable { name: "ulw-execute".to_owned() }
    );
}

#[test]
fn given_a_host_without_get_commands_when_resolved_then_the_command_still_expands() {
    assert_eq!(
        resolve_bare_skill_command("/ulw-execute demo-plan", &names(), None),
        BareSkillCommandResolution::Expand { text: "/skill:ulw-execute demo-plan".to_owned() }
    );
}

#[test]
fn given_a_text_that_names_no_bundled_skill_when_resolved_then_it_is_not_a_bare_skill() {
    let commands = [command("skill:foo", "skill")];
    assert_eq!(
        resolve_bare_skill_command("/foo bar", &names(), Some(commands.as_slice())),
        BareSkillCommandResolution::NotBareSkill
    );
}

#[test]
fn given_a_skills_root_when_read_then_only_directories_holding_a_skill_md_are_named() {
    let root = tempfile::tempdir().expect("temp");
    let skills = root.path().join("skills");
    for name in ["ulw-execute", "ulw-plan"] {
        std::fs::create_dir_all(skills.join(name)).expect("skill dir");
        std::fs::write(skills.join(name).join("SKILL.md"), "---\nname: fixture\n---\n").expect("skill file");
    }
    std::fs::create_dir_all(skills.join("not-a-skill")).expect("plain directory");

    let read = read_bundled_skill_names(Some(&skills)).expect("read");
    assert_eq!(read, ["ulw-execute".to_owned(), "ulw-plan".to_owned()].into_iter().collect::<BTreeSet<_>>());
}

#[test]
fn given_no_or_missing_skills_root_when_read_then_no_bundled_skill_is_named() {
    let root = tempfile::tempdir().expect("temp");
    assert!(read_bundled_skill_names(None).expect("read").is_empty());
    assert!(read_bundled_skill_names(Some(&root.path().join("missing"))).expect("read").is_empty());
}
