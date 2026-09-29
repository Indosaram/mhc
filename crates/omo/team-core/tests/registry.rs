//! Translated from src/team-registry/*.test.ts.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use team_core::TeamCoreError;
use team_core::TeamModeConfig;
use team_core::resolve_caller_team_lead::resolve_caller_team_lead;
use team_core::team_registry::{
    NormalizeTeamSpecInputOptions, PathDeps, TeamSpecEntry, discover_team_specs_with,
    ensure_base_dirs, ensure_base_dirs_with, get_inbox_dir, get_runtime_state_dir, get_tasks_dir,
    get_worktree_dir, load_all_team_specs, load_team_spec, normalize_team_spec_input,
    resolve_base_dir, validate_dual_support, validate_member_eligibility, validate_spec,
};
use team_core::types::{Member, SpecSource, TeamSpec};
use tempfile::TempDir;

type LogCalls = Arc<Mutex<Vec<(String, Option<Value>)>>>;

fn now() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("ms")
}

fn home() -> PathBuf {
    dirs::home_dir().expect("home dir")
}

// --- paths.test.ts ---------------------------------------------------------------------------

#[test]
fn resolve_base_dir_defaults_to_home_omo() {
    assert_eq!(
        resolve_base_dir(&TeamModeConfig::default()),
        home().join(".maho")
    );
}

#[test]
fn resolve_base_dir_honors_override() {
    assert_eq!(
        resolve_base_dir(&TeamModeConfig::with_base_dir("/tmp/test-abc")),
        PathBuf::from("/tmp/test-abc")
    );
}

#[test]
fn resolve_base_dir_expands_leading_tilde() {
    assert_eq!(
        resolve_base_dir(&TeamModeConfig::with_base_dir("~/.omo")),
        home().join(".omo")
    );
}

#[test]
fn runtime_paths_reject_traversal_ids() {
    let base = Path::new("/tmp/omo-contained");
    let results = [
        get_runtime_state_dir(base, "../../escape"),
        get_inbox_dir(base, "run-1", "../../escape"),
        get_tasks_dir(base, "../../escape"),
        get_worktree_dir(base, "run-1", "../../escape"),
    ];
    for result in results {
        let error = result.expect_err("traversal must be rejected");
        assert_eq!(error.to_string(), "team path escapes base directory");
    }
}

fn capture_log() -> (LogCalls, team_core::logger::TeamCoreLog) {
    let calls: LogCalls = Arc::default();
    let sink = Arc::clone(&calls);
    (
        calls,
        Arc::new(move |message, data| sink.lock().expect("logs").push((message.to_owned(), data))),
    )
}

#[test]
fn discover_team_specs_prefers_project_scope() {
    let root = tempfile::Builder::new()
        .prefix("team-mode-paths-")
        .tempdir()
        .expect("tempdir");
    let project_root = root.path().join("project");
    let user_base_dir = root.path().join("home").join(".omo");
    let project_team_dir = project_root.join(".omo").join("teams").join("foo");
    let user_team_dir = user_base_dir.join("teams").join("foo");
    fs::create_dir_all(&project_team_dir).expect("project dir");
    fs::create_dir_all(&user_team_dir).expect("user dir");
    fs::write(project_team_dir.join("config.json"), "{}").expect("project spec");
    fs::write(user_team_dir.join("config.json"), "{}").expect("user spec");
    let (calls, log) = capture_log();

    let config = TeamModeConfig::with_base_dir(user_base_dir.display().to_string());
    let specs = discover_team_specs_with(&config, &project_root, &log);

    assert_eq!(
        specs,
        vec![TeamSpecEntry {
            name: "foo".into(),
            scope: SpecSource::Project,
            path: project_team_dir.join("config.json")
        }]
    );
    assert_eq!(
        *calls.lock().expect("logs"),
        vec![(
            "team-spec collision".to_owned(),
            Some(json!({
                "event": "team-spec-collision",
                "teamName": "foo",
                "projectPath": project_team_dir.join("config.json"),
                "userPath": user_team_dir.join("config.json"),
            }))
        )]
    );
}

