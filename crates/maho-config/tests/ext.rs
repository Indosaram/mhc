mod common;

use common::Tree;
use maho_config::ext::hot_reload::touches_protected;
use maho_config::ext::notices::{ConfigLoader, MigrationRunner, startup_notices};
use maho_config::ext::*;
use maho_config::migrate::MigrationEnv;
use maho_config::migrate::plans::run_startup_migration_with;
use maho_config::*;
use maho_ext_api::{
    ConfigWatchTargetKind, ConfigWatchTargetSpec, ConfigWatchValidation, NotificationType,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn runner() -> MigrationRunner {
    Arc::new(|cwd, env, home| {
        let process = MigrationEnv {
            now_ms: Arc::new(|| 1_800_000_000_000),
            is_alive: Arc::new(|_| false),
            ..MigrationEnv::default()
        };
        run_startup_migration_with(cwd, env, home, process)
    })
}

fn loader(t: &Tree) -> ConfigLoader {
    let _ = t;
    Arc::new(|cwd, env| {
        load(
            &LoadOptions::new(cwd)
                .with_env(env.clone())
                .with_account_home(None)
                .with_catalog(true),
        )
    })
}

type Seen = Arc<Mutex<Vec<(String, Option<NotificationType>)>>>;

fn deliver_to(queue: &NoticeQueue, has_ui: bool, seen: &Seen) {
    let notices = queue.take();
    let ui = Arc::clone(seen);
    let log = Arc::clone(seen);
    deliver(
        &notices,
        has_ui,
        &move |text, kind| ui.lock().expect("lock").push((text.to_owned(), Some(kind))),
        &move |text| log.lock().expect("lock").push((text.to_owned(), None)),
    );
}

#[test]
fn startup_notices_are_delivered_once() {
    let t = Tree::new();
    let source = t.write(
        "home/.omo/config.jsonc",
        r#"{"[senpi]": {"model_profile": "geeky-heavy"}}"#,
    );
    let mut env = t.env();
    env.insert("OMO_PROFILE".into(), "missing".into());
    let notices = startup_notices(&t.home, env, &runner(), &loader(&t));
    let queue = NoticeQueue::new(notices);
    let seen: Seen = Arc::default();
    deliver_to(&queue, true, &seen);
    deliver_to(&queue, true, &seen);
    let seen = seen.lock().expect("lock").clone();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0].1, Some(NotificationType::Info));
    assert!(seen[0].0.contains(&source.display().to_string()));
    assert!(seen[0].0.starts_with("mhc"));
    assert_eq!(seen[1].1, Some(NotificationType::Warning));
    assert!(seen[1].0.contains("profiles.missing"));
}

