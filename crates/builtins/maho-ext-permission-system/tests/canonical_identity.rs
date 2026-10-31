//! Port of test/permission/canonical-identity.test.ts and 4351-physical-walk.test.ts (senpi#4311, #4351).
#![cfg(unix)]

use maho_ext_permission_system::{
    config::rules_for_preset,
    events::PermissionEventEmitter,
    external_dir::is_external_path,
    parsers::{PermissionRequest, create_builtin_parser_registry},
    prompt::format_request_for_display,
    service::{DecisionOptions, PermissionService},
    types::*,
};
use serde_json::json;
use std::{
    os::unix::fs::symlink,
    path::{Path, PathBuf},
};

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    cwd: PathBuf,
    outside: PathBuf,
    alias: PathBuf,
}
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("root");
    let root = std::fs::canonicalize(dir.path()).expect("canonical root");
    let (cwd, outside, alias) = (
        root.join("Project"),
        root.join("outside"),
        root.join("alias"),
    );
    std::fs::create_dir(&cwd).expect("cwd");
    std::fs::create_dir(&outside).expect("outside");
    symlink(&outside, &alias).expect("alias");
    std::fs::write(outside.join("file.txt"), "fixture").expect("file");
    std::fs::write(outside.join("f.txt"), "fixture").expect("f.txt");
    Fixture {
        _dir: dir,
        root,
        cwd,
        outside,
        alias,
    }
}
fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
fn rule(pattern: &str, action: Action) -> Rule {
    Rule {
        permission: "external_directory".into(),
        pattern: pattern.into(),
        action,
    }
}
fn external(tool: &str, input: serde_json::Value, cwd: &Path) -> PermissionRequest {
    create_builtin_parser_registry()
        .parse(tool, &input, (cwd, Path::new("/home/user")))
        .into_iter()
        .find(|request| request.permission == "external_directory")
        .expect("external request")
}
fn decide(service: &PermissionService, request: &PermissionRequest) -> Action {
    let options = DecisionOptions {
        rule_aliases: request.rule_aliases.clone(),
        original_paths: request.original_paths.clone(),
    };
    let mut action = Action::Allow;
    for pattern in &request.patterns {
        match service
            .decide("external_directory", pattern, &options)
            .action
        {
            Action::Deny => return Action::Deny,
            Action::Ask => action = Action::Ask,
            Action::Allow => {}
        }
    }
    action
}

#[test]
fn an_alias_approval_cannot_bypass_a_real_target_deny() {
    let f = fixture();
    let target = text(&f.outside.join("file.txt"));
    let aliased = text(&f.alias.join("file.txt"));
    for tool in [
        "write",
        "edit",
        "multiedit",
        "apply_patch",
        "read",
        "grep",
        "ls",
        "find",
        "glob",
        "monitor",
        "bash",
    ] {
        let input = if tool == "bash" {
            json!({ "command": format!("cat \"{aliased}\"") })
        } else {
            json!({ "path": aliased })
        };
        let request = external(tool, input, &f.cwd);
        let mut rules = rules_for_preset(PermissionPresetName::AcceptEdits);
        rules.push(rule(&format!("{}/*", text(&f.outside)), Action::Deny));
        let service = PermissionService::new(
            rules,
            vec![rule(&format!("{}/*", text(&f.alias)), Action::Allow)],
            PermissionEventEmitter::default(),
        );
        assert_eq!(decide(&service, &request), Action::Deny, "{tool}");
        assert_eq!(request.patterns, vec![target.clone()], "{tool}");
    }
}

#[test]
fn the_resolved_parent_of_a_missing_target_is_remembered() {
    let f = fixture();
    let aliased = text(&f.alias.join("missing/nested/file.txt"));
    let request = external("write", json!({ "path": aliased }), &f.cwd);
    assert_eq!(
        request.patterns,
        vec![text(&f.outside.join("missing/nested/file.txt"))]
    );
    assert_eq!(
        request.always,
        vec![format!("{}/*", text(&f.outside.join("missing/nested")))]
    );
    let approved: Ruleset = request
        .always
        .iter()
        .map(|pattern| rule(pattern, Action::Allow))
        .collect();
    let service = PermissionService::new(
        rules_for_preset(PermissionPresetName::AcceptEdits),
        approved,
        PermissionEventEmitter::default(),
    );
    assert_eq!(decide(&service, &request), Action::Allow);
    // The approval names the original target, not wherever the alias points next.
    std::fs::remove_file(&f.alias).expect("unlink alias");
    let other = f.root.join("other");
    std::fs::create_dir(&other).expect("other");
    symlink(&other, &f.alias).expect("re-alias");
    let changed = external("write", json!({ "path": aliased }), &f.cwd);
    assert_eq!(decide(&service, &changed), Action::Ask);
}

