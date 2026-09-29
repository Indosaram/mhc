use std::collections::BTreeMap;

use omo_config_core::issue::PathSegment;
use omo_config_core::writer::types::FsError;
use omo_config_core::{
    LoadOmoConfigOptions, OmoConfigEdit, OmoConfigEnv, OmoConfigWriteFileSystem,
    StdWriteFileSystem, UpdateOmoConfigOptions, load_omo_config, update_omo_config,
};
use serde_json::json;

fn env_of(pairs: &[(&str, &str)]) -> OmoConfigEnv {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<BTreeMap<_, _>>()
}

fn write_file(path: &str, content: &str) {
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent).expect("parent directory");
    }
    std::fs::write(path, content).expect("write fixture");
}

fn edit(path: &[&str], value: serde_json::Value) -> OmoConfigEdit {
    OmoConfigEdit::set(
        path.iter()
            .map(|key| PathSegment::Key((*key).to_string()))
            .collect(),
        value,
    )
}

fn project_options<'a>(project_dir: &'a str, env: &OmoConfigEnv) -> UpdateOmoConfigOptions<'a> {
    UpdateOmoConfigOptions {
        scope: "project",
        project_dir: Some(project_dir.to_string()),
        env: Some(env.clone()),
        ..UpdateOmoConfigOptions::default()
    }
}

fn user_options<'a>(env: &OmoConfigEnv) -> UpdateOmoConfigOptions<'a> {
    UpdateOmoConfigOptions {
        scope: "user",
        env: Some(env.clone()),
        ..UpdateOmoConfigOptions::default()
    }
}

fn fixture() -> (tempfile::TempDir, String, String) {
    let root = tempfile::tempdir().expect("tempdir");
    let root_path = root.path().to_string_lossy().to_string();
    let home = format!("{root_path}/home");
    let project = format!("{home}/project");
    std::fs::create_dir_all(&project).expect("project");
    (root, home, project)
}