#[test]
fn migration_error_and_load_problems_are_both_reported() {
    let t = Tree::new();
    let user = t.write(
        "home/.omo/omo.jsonc",
        r#"{"categories": {"quick": {"model": "a/b"}}}"#,
    );
    t.write("home/p/.omo/omo.jsonc", r#"{"typo_key": 1}"#);
    let cwd = t.mkdir("home/p");
    let notices = startup_notices(&cwd, t.env(), &runner(), &loader(&t));
    let warnings: Vec<&StartupNotice> = notices
        .iter()
        .filter(|n| n.level == NoticeLevel::Warning)
        .collect();
    assert_eq!(warnings.len(), 2, "{notices:?}");
    assert!(warnings[0].text.contains("migration") && warnings[0].text.contains("typo_key"));
    assert!(warnings[1].text.contains("typo_key"));
    let text = std::fs::read_to_string(user).expect("read");
    assert!(text.contains("2026-08-reasoning-unification"));
}

#[test]
fn without_ui_notices_go_to_the_log() {
    let t = Tree::new();
    let mut env = t.env();
    env.insert("OMO_PROFILE".into(), "missing".into());
    let queue = NoticeQueue::new(startup_notices(&t.home, env, &runner(), &loader(&t)));
    let seen: Seen = Arc::default();
    deliver_to(&queue, false, &seen);
    let seen = seen.lock().expect("lock").clone();
    assert!(!seen.is_empty());
    assert!(seen.iter().all(|(_, kind)| kind.is_none()));
}

fn validator(t: &Tree) -> ReloadValidator {
    let cwd = t.mkdir("home/p");
    let env = t.env();
    ReloadValidator::new(Arc::new(move || {
        load(
            &LoadOptions::new(&cwd)
                .with_env(env.clone())
                .with_account_home(None)
                .with_catalog(true),
        )
    }))
}

#[test]
fn unknown_key_in_a_changed_file_is_rejected_until_fixed() {
    let t = Tree::new();
    let user = t.write("home/.omo/omo.jsonc", r#"{"model_profile": "x"}"#);
    let mut v = validator(&t);
    std::fs::write(&user, r#"{"model_profile": "x", "oops": 1}"#).expect("write");
    match v.validate(std::slice::from_ref(&user)) {
        ConfigWatchValidation::Rejected { errors } => {
            assert!(errors.iter().any(|e| e.contains("oops")), "{errors:?}")
        }
        other => panic!("expected rejection, got {other:?}"),
    }
    // Sticky: an unrelated change does not clear it while the problem remains.
    let other = t.root.join("home/p/.omo/omo.jsonc");
    assert!(matches!(
        v.validate(std::slice::from_ref(&other)),
        ConfigWatchValidation::Rejected { .. }
    ));
    std::fs::write(&user, r#"{"model_profile": "x"}"#).expect("write");
    assert_eq!(
        v.validate(std::slice::from_ref(&user)),
        ConfigWatchValidation::Ok
    );
}

#[test]
fn pre_existing_problems_elsewhere_do_not_block() {
    let t = Tree::new();
    let user = t.write("home/.omo/omo.jsonc", r#"{"model_profile": "x"}"#);
    t.write(
        "home/p/.omo/omo.jsonc",
        r#"{"telemetry": {"enabled": "yes"}, "model_profile": 3}"#,
    );
    let mut v = validator(&t);
    std::fs::write(&user, r#"{"model_profile": "y"}"#).expect("write");
    assert_eq!(
        v.validate(std::slice::from_ref(&user)),
        ConfigWatchValidation::Ok
    );
}

#[test]
fn deprecations_never_reject() {
    let t = Tree::new();
    let user = t.write(
        "home/.omo/omo.jsonc",
        r#"{"categories": {"deep-low": {"model": "a/b"}}}"#,
    );
    let mut v = validator(&t);
    std::fs::write(&user, r#"{"categories": {"deep": {"model": "a/b"}}}"#).expect("write");
    assert_eq!(
        v.validate(std::slice::from_ref(&user)),
        ConfigWatchValidation::Ok
    );
}

fn targets(t: &Tree, cwd: &str, home: PathBuf, agent: PathBuf) -> WatchPlan {
    let cwd = t.mkdir(cwd);
    watch_targets(&WatchInputs {
        cwd: &cwd,
        home: &home,
        agent_dir: &agent,
        account_home: None,
        fs: &RealFs,
        skip_mount: &|_| false,
    })
}

#[test]
fn home_without_omo_is_watched_for_creation() {
    let t = Tree::new();
    // The agent dir lives outside home here: a `/.omo` filter on home would otherwise reach the
    // protected agent paths under `~/.omo/agent` and the host would refuse the registration.
    let plan = targets(&t, "home/p", t.home.clone(), t.root.join("agent"));
    assert!(plan.user_watched);
    let home_target = plan
        .targets
        .iter()
        .find(|target| target.path == t.home)
        .expect("home target");
    assert_eq!(home_target.kind, ConfigWatchTargetKind::Dir);
    assert!(home_target.filter_globs.contains(&"/.omo".to_string()));
    assert!(
        home_target
            .filter_globs
            .contains(&"/.omo/omo.jsonc".to_string())
    );
    assert!(
        plan.targets
            .iter()
            .any(|target| target.path == t.root.join("home/p"))
    );

    let missing = t.root.join("nohome");
    let plan = targets(&t, "elsewhere", missing.clone(), missing.join(".omo/agent"));
    assert!(!plan.user_watched);
}

#[test]
fn existing_omo_dirs_are_watched_for_their_files() {
    let t = Tree::new();
    t.mkdir("home/.omo");
    t.write("home/p/.omo/omo.jsonc", "{}");
    let plan = targets(&t, "home/p/q", t.home.clone(), t.home.join(".omo/agent"));
    let user = plan
        .targets
        .iter()
        .find(|target| target.path == t.home.join(".omo"))
        .expect("user");
    assert_eq!(
        user.filter_globs,
        vec!["/omo.jsonc".to_string(), "/omo.json".to_string()]
    );
    assert!(
        plan.targets
            .iter()
            .any(|target| target.path == t.root.join("home/p/.omo"))
    );
}

#[test]
fn protected_agent_paths_are_never_watched() {
    let t = Tree::new();
    t.mkdir("home/.omo/agent/sessions");
    let agent = t.home.join(".omo/agent");
    let plan = targets(
        &t,
        "home/.omo/agent/sessions",
        t.home.clone(),
        agent.clone(),
    );
    for target in &plan.targets {
        assert!(!touches_protected(target, &agent), "{target:?}");
        assert!(!target.path.starts_with(agent.join("sessions")));
    }
    let bad = ConfigWatchTargetSpec {
        path: agent.clone(),
        kind: ConfigWatchTargetKind::Dir,
        filter_globs: vec!["/auth.json".into()],
    };
    assert!(touches_protected(&bad, &agent));
}

#[test]
fn republish_is_capped_per_target_set() {
    let mut budget = RepublishBudget::default();
    let set = vec![ConfigWatchTargetSpec {
        path: PathBuf::from("/x"),
        kind: ConfigWatchTargetKind::Dir,
        filter_globs: vec![],
    }];
    let allowed: Vec<bool> = (0..4).map(|_| budget.allow(&set)).collect();
    assert_eq!(allowed, vec![true, true, true, false]);
    let other = vec![ConfigWatchTargetSpec {
        path: PathBuf::from("/y"),
        kind: ConfigWatchTargetKind::Dir,
        filter_globs: vec![],
    }];
    assert!(budget.allow(&other));
}