fn base_directories(base: &Path) -> [PathBuf; 4] {
    [
        base.to_path_buf(),
        base.join("teams"),
        base.join("runtime"),
        base.join("worktrees"),
    ]
}

#[test]
fn ensure_base_dirs_creates_all_dirs_with_mode_0700() {
    let root = TempDir::new().expect("tempdir");
    let base = root
        .path()
        .join(format!("omo-test-{}", uuid::Uuid::new_v4()));
    ensure_base_dirs(&base).expect("first");
    ensure_base_dirs(&base).expect("second");
    for directory in base_directories(&base) {
        let metadata = fs::metadata(&directory).expect("stat");
        assert!(metadata.is_dir());
        assert_eq!(
            metadata.permissions().mode() & 0o777,
            0o700,
            "{}",
            directory.display()
        );
    }
}

#[test]
fn ensure_base_dirs_swallows_eperm_from_chmod_and_logs_warning() {
    let root = TempDir::new().expect("tempdir");
    let base = root
        .path()
        .join(format!("omo-test-eperm-{}", uuid::Uuid::new_v4()));
    for directory in base_directories(&base) {
        fs::create_dir_all(directory).expect("mkdir");
    }
    let chmod_calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&chmod_calls);
    let (calls, log) = capture_log();
    let deps = PathDeps {
        chmod: Box::new(move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            Err(io::Error::from_raw_os_error(libc::EPERM))
        }),
        log,
        ..PathDeps::default()
    };

    ensure_base_dirs_with(&base, &deps).expect("EPERM must not abort");

    assert!(chmod_calls.load(Ordering::SeqCst) > 0);
    let calls = calls.lock().expect("logs");
    let warnings: Vec<&Option<Value>> = calls
        .iter()
        .filter(|(message, _)| {
            message == "team-mode: chmod refused on base directory; continuing with existing permissions"
        })
        .map(|(_, data)| data)
        .collect();
    assert!(!warnings.is_empty());
    let first = warnings[0].as_ref().expect("warning metadata");
    assert!(first.is_object());
    assert_eq!(first["code"], "EPERM");
    assert!(
        first["path"]
            .as_str()
            .expect("path")
            .contains(&base.display().to_string())
    );
}

// --- loader.test.ts / loader-member-name-normalization.test.ts -------------------------------

const ORACLE_REJECTION_MESSAGE: &str = "Agent 'oracle' is read-only (cannot write files). Team members must write to mailbox inbox files. Use delegate-task with subagent_type: 'oracle' for read-only analysis instead.";

struct Fixture {
    _root: TempDir,
    project_root: PathBuf,
    user_base_dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("team-mode-loader-")
            .tempdir()
            .expect("tempdir");
        Self {
            project_root: root.path().join("project"),
            user_base_dir: root.path().join("home").join(".omo"),
            _root: root,
        }
    }

    fn user_config_path(&self, team: &str) -> PathBuf {
        self.user_base_dir
            .join("teams")
            .join(team)
            .join("config.json")
    }

    fn project_config_path(&self, team: &str) -> PathBuf {
        self.project_root
            .join(".omo")
            .join("teams")
            .join(team)
            .join("config.json")
    }

    fn config(&self) -> TeamModeConfig {
        TeamModeConfig::with_base_dir(self.user_base_dir.display().to_string())
    }

    fn load(&self, team: &str) -> team_core::Result<TeamSpec> {
        load_team_spec(team, &self.config(), &self.project_root, None)
    }
}

fn write_json(path: &Path, value: &Value) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(value).expect("json")),
    )
    .expect("write");
}

fn base_spec(team: &str) -> Value {
    json!({
        "version": 1,
        "name": team,
        "description": format!("{team} description"),
        "createdAt": now(),
        "leadAgentId": "lead",
        "members": [
            { "kind": "category", "name": "lead", "category": "deep", "prompt": "implement the leader task" },
            { "kind": "category", "name": "reviewer", "category": "quick", "prompt": "review the current output" },
            { "kind": "category", "name": "tester", "category": "deep", "prompt": "verify the resulting behavior" },
        ],
    })
}