#[test]
fn the_prompt_shows_every_resolved_literal_target_and_its_spelling() {
    let f = fixture();
    for count in [1, 2] {
        let aliases = [
            text(&f.alias.join("legal*name.txt")),
            text(&f.alias.join("second.txt")),
        ];
        let targets = [
            text(&f.outside.join("legal*name.txt")),
            text(&f.outside.join("second.txt")),
        ];
        let command = format!(
            "cat {}",
            aliases[..count]
                .iter()
                .map(|path| format!("\"{path}\""))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let parsed = external("bash", json!({ "command": command }), &f.cwd);
        let mut request = Request::new(
            "identity".into(),
            "identity".into(),
            parsed.permission,
            parsed.patterns,
            parsed.always,
            Default::default(),
        );
        request.original_paths = parsed.original_paths;
        request
            .metadata
            .insert("parentDir".into(), text(&f.cwd).into());
        let shown = format_request_for_display(&request);
        let directories: Vec<&str> = shown
            .lines()
            .filter_map(|line| line.strip_prefix("Directory: "))
            .collect();
        assert_eq!(
            directories,
            targets[..count]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        );
        for path in &aliases[..count] {
            assert!(shown.contains(path.as_str()), "{path}");
        }
    }
}

#[test]
fn distinct_case_sensitive_directories_stay_separate() {
    let f = fixture();
    let other = f.root.join("project");
    if std::fs::create_dir(&other).is_err() {
        // A case-insensitive volume: the lowercase name already is the project directory.
        return;
    }
    assert!(is_external_path(
        &text(&other.join("missing/file.txt")),
        &f.cwd,
        Path::new("/home/user")
    ));
    assert!(is_external_path(
        &text(&f.cwd.join("missing/file.txt")),
        &other,
        Path::new("/home/user")
    ));
}

/// senpi#4351 HIGH: resolving S before `..` must expose the denied p/y route.
#[test]
fn a_physical_parent_in_a_link_target_is_denied() {
    for (absolute, denied) in [(true, "y"), (false, "y"), (true, "?"), (false, "?")] {
        let f = fixture();
        let root = &f.root;
        std::fs::create_dir(root.join("a")).expect("a");
        std::fs::create_dir(root.join("x")).expect("x");
        std::fs::create_dir_all(root.join("p/q")).expect("p/q");
        symlink(root.join("p/q"), root.join("x/S")).expect("S");
        symlink(&f.outside, root.join("p/y")).expect("y");
        let link = if absolute {
            format!("{}/x/S/../y", text(root))
        } else {
            "../x/S/../y".into()
        };
        symlink(link, root.join("a/L")).expect("L");
        let service = PermissionService::new(
            vec![
                rule("*", Action::Allow),
                rule(&format!("{}/p/{denied}/*", text(root)), Action::Deny),
            ],
            vec![],
            PermissionEventEmitter::default(),
        );
        for input in [root.join("p/y/f.txt"), root.join("a/L/f.txt")] {
            let request = external("read", json!({ "path": text(&input) }), &f.cwd);
            assert_eq!(
                decide(&service, &request),
                Action::Deny,
                "{absolute} {denied} {}",
                input.display()
            );
        }
    }
}

/// senpi#4351 MEDIUM: L2 is relative to physical b/c, never lexical a/L1.
#[test]
fn a_relative_link_does_not_invent_an_unresolved_parent_spelling() {
    let f = fixture();
    let root = &f.root;
    std::fs::create_dir_all(root.join("a/d")).expect("a/d");
    std::fs::create_dir_all(root.join("b/c")).expect("b/c");
    std::fs::create_dir(root.join("b/d")).expect("b/d");
    std::fs::write(root.join("b/d/f.txt"), "fixture").expect("f");
    symlink(root.join("b/c"), root.join("a/L1")).expect("L1");
    symlink("../d", root.join("b/c/L2")).expect("L2");
    let request = external(
        "read",
        json!({ "path": text(&root.join("a/L1/L2/f.txt")) }),
        &f.cwd,
    );
    let service = PermissionService::new(
        vec![
            rule("*", Action::Allow),
            rule(&format!("{}/a/d/*", text(root)), Action::Deny),
        ],
        vec![],
        PermissionEventEmitter::default(),
    );
    assert_eq!(decide(&service, &request), Action::Allow);
}