fn backup_files(directory: &str) -> Vec<String> {
    std::fs::read_dir(directory)
        .expect("read_dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.contains(".bak."))
        .collect()
}

#[test]
fn editing_a_commented_project_config_twice_keeps_comments_and_distinct_backups() {
    let (_root, home, project) = fixture();
    let config_path = format!("{project}/.omo/omo.jsonc");
    write_file(
        &config_path,
        "{\n  // task settings stay documented\n  \"task\": {\n    \"default_concurrency\": 5\n  }\n}\n",
    );
    let env = env_of(&[("HOME", &home)]);

    let mut first = project_options(&project, &env);
    first.edits = vec![edit(&["task", "default_concurrency"], json!(3))];
    let first = update_omo_config(&first).expect("first write");

    let mut second = project_options(&project, &env);
    second.edits = vec![edit(&["task", "wait", "default_ms"], json!(12000))];
    let second = update_omo_config(&second).expect("second write");

    let content = std::fs::read_to_string(&config_path).expect("content");
    assert_eq!(first.path, config_path);
    assert_eq!(second.path, config_path);
    assert!(content.contains("// task settings stay documented"));
    assert!(content.contains("\"default_concurrency\": 3"));
    assert!(content.contains("\"default_ms\": 12000"));
    let backups = backup_files(&format!("{project}/.omo"));
    assert_eq!(backups.len(), 2);
    assert!(backups[0].contains(".bak."));
}

#[test]
fn a_pinned_timestamp_still_yields_distinct_backup_paths() {
    let (_root, home, project) = fixture();
    let config_path = format!("{project}/.omo/omo.jsonc");
    write_file(
        &config_path,
        "{\n  // task settings stay documented\n  \"task\": {\n    \"default_concurrency\": 5\n  }\n}\n",
    );
    let env = env_of(&[("HOME", &home)]);
    let pinned = "2026-07-06T00:00:00.000Z".to_string();

    let mut first = project_options(&project, &env);
    first.edits = vec![edit(&["task", "default_concurrency"], json!(3))];
    first.timestamp = Some(pinned.clone());
    let first = update_omo_config(&first).expect("first write");

    let mut second = project_options(&project, &env);
    second.edits = vec![edit(&["task", "wait", "default_ms"], json!(12000))];
    second.timestamp = Some(pinned);
    let second = update_omo_config(&second).expect("second write");

    assert_eq!(
        first.backup_path,
        Some(format!("{config_path}.bak.2026-07-06T00-00-00-000Z"))
    );
    let second_backup = second.backup_path.clone().expect("second backup");
    assert!(second_backup.starts_with(&format!("{config_path}.bak.2026-07-06T00-00-00-000Z")));
    assert_ne!(second.backup_path, first.backup_path);
    assert!(std::path::Path::new(first.backup_path.as_deref().expect("path")).exists());
    assert!(std::path::Path::new(&second_backup).exists());
    assert_eq!(backup_files(&format!("{project}/.omo")).len(), 2);
    let content = std::fs::read_to_string(&config_path).expect("content");
    assert!(content.contains("// task settings stay documented"));
    assert!(content.contains("\"default_concurrency\": 3"));
    assert!(content.contains("\"default_ms\": 12000"));
}

#[test]
fn editing_a_missing_user_config_creates_it_with_the_header() {
    let (_root, home, _project) = fixture();
    let config_path = format!("{home}/.maho/omo.jsonc");
    let env = env_of(&[("HOME", &home)]);

    let mut options = user_options(&env);
    options.edits = vec![edit(&["task", "default_concurrency"], json!(6))];
    let result = update_omo_config(&options).expect("write");

    let content = std::fs::read_to_string(&config_path).expect("content");
    assert_eq!(result.path, config_path);
    assert!(content.contains("// OMO configuration"));
    assert!(content.contains("\"default_concurrency\": 6"));
}

#[test]
fn editing_an_existing_project_json_preserves_the_path_and_the_settings() {
    let (_root, home, project) = fixture();
    let config_path = format!("{project}/.omo/omo.json");
    let shadow_path = format!("{project}/.omo/omo.jsonc");
    write_file(
        &config_path,
        "{\"task\":{\"default_concurrency\":9,\"wait\":{\"max_ms\":70000}}}\n",
    );
    let env = env_of(&[("HOME", &home)]);

    let mut options = project_options(&project, &env);
    options.edits = vec![edit(&["task", "wait", "default_ms"], json!(12000))];
    let result = update_omo_config(&options).expect("write");

    let loaded = load_omo_config(&LoadOmoConfigOptions {
        cwd: Some(project.clone()),
        env: Some(env.clone()),
        platform: Some("linux".to_string()),
        ..LoadOmoConfigOptions::default()
    });

    assert_eq!(result.path, config_path);
    assert!(!std::path::Path::new(&shadow_path).exists());
    assert_eq!(loaded.config["task"]["default_concurrency"], json!(9));
    assert_eq!(loaded.config["task"]["wait"]["default_ms"], json!(12000));
    assert_eq!(loaded.config["task"]["wait"]["max_ms"], json!(70000));
}

#[test]
fn editing_an_existing_user_json_preserves_the_path_and_the_settings() {
    let (_root, home, project) = fixture();
    let config_path = format!("{home}/.maho/omo.json");
    let shadow_path = format!("{home}/.maho/omo.jsonc");
    write_file(
        &config_path,
        "{\"task\":{\"default_concurrency\":9,\"wait\":{\"max_ms\":70000}}}\n",
    );
    let env = env_of(&[("HOME", &home)]);

    let mut options = user_options(&env);
    options.edits = vec![edit(&["task", "wait", "default_ms"], json!(12000))];
    let result = update_omo_config(&options).expect("write");

    let loaded = load_omo_config(&LoadOmoConfigOptions {
        cwd: Some(project),
        env: Some(env),
        platform: Some("linux".to_string()),
        ..LoadOmoConfigOptions::default()
    });

    assert_eq!(result.path, config_path);
    assert!(!std::path::Path::new(&shadow_path).exists());
    assert_eq!(loaded.config["task"]["default_concurrency"], json!(9));
    assert_eq!(loaded.config["task"]["wait"]["default_ms"], json!(12000));
    assert_eq!(loaded.config["task"]["wait"]["max_ms"], json!(70000));
}

struct FailingExclusiveWriteFileSystem {
    inner: StdWriteFileSystem,
}

impl OmoConfigWriteFileSystem for FailingExclusiveWriteFileSystem {
    fn copy(&self, source: &str, destination: &str) -> Result<(), FsError> {
        self.inner.copy(source, destination)
    }

    fn exists(&self, path: &str) -> bool {
        self.inner.exists(path)
    }

    fn is_symbolic_link(&self, path: &str) -> Result<bool, FsError> {
        self.inner.is_symbolic_link(path)
    }

    fn mkdirs(&self, path: &str) -> Result<(), FsError> {
        self.inner.mkdirs(path)
    }

    fn read(&self, path: &str) -> Result<String, FsError> {
        self.inner.read(path)
    }

    fn list_dir(&self, path: &str) -> Result<Vec<String>, FsError> {
        self.inner.list_dir(path)
    }

    fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        self.inner.rename(from, to)
    }

    fn unlink(&self, path: &str) -> Result<(), FsError> {
        self.inner.unlink(path)
    }

    fn write_exclusive(&self, _path: &str, _content: &str) -> Result<(), FsError> {
        Err(FsError::other("EACCES synthetic"))
    }

    fn write(&self, path: &str, content: &str) -> Result<(), FsError> {
        self.inner.write(path, content)
    }
}