struct SpecErrorView<'a> {
    name: &'a str,
    message: String,
    code: &'a str,
    field: Option<&'a str>,
    member_name: Option<&'a str>,
}

fn spec_error(error: &TeamCoreError) -> SpecErrorView<'_> {
    match error {
        TeamCoreError::TeamSpecValidation {
            code,
            field,
            member_name,
            ..
        } => SpecErrorView {
            name: error.name(),
            message: error.to_string(),
            code,
            field: field.as_deref(),
            member_name: member_name.as_deref(),
        },
        other => panic!("expected TeamSpecValidationError, got {other:?}"),
    }
}

fn member_names(spec: &TeamSpec) -> Vec<&str> {
    spec.members
        .iter()
        .map(|member| member.name.as_str())
        .collect()
}

#[test]
fn loader_loads_and_validates_a_valid_3_member_team_spec() {
    let fixture = Fixture::new();
    write_json(&fixture.user_config_path("alpha"), &base_spec("alpha"));
    let spec = fixture.load("alpha").expect("spec");
    assert_eq!(spec.name, "alpha");
    assert_eq!(spec.members.len(), 3);
    assert_eq!(spec.lead_agent_id, "lead");
}

#[test]
fn loader_defaults_version_when_omitted() {
    let fixture = Fixture::new();
    let mut spec = base_spec("default-version");
    spec.as_object_mut().expect("object").remove("version");
    write_json(&fixture.user_config_path("default-version"), &spec);
    assert_eq!(fixture.load("default-version").expect("spec").version, 1);
}

#[test]
fn loader_defaults_created_at_from_now_when_omitted() {
    // TS stubs Date.now; the crate clock override is crate-private, so bound the default by the wall clock.
    let fixture = Fixture::new();
    let mut spec = base_spec("default-created-at");
    spec.as_object_mut().expect("object").remove("createdAt");
    write_json(&fixture.user_config_path("default-created-at"), &spec);
    let before = now();
    let created_at = fixture.load("default-created-at").expect("spec").created_at;
    let after = now();
    assert!(
        (before..=after).contains(&created_at),
        "{before} <= {created_at} <= {after}"
    );
}

#[test]
fn loader_derives_lead_agent_id_and_prepends_lead_shorthand() {
    let fixture = Fixture::new();
    write_json(
        &fixture.user_config_path("lead-shorthand"),
        &json!({
            "name": "lead-shorthand",
            "description": "team with shorthand lead",
            "lead": { "kind": "subagent_type", "subagent_type": "sisyphus" },
            "members": [
                { "kind": "category", "name": "scout-1", "category": "deep", "prompt": "Scout the src directory for auth patterns." },
                { "kind": "category", "name": "scout-2", "category": "quick", "prompt": "Scout tests for auth coverage." },
            ],
        }),
    );
    let spec = fixture.load("lead-shorthand").expect("spec");
    assert_eq!(spec.lead_agent_id, "lead");
    assert_eq!(spec.members.len(), 3);
    assert_eq!(spec.members[0].kind.as_str(), "subagent_type");
    assert_eq!(spec.members[0].name, "lead");
    assert_eq!(spec.members[0].subagent_type.as_deref(), Some("sisyphus"));
}

#[test]
fn loader_derives_lead_agent_id_from_the_only_member() {
    let fixture = Fixture::new();
    write_json(
        &fixture.user_config_path("solo"),
        &json!({
            "name": "solo",
            "members": [{ "kind": "category", "name": "solo-lead", "category": "deep", "prompt": "Implement the assigned work for the solo team." }],
        }),
    );
    let spec = fixture.load("solo").expect("spec");
    assert_eq!(spec.lead_agent_id, "solo-lead");
    assert_eq!(spec.members.len(), 1);
}

