//! Translated from jsonc-parser, jsonc-parser.memoization, frontmatter, omo-config, atomic-write,
//! xdg-data-dir and file-utils tests.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tempfile::TempDir;
use utils::*;

fn tempdir() -> TempDir {
    TempDir::new().unwrap_or_else(|error| panic!("{error}"))
}

fn parse(text: &str) -> Value {
    parse_jsonc(text).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn parse_jsonc_accepts_comments_trailing_commas_and_bom() {
    let cases = [
        (r#"{"key": "value"}"#, json!({"key": "value"})),
        (
            "{\n // This is a comment\n \"key\": \"value\"\n}",
            json!({"key": "value"}),
        ),
        (
            "{\n /* Block comment */\n \"key\": \"value\"\n}",
            json!({"key": "value"}),
        ),
        (
            "{\n /* Multi-line\n comment\n here */\n \"key\": \"value\"\n}",
            json!({"key": "value"}),
        ),
        (
            "{\n \"key1\": \"value1\",\n \"key2\": \"value2\",\n}",
            json!({"key1": "value1", "key2": "value2"}),
        ),
        ("{\n \"arr\": [1, 2, 3,]\n}", json!({"arr": [1, 2, 3]})),
        (
            "{\n \"url\": \"https://example.com\"\n}",
            json!({"url": "https://example.com"}),
        ),
        ("\u{feff}{\"key\": \"value\"}", json!({"key": "value"})),
        (
            "\u{feff}{\n // Windows-saved file with BOM\n \"$schema\": \"https://opencode.ai/config.json\",\n \"plugin\": [\"oh-my-openagent@3.15.3\"],\n}",
            json!({"$schema": "https://opencode.ai/config.json", "plugin": ["oh-my-openagent@3.15.3"]}),
        ),
    ];
    for (text, expected) in cases {
        assert_eq!(parse(text), expected, "{text:?}");
    }
}

#[test]
fn parse_jsonc_complex_config() {
    let text = "{\n // example\n \"agents\": {\n \"oracle\": { \"model\": \"openai/gpt-5.4\" }, // GPT\n },\n /* Agent overrides */\n \"disabled_agents\": [],\n}";
    assert_eq!(
        parse(text),
        json!({"agents": {"oracle": {"model": "openai/gpt-5.4"}}, "disabled_agents": []})
    );
}

#[test]
fn parse_jsonc_rejects_invalid_input() {
    assert!(parse_jsonc(r#"{ "key": invalid }"#).is_err());
    assert!(parse_jsonc(r#"{ "key": "unclosed }"#).is_err());
}

#[test]
fn parse_jsonc_safe_reports_data_or_errors() {
    let valid = parse_jsonc_safe(r#"{ "key": "value" }"#);
    assert_eq!(
        (valid.data, valid.errors.len()),
        (Some(json!({"key": "value"})), 0)
    );
    let invalid = parse_jsonc_safe(r#"{ "key": invalid }"#);
    assert_eq!(invalid.data, None);
    assert!(!invalid.errors.is_empty());
    let bom = parse_jsonc_safe("\u{feff}{\"key\": \"value\"}");
    assert_eq!(
        (bom.data, bom.errors.len()),
        (Some(json!({"key": "value"})), 0)
    );
}

#[test]
fn read_jsonc_file_cases() {
    let dir = tempdir();
    let file = dir.path().join("fixture.jsonc");
    fs::write(&file, "{\n // Comment\n \"test\": \"value\"\n}").unwrap_or_default();
    assert_eq!(read_jsonc_file(&file), Some(json!({"test": "value"})));
    assert_eq!(
        read_jsonc_file(dir.path().join("does-not-exist.jsonc")),
        None
    );
    fs::write(&file, "{ invalid }").unwrap_or_default();
    assert_eq!(read_jsonc_file(&file), None);
    let mut bytes = vec![0xef, 0xbb, 0xbf];
    bytes.extend_from_slice(b"{\n // BOM\n \"$schema\": \"https://opencode.ai/config.json\",\n \"plugin\": [\"oh-my-openagent@3.15.3\"]\n}");
    fs::write(&file, bytes).unwrap_or_default();
    assert_eq!(
        read_jsonc_file(&file),
        Some(
            json!({"$schema": "https://opencode.ai/config.json", "plugin": ["oh-my-openagent@3.15.3"]})
        )
    );
}

#[test]
fn detect_config_file_prefers_jsonc_then_json_then_none() {
    let dir = tempdir();
    let base = dir.path().join("config");
    let with = |suffix: &str| PathBuf::from(format!("{}{suffix}", base.display()));
    fs::write(with(".json"), "{}").unwrap_or_default();
    assert_eq!(
        detect_config_file(&base),
        DetectedConfigFile {
            format: ConfigFormat::Json,
            path: with(".json")
        }
    );
    fs::write(with(".jsonc"), "{}").unwrap_or_default();
    assert_eq!(
        detect_config_file(&base),
        DetectedConfigFile {
            format: ConfigFormat::Jsonc,
            path: with(".jsonc")
        }
    );
    assert_eq!(
        detect_config_file(dir.path().join("nonexistent")).format,
        ConfigFormat::None
    );
}

fn plugin_options() -> DetectPluginConfigFileOptions {
    DetectPluginConfigFileOptions {
        basenames: vec!["oh-my-openagent".to_string()],
        legacy_basenames: vec!["oh-my-opencode".to_string()],
    }
}

fn detect_with(files: &[&str]) -> (TempDir, DetectPluginConfigResult) {
    let dir = tempdir();
    for file in files {
        fs::write(dir.path().join(file), "{}").unwrap_or_default();
    }
    let result = detect_plugin_config_file(dir.path(), &plugin_options());
    (dir, result)
}

#[test]
fn detect_plugin_config_file_cases() {
    let cases: [(&[&str], ConfigFormat, &str, Option<&str>); 5] = [
        (
            &["oh-my-openagent.jsonc", "oh-my-opencode.jsonc"],
            ConfigFormat::Jsonc,
            "oh-my-openagent.jsonc",
            Some("oh-my-opencode.jsonc"),
        ),
        (
            &["oh-my-opencode.jsonc"],
            ConfigFormat::Jsonc,
            "oh-my-opencode.jsonc",
            None,
        ),
        (
            &["oh-my-openagent.json", "oh-my-opencode.json"],
            ConfigFormat::Json,
            "oh-my-openagent.json",
            Some("oh-my-opencode.json"),
        ),
        (
            &["oh-my-opencode.json", "oh-my-openagent.jsonc"],
            ConfigFormat::Jsonc,
            "oh-my-openagent.jsonc",
            Some("oh-my-opencode.json"),
        ),
        (
            &["oh-my-openagent.jsonc"],
            ConfigFormat::Jsonc,
            "oh-my-openagent.jsonc",
            None,
        ),
    ];
    for (files, format, path, legacy) in cases {
        let (dir, result) = detect_with(files);
        let expected = DetectPluginConfigResult {
            format,
            path: dir.path().join(path),
            legacy_path: legacy.map(|legacy| dir.path().join(legacy)),
        };
        assert_eq!(result, expected, "{files:?}");
    }
}

#[test]
fn detect_plugin_config_file_none_when_empty() {
    let (dir, result) = detect_with(&[]);
    assert_eq!(
        result,
        DetectPluginConfigResult {
            format: ConfigFormat::None,
            path: dir.path().join("oh-my-openagent.json"),
            legacy_path: None
        }
    );
}

#[test]
fn detect_plugin_config_file_memoizes_until_cleared() {
    let dir = tempdir();
    fs::write(dir.path().join("oh-my-openagent.jsonc"), "{}").unwrap_or_default();
    let first = detect_plugin_config_file(dir.path(), &plugin_options());
    fs::remove_file(dir.path().join("oh-my-openagent.jsonc")).unwrap_or_default();
    assert_eq!(
        detect_plugin_config_file(dir.path(), &plugin_options()),
        first
    );

    clear_plugin_config_file_detection_cache();
    assert_eq!(
        detect_plugin_config_file(dir.path(), &plugin_options()).format,
        ConfigFormat::None
    );
}

fn front(content: &str) -> FrontmatterResult {
    parse_frontmatter(content, FrontmatterMode::Default)
}

#[test]
fn frontmatter_simple_and_boolean_values() {
    let simple = front("---\ndescription: Test command\nagent: build\n---\nBody content");
    assert_eq!(
        (simple.data, simple.body.as_str()),
        (
            json!({"description": "Test command", "agent": "build"}),
            "Body content"
        )
    );
    let booleans = front("---\nsubtask: true\nenabled: false\n---\nBody");
    assert_eq!(
        (booleans.data, booleans.body.as_str()),
        (json!({"subtask": true, "enabled": false}), "Body")
    );
}

#[test]
fn frontmatter_complex_arrays_and_nested_objects() {
    let handoffs = front(
        "---\ndescription: Execute planning workflow\nhandoffs:\n  - label: Create Tasks\n    agent: speckit.tasks\n    prompt: Break the plan into tasks\n    send: true\n  - label: Create Checklist\n    agent: speckit.checklist\n    prompt: Create a checklist\n---\nWorkflow instructions",
    );
    assert_eq!(
        handoffs.data,
        json!({"description": "Execute planning workflow", "handoffs": [
            {"label": "Create Tasks", "agent": "speckit.tasks", "prompt": "Break the plan into tasks", "send": true},
            {"label": "Create Checklist", "agent": "speckit.checklist", "prompt": "Create a checklist"},
        ]})
    );
    let nested = front(
        "---\nname: test\nconfig:\n  timeout: 5000\n  retry: true\n  options:\n    verbose: false\n---\nContent",
    );
    assert_eq!(
        nested.data,
        json!({"name": "test", "config": {"timeout": 5000, "retry": true, "options": {"verbose": false}}})
    );
}

#[test]
fn frontmatter_edge_cases() {
    let cases = [
        ("Just body content", "Just body content"),
        ("---\n---\nBody", "Body"),
        (
            "---\ninvalid: yaml: syntax: here\n  bad indentation\n---\nBody",
            "Body",
        ),
        (
            "---\n   \n---\nBody with whitespace-only frontmatter",
            "Body with whitespace-only frontmatter",
        ),
    ];
    for (content, body) in cases {
        let result = front(content);
        assert_eq!(
            (result.data, result.body.as_str()),
            (json!({}), body),
            "{content:?}"
        );
    }
}

#[test]
fn frontmatter_mixed_content() {
    let multiline = front("---\ntitle: Test\n---\nLine 1\nLine 2\n\nLine 4 after blank");
    assert_eq!(
        (multiline.data, multiline.body.as_str()),
        (
            json!({"title": "Test"}),
            "Line 1\nLine 2\n\nLine 4 after blank"
        )
    );
    let crlf = front("---\r\ndescription: Test\r\n---\r\nBody");
    assert_eq!(
        (crlf.data, crlf.body.as_str()),
        (json!({"description": "Test"}), "Body")
    );
}

#[test]
fn frontmatter_tolerates_extra_fields() {
    let result = front(
        "---\ndescription: Test command\nagent: build\nextra_field: should not fail\nanother_extra:\n  nested: value\n  array:\n    - item1\n    - item2\ncustom_boolean: true\ncustom_number: 42\n---\nBody content",
    );
    assert_eq!(result.body, "Body content");
    assert_eq!(
        result.data,
        json!({
            "description": "Test command", "agent": "build", "extra_field": "should not fail",
            "another_extra": {"nested": "value", "array": ["item1", "item2"]},
            "custom_boolean": true, "custom_number": 42,
        })
    );
    let handoff = front(
        "---\ndescription: Original description\nunknown_field: extra value\nhandoffs:\n  - label: Task 1\n    agent: test.agent\n---\nContent",
    );
    assert_eq!(handoff.data["description"], json!("Original description"));
    assert_eq!(
        handoff.data["handoffs"],
        json!([{"label": "Task 1", "agent": "test.agent"}])
    );
}

#[test]
fn validate_omo_config_accepts_supported_overrides() {
    let config = json!({
        "codegraph": {
            "auto_provision": true, "enabled": true, "install_dir": "~/.omo/codegraph",
            "excluded_roots": ["/tmp/omo-scratch"], "telemetry": false,
            "session_start_cooldown_ms": 900_000, "watch_debounce_ms": 2_000,
        },
        "[codex]": {"codegraph": {"enabled": false}},
        "[opencode]": {"codegraph": {"watch_debounce_ms": 500}},
    });
    assert_eq!(
        validate_omo_config(&config),
        OmoConfigValidationResult {
            errors: vec![],
            ok: true
        }
    );
}

#[test]
fn validate_omo_config_rejections() {
    let cases = [
        (
            json!({"[codex]": {"codegraph": {"session_start_cooldown_ms": 59_999}}}),
            "[codex].codegraph.session_start_cooldown_ms must be a finite number of at least 60000",
        ),
        (
            json!({"[android]": {}}),
            "Unknown harness override block \"[android]\"",
        ),
        (
            json!({"[codex]": {"codegraph": {"watch_debounce_ms": 250}}}),
            "codegraph.watch_debounce_ms is not supported for harness codex",
        ),
    ];
    for (config, message) in cases {
        let result = validate_omo_config(&config);
        assert!(!result.ok);
        assert!(
            result.errors.iter().any(|error| error == message),
            "{:?}",
            result.errors
        );
    }
}

#[test]
fn write_file_atomically_behaviour() {
    let dir = tempdir();
    let target = dir.path().join("state.json");
    write_file_atomically(&target, "first-content").unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        fs::read_to_string(&target).unwrap_or_default(),
        "first-content"
    );
    fs::write(&target, "old-content").unwrap_or_default();
    write_file_atomically(&target, "new-content").unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        fs::read_to_string(&target).unwrap_or_default(),
        "new-content"
    );
    assert!(!PathBuf::from(format!("{}.tmp", target.display())).exists());
    for content in ["first", "second", "third"] {
        write_file_atomically(&target, content).unwrap_or_else(|error| panic!("{error}"));
    }
    assert_eq!(fs::read_to_string(&target).unwrap_or_default(), "third");
    assert!(
        write_file_atomically(dir.path().join("missing-parent/state.json"), "content").is_err()
    );
}

struct FixedOs {
    home: PathBuf,
    tmp: PathBuf,
}

impl XdgOsProvider for FixedOs {
    fn homedir(&self) -> PathBuf {
        self.home.clone()
    }

    fn tmpdir(&self) -> PathBuf {
        self.tmp.clone()
    }
}

#[test]
fn resolve_xdg_data_dir_cases() {
    let xdg = tempdir();
    let env = HashMap::from([(
        "XDG_DATA_HOME".to_string(),
        xdg.path().display().to_string(),
    )]);
    let options = ResolveXdgDataDirOptions {
        env: Some(&env),
        os_provider: None,
    };
    assert_eq!(resolve_xdg_data_dir("omo-codex", &options), xdg.path());

    let (home, tmp) = (tempdir(), tempdir());
    let provider = FixedOs {
        home: home.path().to_path_buf(),
        tmp: tmp.path().to_path_buf(),
    };
    let file_path = xdg.path().join("xdg-data-home");
    fs::write(&file_path, "not-a-directory").unwrap_or_default();
    let file_env = HashMap::from([("XDG_DATA_HOME".to_string(), file_path.display().to_string())]);
    let fallback = ResolveXdgDataDirOptions {
        env: Some(&file_env),
        os_provider: Some(&provider),
    };
    assert_eq!(
        resolve_xdg_data_dir("opencode", &fallback),
        tmp.path().join("opencode-data")
    );

    let empty = HashMap::new();
    let absent = ResolveXdgDataDirOptions {
        env: Some(&empty),
        os_provider: Some(&provider),
    };
    assert_eq!(
        resolve_xdg_data_dir("omo-codex", &absent),
        home.path().join(".local").join("share")
    );
}

fn expected_realpath(path: &Path) -> PathBuf {
    let real = fs::canonicalize(path).unwrap_or_else(|error| panic!("{error}"));
    match real.to_str().and_then(|text| text.strip_prefix("/private")) {
        Some(rest) if rest.starts_with("/var/") => PathBuf::from(rest),
        _ => real,
    }
}

#[cfg(unix)]
#[test]
fn symlink_resolution_and_existence() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let dir = tempdir();
    let real_skill = dir.path().join("repo/skills/category/my-skill");
    let repo_skills = dir.path().join("repo/.opencode/skills");
    let config_skills = dir.path().join("config/skills");
    fs::create_dir_all(&real_skill).unwrap_or_default();
    fs::write(real_skill.join("SKILL.md"), "# My Skill").unwrap_or_default();
    fs::create_dir_all(&repo_skills).unwrap_or_default();
    symlink(
        "../../skills/category/my-skill",
        repo_skills.join("my-skill"),
    )
    .unwrap_or_default();
    fs::create_dir_all(dir.path().join("config")).unwrap_or_default();
    symlink(&repo_skills, &config_skills).unwrap_or_default();

    let file = real_skill.join("SKILL.md");
    let missing = dir.path().join("does-not-exist");
    let resolvers: [fn(&Path) -> PathBuf; 2] = [
        |path| resolve_symlink(path),
        |path| resolve_symlink_async(path),
    ];
    for resolve in resolvers {
        assert_eq!(resolve(&file), expected_realpath(&file));
        assert_eq!(
            resolve(&repo_skills.join("my-skill")),
            expected_realpath(&real_skill)
        );
        assert_eq!(
            resolve(&config_skills.join("my-skill")),
            expected_realpath(&real_skill)
        );
        assert_eq!(resolve(&missing), missing);
    }
    assert!(is_symbolic_link(repo_skills.join("my-skill")));
    assert!(!is_symbolic_link(&real_skill));
    assert!(!is_symbolic_link(&missing));

    assert!(file_exists(&file));
    assert!(!file_exists(dir.path().join("missing-file")));
    assert_eq!(file_exists_strict(&file).ok(), Some(true));
    assert_eq!(
        file_exists_strict(dir.path().join("missing-strict-file")).ok(),
        Some(false)
    );

    let locked = dir.path().join("locked");
    fs::create_dir_all(&locked).unwrap_or_default();
    fs::write(locked.join("secret.txt"), "secret").unwrap_or_default();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o0)).unwrap_or_default();
    let lenient = file_exists(locked.join("secret.txt"));
    let strict = file_exists_strict(locked.join("secret.txt"));
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap_or_default();
    assert!(!lenient);
    assert!(strict.is_err());
}