#[test]
fn a_failing_temp_write_surfaces_a_typed_error_and_leaves_no_partial() {
    let (_root, home, project) = fixture();
    let config_path = format!("{project}/.omo/omo.jsonc");
    write_file(&config_path, "{\"task\":{\"default_concurrency\":5}}\n");
    let env = env_of(&[("HOME", &home)]);
    let file_system = FailingExclusiveWriteFileSystem {
        inner: StdWriteFileSystem,
    };

    let mut options = project_options(&project, &env);
    options.edits = vec![edit(&["task", "default_concurrency"], json!(4))];
    options.file_system = Some(&file_system);
    let error = update_omo_config(&options).expect_err("expected a typed error");

    assert_eq!(error.operation, "backup");
    assert!(!std::path::Path::new(&format!("{config_path}.tmp")).exists());
    assert!(
        std::fs::read_to_string(&config_path)
            .expect("content")
            .contains("\"default_concurrency\":5")
    );
}

#[test]
fn a_malformed_existing_config_surfaces_a_typed_error_and_keeps_the_original_bytes() {
    let (_root, home, project) = fixture();
    let config_path = format!("{project}/.omo/omo.jsonc");
    let original = "{\"task\":";
    write_file(&config_path, original);
    let env = env_of(&[("HOME", &home)]);

    let mut options = project_options(&project, &env);
    options.edits = vec![edit(&["task", "default_concurrency"], json!(4))];
    let error = update_omo_config(&options).expect_err("expected a typed error");

    assert_eq!(error.operation, "parse");
    assert_eq!(
        std::fs::read_to_string(&config_path).expect("content"),
        original
    );
    assert!(!std::path::Path::new(&format!("{config_path}.tmp")).exists());
}