#[test]
fn loader_rejects_multi_member_specs_without_lead_indicator() {
    let fixture = Fixture::new();
    write_json(
        &fixture.user_config_path("missing-lead"),
        &json!({
            "name": "missing-lead",
            "members": [
                { "kind": "category", "name": "member-1", "category": "deep", "prompt": "Implement the assigned work for member one." },
                { "kind": "category", "name": "member-2", "category": "quick", "prompt": "Review the assigned work for member one." },
            ],
        }),
    );
    let error = fixture.load("missing-lead").expect_err("missing lead");
    let view = spec_error(&error);
    assert_eq!(view.name, "TeamSpecValidationError");
    assert_eq!(
        view.message,
        "Invalid team spec field 'leadAgentId': leadAgentId required (or write a `lead: {...}` field, or mark one member with `isLead: true`)"
    );
    assert_eq!(view.code, "INVALID_TEAM_SPEC");
    assert_eq!(view.field, Some("leadAgentId"));
}

#[test]
fn loader_rejects_oracle_subagent_members_with_exact_message() {
    let fixture = Fixture::new();
    let mut spec = base_spec("oracle-team");
    spec["members"] =
        json!([{ "kind": "subagent_type", "name": "lead", "subagent_type": "oracle" }]);
    write_json(&fixture.user_config_path("oracle-team"), &spec);
    let error = fixture.load("oracle-team").expect_err("oracle rejected");
    let view = spec_error(&error);
    assert_eq!(view.name, "TeamSpecValidationError");
    assert_eq!(view.message, ORACLE_REJECTION_MESSAGE);
    assert_eq!(view.code, "INELIGIBLE_AGENT");
    assert_eq!(view.field, Some("subagent_type"));
    assert_eq!(view.member_name, Some("lead"));
}

#[test]
fn loader_prefers_project_scoped_spec_when_both_scopes_define_the_same_name() {
    let fixture = Fixture::new();
    let mut project_spec = base_spec("dup");
    project_spec["description"] = json!("project-owned");
    let mut user_spec = base_spec("dup");
    user_spec["description"] = json!("user-owned");
    write_json(&fixture.project_config_path("dup"), &project_spec);
    write_json(&fixture.user_config_path("dup"), &user_spec);
    assert_eq!(
        fixture.load("dup").expect("spec").description.as_deref(),
        Some("project-owned")
    );
}

#[test]
fn loader_returns_malformed_specs_as_data_during_load_all() {
    let fixture = Fixture::new();
    write_json(&fixture.user_config_path("good"), &base_spec("good"));
    let broken = fixture.user_config_path("broken");
    fs::create_dir_all(broken.parent().expect("parent")).expect("mkdir");
    fs::write(&broken, "{\n  invalid json\n").expect("write");
    let results = load_all_team_specs(&fixture.config(), &fixture.project_root);
    assert_eq!(results.len(), 2);
    let good = results
        .iter()
        .find(|result| result.name == "good")
        .expect("good");
    assert_eq!(good.scope, SpecSource::User);
    assert_eq!(good.spec.as_ref().expect("good spec").name, "good");
    let broken = results
        .iter()
        .find(|result| result.name == "broken")
        .expect("broken");
    assert_eq!(broken.scope, SpecSource::User);
    let error = broken.error.as_ref().expect("broken error");
    assert_eq!(error.name(), "TeamSpecValidationError");
    assert_eq!(error.code(), Some("INVALID_JSON"));
}

#[test]
fn loader_rejects_specs_with_more_than_8_members() {
    let fixture = Fixture::new();
    let mut spec = base_spec("too-many");
    spec["members"] = Value::Array(
        (0..9)
            .map(|index| {
                json!({ "kind": "category", "name": format!("member-{index}"), "category": "deep", "prompt": format!("implement task number {index}") })
            })
            .collect(),
    );
    spec["leadAgentId"] = json!("member-0");
    write_json(&fixture.user_config_path("too-many"), &spec);
    let error = fixture.load("too-many").expect_err("too many");
    let view = spec_error(&error);
    assert_eq!(view.name, "TeamSpecValidationError");
    assert_eq!(view.message, "Team 'too-many' exceeds max 8 members.");
    assert_eq!(view.code, "TEAM_MEMBER_LIMIT_EXCEEDED");
    assert_eq!(view.field, Some("members"));
}

