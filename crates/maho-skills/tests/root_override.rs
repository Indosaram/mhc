//! Tests written from the v5.1.52 delta of docs/cleanroom/specs/skills.md (D3 cases 1-5).

use maho_skills::{
    LEGACY_SKILLS_ROOT_ENV, NoticeContext, NoticeLedger, RootResolver, SKILLS_ROOT_ENV,
    SkillsExtension,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

struct Setup {
    _dir: tempfile::TempDir,
    root: PathBuf,
    exe: PathBuf,
    sibling: PathBuf,
}

fn setup() -> Setup {
    let dir = tempfile::tempdir().expect("dir");
    let root = dir.path().join("override");
    std::fs::create_dir_all(&root).expect("mkdir");
    let bin = dir.path().join("bin");
    let sibling = bin.join("skills");
    std::fs::create_dir_all(&sibling).expect("mkdir");
    Setup {
        root,
        exe: bin.join("mhc"),
        sibling,
        _dir: dir,
    }
}

fn resolver(exe: &Path, env: &[(&str, &str)]) -> RootResolver {
    let env: BTreeMap<String, String> = env
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    RootResolver {
        env: Arc::new(move |key: &str| env.get(key).cloned()),
        exe: Some(exe.to_path_buf()),
        ledger: Arc::new(NoticeLedger::default()),
    }
}

/// Records log lines and UI notices.
#[derive(Default)]
struct Sink {
    logs: Mutex<Vec<String>>,
    shown: Mutex<Vec<String>>,
}

impl Sink {
    fn resolve(&self, resolver: &RootResolver, option: Option<&Path>) -> Option<PathBuf> {
        let log = |message: &str| self.logs.lock().expect("lock").push(message.into());
        let ui = |message: &str| self.shown.lock().expect("lock").push(message.into());
        resolver.resolve(
            option,
            &NoticeContext {
                log: &log,
                ui: Some(&ui),
            },
        )
    }

    fn logs(&self) -> Vec<String> {
        self.logs.lock().expect("lock").clone()
    }

    fn shown(&self) -> Vec<String> {
        self.shown.lock().expect("lock").clone()
    }
}

#[test]
fn d3_1_relative_value_is_ignored_and_reported_once() {
    let s = setup();
    let resolver = resolver(&s.exe, &[(SKILLS_ROOT_ENV, "relative/skills")]);
    let sink = Sink::default();
    assert_eq!(sink.resolve(&resolver, None), Some(s.sibling.clone()));
    assert_eq!(sink.logs().len(), 1);
    assert_eq!(sink.shown().len(), 1);
    let message = &sink.logs()[0];
    assert!(message.contains(SKILLS_ROOT_ENV));
    assert!(message.contains("relative/skills"));
    assert!(message.contains("relative"));
    assert!(message.contains("absolute"));
    assert_eq!(sink.resolve(&resolver, None), Some(s.sibling.clone()));
    assert_eq!(sink.logs().len(), 1);
    assert_eq!(sink.shown().len(), 1);
}

#[test]
fn d3_1_consumer_without_ui_only_logs() {
    let s = setup();
    let resolver = resolver(&s.exe, &[(SKILLS_ROOT_ENV, "rel")]);
    let logs = Mutex::new(Vec::<String>::new());
    let log = |message: &str| logs.lock().expect("lock").push(message.into());
    let root = resolver.resolve(
        None,
        &NoticeContext {
            log: &log,
            ui: None,
        },
    );
    assert_eq!(root, Some(s.sibling.clone()));
    assert_eq!(logs.lock().expect("lock").len(), 1);
}

#[test]
fn d3_2_file_missing_and_unreadable_values_name_the_reason() {
    let s = setup();
    let file = s.root.join("a-file");
    std::fs::write(&file, "x").expect("write");
    let missing = s.root.join("missing");
    for (value, reason) in [(file, "ENOTDIR"), (missing, "ENOENT")] {
        let value = value.to_string_lossy().into_owned();
        let resolver = resolver(&s.exe, &[(SKILLS_ROOT_ENV, value.as_str())]);
        let sink = Sink::default();
        assert_eq!(sink.resolve(&resolver, None), Some(s.sibling.clone()));
        let logs = sink.logs();
        assert_eq!(logs.len(), 1, "{value}");
        assert!(logs[0].contains(reason), "{}", logs[0]);
        assert!(logs[0].contains(&value));
    }

    let valid = s.root.to_string_lossy().into_owned();
    let resolver = resolver(&s.exe, &[(SKILLS_ROOT_ENV, valid.as_str())]);
    let sink = Sink::default();
    assert_eq!(sink.resolve(&resolver, None), Some(s.root.clone()));
    assert!(sink.logs().is_empty() && sink.shown().is_empty());
}

#[cfg(unix)]
#[test]
fn d3_2_unreadable_directory_is_eacces() {
    use std::os::unix::fs::PermissionsExt;
    let s = setup();
    let locked = s.root.join("locked");
    std::fs::create_dir_all(&locked).expect("mkdir");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    // A privileged user can read any directory; the case only exists for an unprivileged one.
    let readable = std::fs::read_dir(&locked).is_ok();
    let value = locked.to_string_lossy().into_owned();
    let resolver = resolver(&s.exe, &[(SKILLS_ROOT_ENV, value.as_str())]);
    let sink = Sink::default();
    let root = sink.resolve(&resolver, None);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    if readable {
        assert_eq!(root, Some(locked));
        return;
    }
    assert_eq!(root, Some(s.sibling.clone()));
    assert!(sink.logs()[0].contains("EACCES"), "{}", sink.logs()[0]);
}

#[test]
fn d3_3_option_wins_over_invalid_env_without_notice() {
    let s = setup();
    let resolver = resolver(&s.exe, &[(SKILLS_ROOT_ENV, "relative")]);
    let sink = Sink::default();
    let option = s.root.join("from-code");
    assert_eq!(sink.resolve(&resolver, Some(&option)), Some(option.clone()));
    assert!(sink.logs().is_empty() && sink.shown().is_empty());
}

#[test]
fn d3_4_all_consumers_share_one_root_and_one_notice() {
    let s = setup();
    let extension = SkillsExtension {
        bundled_root: None,
        resolver: resolver(&s.exe, &[(SKILLS_ROOT_ENV, "relative")]),
    };
    // `/skill` and the `skill` tool both resolve through `SkillsExtension::root`.
    let command = Sink::default();
    let tool = Sink::default();
    let log_c = |m: &str| command.logs.lock().expect("lock").push(m.into());
    let ui_c = |m: &str| command.shown.lock().expect("lock").push(m.into());
    let log_t = |m: &str| tool.logs.lock().expect("lock").push(m.into());
    let ui_t = |m: &str| tool.shown.lock().expect("lock").push(m.into());
    let from_command = extension.root(&NoticeContext {
        log: &log_c,
        ui: Some(&ui_c),
    });
    let from_tool = extension.root(&NoticeContext {
        log: &log_t,
        ui: Some(&ui_t),
    });
    assert_eq!(from_command, Some(s.sibling.clone()));
    assert_eq!(from_command, from_tool);
    assert_eq!(command.logs().len() + tool.logs().len(), 1);
    assert_eq!(command.shown().len() + tool.shown().len(), 1);
}

#[test]
fn d3_5_legacy_variable_is_read_only_when_new_one_is_unset_or_empty() {
    let s = setup();
    let valid = s.root.to_string_lossy().into_owned();

    let resolver_a = resolver(
        &s.exe,
        &[
            (SKILLS_ROOT_ENV, ""),
            (LEGACY_SKILLS_ROOT_ENV, valid.as_str()),
        ],
    );
    let sink = Sink::default();
    assert_eq!(sink.resolve(&resolver_a, None), Some(s.root.clone()));
    assert!(sink.logs().is_empty());

    let resolver_b = resolver(
        &s.exe,
        &[
            (SKILLS_ROOT_ENV, "relative"),
            (LEGACY_SKILLS_ROOT_ENV, valid.as_str()),
        ],
    );
    let sink = Sink::default();
    assert_eq!(sink.resolve(&resolver_b, None), Some(s.sibling.clone()));
    let logs = sink.logs();
    assert_eq!(logs.len(), 1);
    assert!(logs[0].contains(SKILLS_ROOT_ENV));
    assert!(!logs[0].contains(LEGACY_SKILLS_ROOT_ENV));
}

#[test]
fn unset_or_empty_value_is_absent_without_notice() {
    let s = setup();
    for env in [vec![], vec![(SKILLS_ROOT_ENV, "")]] {
        let resolver = resolver(&s.exe, &env);
        let sink = Sink::default();
        assert_eq!(sink.resolve(&resolver, None), Some(s.sibling.clone()));
        assert!(sink.logs().is_empty() && sink.shown().is_empty());
    }
}
