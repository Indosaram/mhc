//! read-permission.ts: a file inside a bundled resource root is auto-approved when the rules only
//! ask, a deny still decides it, and other files keep the read/external_directory requests.
use maho_ext_permission_system::{
    events::PermissionEventEmitter,
    read_permission::{bundled_resource_roots, parse_read_permission},
    service::{AskOptions, PermissionService},
    types::{Action, PermissionError, Request, Rule},
};
use std::{
    future::Future,
    path::Path,
    task::{Context, Poll, Waker},
};

fn rule(permission: &str, pattern: &str, action: Action) -> Rule {
    Rule {
        permission: permission.into(),
        pattern: pattern.into(),
        action,
    }
}
fn settled<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("read permission is still pending"),
    }
}

#[test]
fn bundled_resource_reads_are_auto_approved_but_still_deniable() {
    let root = tempfile::tempdir().expect("root");
    let base = root.path().canonicalize().expect("real root");
    let (cwd, bundled) = (base.join("project"), base.join("bundle").join("docs"));
    std::fs::create_dir_all(&cwd).expect("cwd");
    std::fs::create_dir_all(&bundled).expect("bundle");
    let doc = bundled.join("guide.md");
    std::fs::write(&doc, "doc").expect("doc");
    let doc_text = doc.to_string_lossy().into_owned();
    let home = base.join("home");

    let requests = parse_read_permission(&doc_text, (&cwd, &home), &[bundled.clone()]);
    assert_eq!(
        requests.len(),
        1,
        "no external_directory for a bundled file"
    );
    let read = &requests[0];
    assert_eq!(
        (read.permission.as_str(), read.auto_approve_ask),
        ("read", true)
    );
    assert_eq!(read.patterns, [doc_text.clone()]);
    assert_eq!(read.rule_aliases.as_deref(), Some(&[doc_text.clone()][..]));

    // An ask-only ruleset approves the bundled read without prompting.
    let ask = |rules: Vec<Rule>| {
        let mut service = PermissionService::new(rules, vec![], PermissionEventEmitter::default());
        let request = Request::new(
            "read-1".into(),
            "session".into(),
            read.permission.clone(),
            read.patterns.clone(),
            read.always.clone(),
            Default::default(),
        );
        let result = settled(service.ask_with(
            request,
            AskOptions {
                auto_approve_ask: read.auto_approve_ask,
                rule_aliases: read.rule_aliases.clone(),
            },
        ));
        (result, service.list().len())
    };
    let (allowed, pending) = ask(vec![rule("*", "*", Action::Ask)]);
    assert!(allowed.is_ok() && pending == 0);
    // A deny on the file still refuses it.
    let (denied, _) = ask(vec![
        rule("*", "*", Action::Ask),
        rule("read", &format!("{}/*", bundled.display()), Action::Deny),
    ]);
    assert!(matches!(denied, Err(PermissionError::Denied(_))));
}

#[test]
fn files_outside_bundled_roots_keep_read_and_external_directory_requests() {
    let root = tempfile::tempdir().expect("root");
    let base = root.path().canonicalize().expect("real root");
    let cwd = base.join("project");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let home = base.join("home");
    let unrelated = [base.join("bundle")];

    let inside = parse_read_permission("src/main.rs", (&cwd, &home), &unrelated);
    assert_eq!(inside.len(), 1);
    assert_eq!(
        (inside[0].auto_approve_ask, inside[0].patterns.clone()),
        (false, vec!["src/main.rs".to_owned()])
    );

    let outside = parse_read_permission("../notes.txt", (&cwd, &home), &unrelated);
    assert_eq!(
        outside
            .iter()
            .map(|request| request.permission.as_str())
            .collect::<Vec<_>>(),
        ["read", "external_directory"]
    );
    let external = &outside[1];
    assert_eq!(
        external.patterns,
        [base.join("notes.txt").to_string_lossy()]
    );
    assert_eq!(
        external.original_paths.as_deref(),
        Some(&["../notes.txt".to_owned()][..])
    );
    assert!(!external.auto_approve_ask);
}

#[test]
fn bundled_roots_name_shipped_payloads_but_not_the_binary_or_package_directory() {
    let agent_dir = tempfile::tempdir().expect("agent dir");
    let roots = bundled_resource_roots(agent_dir.path());
    let package = Path::new(&maho_core::config::get_package_dir()).to_path_buf();
    for shipped in ["theme", "export-html", "assets", "docs", "examples"] {
        assert!(
            roots.contains(&package.join(shipped)),
            "{shipped} missing from {roots:?}"
        );
    }
    // The imagegen skill root is the per-user folder imagegen materializes, not a shared temp dir.
    let skill = maho_ext_imagegen::embedded_skill_dir(agent_dir.path());
    assert!(roots.contains(&skill), "{skill:?} missing from {roots:?}");
    assert!(skill.starts_with(agent_dir.path()));
    assert!(
        !roots
            .iter()
            .any(|root| root.starts_with(std::env::temp_dir())
                && !root.starts_with(agent_dir.path())),
        "a root sits in the shared temp dir: {roots:?}"
    );
    let binary_dir = std::env::current_exe()
        .expect("exe")
        .parent()
        .expect("binary dir")
        .to_path_buf();
    assert!(
        !roots.contains(&binary_dir),
        "binary dir is a root: {roots:?}"
    );
    assert!(
        !roots.contains(&package),
        "package dir is a root: {roots:?}"
    );

    // A file directly next to the binary is a plain read plus external_directory when the
    // binary directory lies outside cwd.
    let root = tempfile::tempdir().expect("root");
    let base = root.path().canonicalize().expect("real root");
    let cwd = base.join("project");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let home = base.join("home");
    let neighbor = binary_dir.join("neighbor-of-the-binary.txt");
    let neighbor_text = neighbor.to_string_lossy().into_owned();
    let requests = parse_read_permission(&neighbor_text, (&cwd, &home), &roots);
    assert_eq!(
        requests
            .iter()
            .map(|request| request.permission.as_str())
            .collect::<Vec<_>>(),
        ["read", "external_directory"]
    );
    assert!(requests.iter().all(|request| !request.auto_approve_ask));
}

#[cfg(unix)]
#[test]
fn a_symlinked_root_does_not_auto_approve_reads_through_it() {
    // Another user pre-creates the configured root as a link to the victim's files: a read through
    // it must keep the read prompt and the external_directory request.
    let root = tempfile::tempdir().expect("root");
    let base = root.path().canonicalize().expect("real root");
    let (cwd, outside, home) = (base.join("project"), base.join("victim"), base.join("home"));
    std::fs::create_dir_all(&cwd).expect("cwd");
    std::fs::create_dir_all(&outside).expect("outside");
    std::fs::write(outside.join("secret.txt"), "secret").expect("secret");
    let link = base.join("bundle-skill");
    std::os::unix::fs::symlink(&outside, &link).expect("symlink");
    let through = link.join("secret.txt").to_string_lossy().into_owned();

    let requests = parse_read_permission(&through, (&cwd, &home), &[link.clone()]);
    assert_eq!(
        requests
            .iter()
            .map(|request| request.permission.as_str())
            .collect::<Vec<_>>(),
        ["read", "external_directory"]
    );
    assert!(requests.iter().all(|request| !request.auto_approve_ask));
    assert_eq!(
        requests[1].patterns,
        [outside.join("secret.txt").to_string_lossy()]
    );
}