#[test]
fn loader_auto_assigns_missing_member_names_for_specs_on_disk() {
    let fixture = Fixture::new();
    write_json(
        &fixture.user_config_path("autoname"),
        &json!({
            "name": "autoname",
            "lead": { "kind": "subagent_type", "subagent_type": "sisyphus" },
            "members": [
                { "kind": "category", "category": "quick", "prompt": "Quick scout the workspace structure." },
                { "kind": "category", "category": "deep", "prompt": "Deep dive the runtime setup." },
                { "kind": "category", "category": "deep", "prompt": "Deep dive the mailbox implementation." },
                { "kind": "subagent_type", "subagent_type": "atlas" },
            ],
        }),
    );
    let spec = fixture.load("autoname").expect("spec");
    assert_eq!(spec.lead_agent_id, "lead");
    assert_eq!(
        member_names(&spec),
        vec!["lead", "quick-1", "deep-1", "deep-2", "atlas-1"]
    );
}

#[test]
fn loader_injects_caller_as_lead_for_preset_specs_without_lead_metadata() {
    let fixture = Fixture::new();
    write_json(
        &fixture.user_config_path("caller-lead"),
        &json!({
            "name": "caller-lead",
            "members": [
                { "kind": "category", "category": "quick", "prompt": "Quick scout the workspace structure." },
                { "kind": "subagent_type", "subagent_type": "atlas" },
            ],
        }),
    );
    let options = caller_options("\u{200B}Sisyphus - Ultraworker");
    let spec = load_team_spec(
        "caller-lead",
        &fixture.config(),
        &fixture.project_root,
        Some(&options),
    )
    .expect("spec");
    assert_eq!(spec.lead_agent_id, "lead");
    assert_eq!(member_names(&spec), vec!["lead", "quick-1", "atlas-1"]);
}

// --- team-spec-input-normalizer.test.ts ------------------------------------------------------

fn caller_options(agent: &str) -> NormalizeTeamSpecInputOptions {
    NormalizeTeamSpecInputOptions {
        caller_team_lead: Some(resolve_caller_team_lead(Some(agent))),
        ..NormalizeTeamSpecInputOptions::default()
    }
}

/// `toMatchObject`: every key in `expected` matches `actual`; arrays match element-wise with equal length.
fn assert_matches(actual: &Value, expected: &Value, path: &str) {
    match (actual, expected) {
        (Value::Object(actual), Value::Object(expected)) => {
            for (key, value) in expected {
                let child = actual
                    .get(key)
                    .unwrap_or_else(|| panic!("missing {path}.{key} in {actual:?}"));
                assert_matches(child, value, &format!("{path}.{key}"));
            }
        }
        (Value::Array(actual), Value::Array(expected)) => {
            assert_eq!(actual.len(), expected.len(), "length of {path}");
            for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                assert_matches(actual, expected, &format!("{path}[{index}]"));
            }
        }
        _ => assert_eq!(actual, expected, "at {path}"),
    }
}

#[test]
fn normalizer_injects_the_caller_as_lead_when_no_lead_is_specified() {
    let raw = json!({ "name": "alpha-team", "members": [{ "kind": "category", "category": "quick", "prompt": "Inspect the workspace" }] });
    let normalized = normalize_team_spec_input(
        &raw,
        Some(&caller_options("\u{200B}Sisyphus - Ultraworker")),
    )
    .expect("normalized");
    assert_matches(
        &normalized,
        &json!({
            "leadAgentId": "lead",
            "members": [
                { "name": "lead", "kind": "subagent_type", "subagent_type": "sisyphus" },
                { "name": "quick-1", "kind": "category", "category": "quick" },
            ],
        }),
        "$",
    );
}