#[test]
fn the_user_write_target_never_leaves_the_dot_omo_directory() {
    let (_root, home, _project) = fixture();
    let legacy_dir = format!("{home}/.config/omo");
    write_file(
        &format!("{legacy_dir}/omo.jsonc"),
        "{\"task\":{\"default_concurrency\":99}}\n",
    );
    let env = env_of(&[
        ("HOME", &home),
        ("XDG_CONFIG_HOME", &format!("{home}/.config")),
        ("APPDATA", &format!("{home}/AppData/Roaming")),
    ]);

    let mut options = user_options(&env);
    options.edits = vec![edit(&["task", "default_concurrency"], json!(3))];
    let created = update_omo_config(&options).expect("write");

    assert_eq!(created.path, format!("{home}/.maho/omo.jsonc"));
    assert!(
        std::fs::read_to_string(format!("{legacy_dir}/omo.jsonc"))
            .expect("legacy content")
            .contains("\"default_concurrency\":99")
    );

    let (_second_root, second_home, _second_project) = fixture();
    let json_path = format!("{second_home}/.maho/omo.json");
    write_file(&json_path, "{\"task\":{\"default_concurrency\":9}}\n");
    let second_env = env_of(&[
        ("HOME", &second_home),
        ("XDG_CONFIG_HOME", &format!("{second_home}/.config")),
        ("APPDATA", &format!("{second_home}/AppData/Roaming")),
    ]);
    let mut second_options = user_options(&second_env);
    second_options.edits = vec![edit(&["task", "default_concurrency"], json!(4))];
    let edited = update_omo_config(&second_options).expect("write");

    assert_eq!(edited.path, json_path);
    assert_eq!(
        std::path::Path::new(&edited.path)
            .parent()
            .map(|parent| parent.to_string_lossy().to_string()),
        Some(format!("{second_home}/.maho"))
    );
}

#[test]
fn a_preexisting_temp_symlink_is_never_followed() {
    let (_root, _home, project) = fixture();
    let config_path = format!("{project}/.omo/omo.jsonc");
    let victim_path = format!("{project}/victim.txt");
    write_file(&config_path, "{\"task\":{\"default_concurrency\":5}}\n");
    write_file(&victim_path, "victim-original");
    std::os::unix::fs::symlink(&victim_path, format!("{config_path}.tmp")).expect("symlink");
    let env = env_of(&[("HOME", "/nonexistent-home")]);

    let mut options = project_options(&project, &env);
    options.edits = vec![edit(&["task", "default_concurrency"], json!(4))];
    update_omo_config(&options).expect("write");

    assert_eq!(
        std::fs::read_to_string(&victim_path).expect("victim"),
        "victim-original"
    );
    assert!(
        std::fs::read_to_string(&config_path)
            .expect("content")
            .contains("\"default_concurrency\":4")
    );
    assert!(std::path::Path::new(&format!("{config_path}.tmp")).exists());
}

#[test]
fn a_symlinked_existing_config_is_rejected_without_writing_a_backup() {
    for extension in ["jsonc", "json"] {
        let (_root, _home, project) = fixture();
        let config_path = format!("{project}/.omo/omo.{extension}");
        let victim_path = format!("{project}/victim.json");
        write_file(&victim_path, "{\"task\":{\"default_concurrency\":8}}\n");
        std::fs::create_dir_all(format!("{project}/.omo")).expect("omo dir");
        std::os::unix::fs::symlink(&victim_path, &config_path).expect("symlink");
        let env = env_of(&[("HOME", "/nonexistent-home")]);

        let mut options = project_options(&project, &env);
        options.edits = vec![edit(&["task", "default_concurrency"], json!(4))];
        let error = update_omo_config(&options).expect_err("expected a typed error");

        assert_eq!(error.operation, "read", "{extension}");
        assert_eq!(
            std::fs::read_to_string(&victim_path).expect("victim"),
            "{\"task\":{\"default_concurrency\":8}}\n"
        );
        assert!(
            backup_files(&format!("{project}/.omo")).is_empty(),
            "{extension}"
        );
    }
}

#[test]
fn a_symlinked_project_omo_directory_is_rejected_without_touching_the_target() {
    let (_root, home, project) = fixture();
    let target_dir = format!("{home}/.omo");
    let target_path = format!("{target_dir}/omo.jsonc");
    let original = "{\"task\":{\"default_concurrency\":8}}\n";
    write_file(&target_path, original);
    std::os::unix::fs::symlink(&target_dir, format!("{project}/.omo")).expect("symlink");
    let env = env_of(&[("HOME", &home)]);

    let mut options = project_options(&project, &env);
    options.edits = vec![edit(&["task", "default_concurrency"], json!(4))];
    let error = update_omo_config(&options).expect_err("expected a typed error");

    assert_eq!(error.operation, "read");
    assert_eq!(
        std::fs::read_to_string(&target_path).expect("target"),
        original
    );
    assert!(backup_files(&target_dir).is_empty());
}