#[test]
fn normalizer_keeps_explicit_lead_agent_id_unchanged() {
    let raw = json!({
        "name": "alpha-team",
        "leadAgentId": "captain",
        "members": [
            { "kind": "subagent_type", "name": "captain", "subagent_type": "atlas" },
            { "kind": "category", "name": "member-1", "category": "quick", "prompt": "Inspect the workspace" },
        ],
    });
    let normalized =
        normalize_team_spec_input(&raw, Some(&caller_options("Sisyphus - Ultraworker")))
            .expect("normalized");
    assert_eq!(normalized, raw);
}

#[test]
fn normalizer_prefers_is_lead_over_the_caller() {
    let raw = json!({
        "name": "alpha-team",
        "members": [
            { "kind": "subagent_type", "name": "captain", "subagent_type": "atlas", "isLead": true },
            { "kind": "category", "category": "quick", "prompt": "Inspect the workspace" },
        ],
    });
    let normalized =
        normalize_team_spec_input(&raw, Some(&caller_options("Sisyphus - Ultraworker")))
            .expect("normalized");
    assert_matches(
        &normalized,
        &json!({
            "leadAgentId": "captain",
            "members": [
                { "kind": "subagent_type", "name": "captain", "subagent_type": "atlas" },
                { "kind": "category", "name": "quick-1", "category": "quick" },
            ],
        }),
        "$",
    );
}

const INELIGIBLE_CALLER: &str =
    "Caller agent explore is not eligible as team lead; specify leadAgentId explicitly";

#[test]
fn normalizer_throws_when_caller_is_not_eligible_and_no_lead_is_specified() {
    let raw = json!({ "name": "alpha-team", "members": [{ "kind": "category", "category": "quick", "prompt": "Inspect the workspace" }] });
    let error =
        normalize_team_spec_input(&raw, Some(&caller_options("explore"))).expect_err("ineligible");
    assert!(error.to_string().contains(INELIGIBLE_CALLER), "{error}");
}

fn eight_inline_members() -> Value {
    json!({
        "name": "eight-member-team",
        "members": (0..8).map(|_| json!({ "category": "quick", "prompt": "Complete one validation task." })).collect::<Vec<_>>(),
    })
}

#[test]
fn normalizer_still_requires_eligible_caller_or_explicit_lead_for_8_inline_members() {
    let error =
        normalize_team_spec_input(&eight_inline_members(), Some(&caller_options("explore")))
            .expect_err("ineligible");
    assert!(error.to_string().contains(INELIGIBLE_CALLER), "{error}");
}

#[test]
fn normalizer_normalizes_natural_inline_names_to_schema_safe_names() {
    let raw = json!({
        "name": "Project Analysis Team",
        "leadAgentId": "Agent Lead",
        "members": [
            { "kind": "category", "name": "Agent Lead", "category": "quick", "prompt": "Lead the analysis work" },
            { "kind": "category", "name": "Agent 1: Structure Analyst", "category": "quick", "prompt": "Inspect the workspace" },
            { "kind": "category", "name": "Agent 1 Structure Analyst", "category": "quick", "prompt": "Inspect related tests" },
        ],
    });
    let normalized =
        normalize_team_spec_input(&raw, Some(&caller_options("Sisyphus - Ultraworker")))
            .expect("normalized");
    assert_matches(
        &normalized,
        &json!({
            "name": "project-analysis-team",
            "leadAgentId": "agent-lead",
            "members": [{ "name": "agent-lead" }, { "name": "agent-1-structure-analyst" }, { "name": "agent-1-structure-analyst-2" }],
        }),
        "$",
    );
}

#[test]
fn normalizer_uses_default_category_for_role_only_natural_members() {
    let raw = json!({
        "name": "analysis-team",
        "members": [{ "name": "Structure Analyst", "role": "Structure Analyst", "capabilities": ["structure", "modules"] }],
    });
    let options = NormalizeTeamSpecInputOptions {
        default_category_name: Some("analysis".into()),
        ..caller_options("Sisyphus - Ultraworker")
    };
    let normalized = normalize_team_spec_input(&raw, Some(&options)).expect("normalized");
    assert_matches(
        &normalized,
        &json!({
            "members": [
                { "name": "lead", "kind": "subagent_type" },
                { "name": "structure-analyst", "kind": "category", "category": "analysis", "prompt": "Role: Structure Analyst\nstructure, modules" },
            ],
        }),
        "$",
    );
}

#[test]
fn normalizer_uses_first_generated_member_as_lead_when_8_inline_members_leave_no_room() {
    let normalized = normalize_team_spec_input(
        &eight_inline_members(),
        Some(&caller_options("Sisyphus - Ultraworker")),
    )
    .expect("normalized");
    let members: Vec<Value> = (1..=8)
        .map(|index| json!({ "name": format!("quick-{index}"), "kind": "category" }))
        .collect();
    assert_matches(
        &normalized,
        &json!({ "leadAgentId": "quick-1", "members": members }),
        "$",
    );
}

#[test]
fn normalizer_strips_empty_string_optional_fields_injected_by_tool_host() {
    let raw = json!({
        "name": "hyperplan-smoke-test",
        "members": [{ "name": "worker", "kind": "category", "category": "quick", "subagent_type": "", "prompt": "Temporary smoke test member.", "cwd": "", "worktreePath": "", "color": "" }],
        "leadAgentId": "",
        "sessionPermission": "",
    });
    let normalized = normalize_team_spec_input(&raw, None).expect("normalized");
    assert_matches(
        &normalized,
        &json!({
            "leadAgentId": "worker",
            "members": [{ "name": "worker", "kind": "category", "category": "quick", "prompt": "Temporary smoke test member." }],
        }),
        "$",
    );
    assert!(!normalized["members"].to_string().contains("subagent_type"));
    assert!(
        normalized
            .get("sessionPermission")
            .is_none_or(Value::is_null)
    );
}

#[test]
fn normalizer_treats_an_all_empty_lead_object_as_absent() {
    let raw = json!({
        "name": "hyperplan-smoke-test",
        "members": [{ "name": "worker", "kind": "category", "category": "quick", "prompt": "Temporary smoke test member." }],
        "lead": { "name": "", "kind": "", "category": "", "subagent_type": "", "prompt": "" },
    });
    let normalized = normalize_team_spec_input(&raw, None).expect("normalized");
    assert_matches(
        &normalized,
        &json!({ "leadAgentId": "worker", "members": [{ "name": "worker", "kind": "category", "category": "quick" }] }),
        "$",
    );
}

// --- validator.test.ts -----------------------------------------------------------------------

const PROMETHEUS_REJECTION_MESSAGE: &str = "Agent 'prometheus' is plan-mode-only; can only write to .omo/*.md (enforced by prometheusMdOnly hook). Cannot write to team mailbox. Use delegate-task with subagent_type: 'plan' instead.";

fn category_member(name: &str) -> Member {
    Member::category(
        name,
        "deep",
        &format!("implement the assigned work for {name}"),
    )
}

fn hyperplan_member(name: &str, category: &str) -> Member {
    Member::category(
        name,
        category,
        &format!("perform the {name} adversarial role"),
    )
}

fn base_team_spec() -> TeamSpec {
    TeamSpec {
        version: 1,
        name: "validator-team".into(),
        description: None,
        created_at: 1,
        lead_agent_id: "lead".into(),
        team_allowed_paths: None,
        session_permission: None,
        members: vec![category_member("lead"), category_member("reviewer")],
    }
}

fn base_team_spec_json(members: Value) -> Value {
    json!({ "version": 1, "name": "validator-team", "createdAt": 1, "leadAgentId": "lead", "members": members })
}

#[test]
fn validator_rejects_members_with_both_category_and_subagent_type() {
    let spec = base_team_spec_json(json!([{
        "kind": "category", "name": "lead", "category": "deep",
        "prompt": "implement the assigned work for lead", "subagent_type": "sisyphus",
    }]));
    assert!(TeamSpec::safe_parse(&spec).is_err());
}

#[test]
fn validator_rejects_members_that_omit_the_kind_discriminator() {
    let spec = base_team_spec_json(
        json!([{ "name": "lead", "category": "deep", "prompt": "implement the assigned work for lead" }]),
    );
    assert!(TeamSpec::safe_parse(&spec).is_err());
}

#[test]
fn validator_rejects_prometheus_subagent_members_with_exact_message() {
    let error = validate_member_eligibility(&Member::subagent("planner", "prometheus"))
        .expect_err("prometheus rejected");
    assert_eq!(error.to_string(), PROMETHEUS_REJECTION_MESSAGE);
    assert_eq!(error.name(), "TeamSpecValidationError");
}

#[test]
fn validator_accepts_hephaestus_subagent_members() {
    validate_member_eligibility(&Member::subagent("craftsman", "hephaestus"))
        .expect("hephaestus eligible");
}

#[test]
fn validator_rejects_lead_agent_id_not_matching_a_member() {
    let spec = TeamSpec {
        lead_agent_id: "ghost".into(),
        ..base_team_spec()
    };
    let error = validate_spec(&spec).expect_err("ghost lead");
    assert_eq!(
        error.to_string(),
        "Team 'validator-team' leadAgentId 'ghost' must match exactly one member.name."
    );
}

#[test]
fn validator_rejects_duplicate_member_names() {
    let spec = TeamSpec {
        members: vec![category_member("lead"), category_member("lead")],
        ..base_team_spec()
    };
    let error = validate_spec(&spec).expect_err("duplicate");
    assert_eq!(
        error.to_string(),
        "Member name 'lead' is duplicated within team 'validator-team'. Member names must be unique."
    );
}

fn n_members(count: usize) -> TeamSpec {
    TeamSpec {
        members: (0..count)
            .map(|index| category_member(&format!("member-{index}")))
            .collect(),
        lead_agent_id: "member-0".into(),
        ..base_team_spec()
    }
}

#[test]
fn validator_rejects_teams_exceeding_the_8_member_cap() {
    let error = validate_spec(&n_members(9)).expect_err("too many");
    assert_eq!(
        error.to_string(),
        "Team 'validator-team' exceeds max 8 members."
    );
}

#[test]
fn validator_accepts_teams_with_exactly_8_members() {
    validate_spec(&n_members(8)).expect("8 members allowed");
}

fn hyperplan_spec(members: Vec<Member>) -> TeamSpec {
    TeamSpec {
        name: "hyperplan".into(),
        lead_agent_id: "architect".into(),
        members,
        ..base_team_spec()
    }
}

#[test]
fn validator_rejects_hyperplan_teams_missing_adversarial_categories() {
    let spec = hyperplan_spec(vec![
        hyperplan_member("researcher", "deep"),
        hyperplan_member("architect", "ultrabrain"),
    ]);
    assert_eq!(
        validate_spec(&spec).expect_err("missing").to_string(),
        "Hyperplan team must include category 'unspecified-low'."
    );
}

#[test]
fn validator_accepts_hyperplan_teams_with_required_categories() {
    let spec = hyperplan_spec(vec![
        hyperplan_member("skeptic", "unspecified-low"),
        hyperplan_member("validator", "unspecified-high"),
        hyperplan_member("architect", "ultrabrain"),
        hyperplan_member("creative", "artistry"),
    ]);
    validate_spec(&spec).expect("hyperplan valid");
}

#[test]
fn validator_rejects_category_prompts_that_collapse_to_empty_text() {
    let error =
        validate_dual_support(&Member::category("lead", "deep", "   ")).expect_err("empty prompt");
    assert_eq!(
        error.to_string(),
        "Member 'lead' prompt must not be empty after trimming whitespace."
    );
}
