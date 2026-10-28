//! The key set of `omo.jsonc` (config-keys.md) as validation rules, with defaults.
//!
//! Two modes: [`Mode::Layer`] (one file: every field optional except inside list elements, no
//! defaults) and [`Mode::Full`] (merged view: required fields enforced, defaults applied inside
//! every present object).

use regex::Regex;
use serde_json::{Map, Value, json};
use std::sync::OnceLock;

/// Validation mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Layer,
    Full,
}

/// One segment of an issue path.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Seg {
    Key(String),
    Index(usize),
}

/// A path inside a document.
pub type Path = Vec<Seg>;

/// Renders a path as dotted text (`agents.x.models.0`); the root renders as an empty string.
pub fn dotted(path: &[Seg]) -> String {
    path.iter()
        .map(|seg| match seg {
            Seg::Key(key) => key.clone(),
            Seg::Index(index) => index.to_string(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// What kind of problem an issue is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueKind {
    UnknownKey,
    Invalid,
}

/// One validation problem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub kind: IssueKind,
    pub path: Path,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct Bounds {
    min: Option<f64>,
    max: Option<f64>,
    exclusive_min: Option<f64>,
}

/// A validation rule.
#[derive(Clone, Debug)]
pub enum Rule {
    Any,
    Bool,
    Str {
        allowed: Option<&'static [&'static str]>,
        min_len: usize,
        pattern: Option<&'static str>,
    },
    Int(Bounds),
    Num(Bounds),
    Const(Value),
    Array(Box<Rule>),
    /// An object with arbitrary keys, each value checked by the inner rule.
    Map(Box<Rule>),
    /// A strict object: only the listed fields are allowed.
    Object(Vec<Field>),
    /// The first alternative whose JSON type matches the value is used.
    ByType(Vec<Rule>),
    /// An object discriminated by a string tag field.
    Tagged {
        tag: &'static str,
        variants: Vec<(&'static str, Rule)>,
    },
}

/// A field of a strict object.
#[derive(Clone, Debug)]
pub struct Field {
    pub name: &'static str,
    pub rule: Rule,
    pub required: bool,
    pub default: Option<Value>,
}

fn f(name: &'static str, rule: Rule) -> Field {
    Field {
        name,
        rule,
        required: false,
        default: None,
    }
}
fn req(name: &'static str, rule: Rule) -> Field {
    Field {
        required: true,
        ..f(name, rule)
    }
}
fn d(name: &'static str, rule: Rule, default: Value) -> Field {
    Field {
        default: Some(default),
        ..f(name, rule)
    }
}

fn s() -> Rule {
    Rule::Str {
        allowed: None,
        min_len: 0,
        pattern: None,
    }
}
fn s1() -> Rule {
    Rule::Str {
        allowed: None,
        min_len: 1,
        pattern: None,
    }
}
fn e(allowed: &'static [&'static str]) -> Rule {
    Rule::Str {
        allowed: Some(allowed),
        min_len: 0,
        pattern: None,
    }
}
fn pat(pattern: &'static str) -> Rule {
    Rule::Str {
        allowed: None,
        min_len: 0,
        pattern: Some(pattern),
    }
}
fn int_min(min: f64) -> Rule {
    Rule::Int(Bounds {
        min: Some(min),
        ..Bounds::default()
    })
}
fn int_range(min: f64, max: f64) -> Rule {
    Rule::Int(Bounds {
        min: Some(min),
        max: Some(max),
        exclusive_min: None,
    })
}
fn pos_int() -> Rule {
    Rule::Int(Bounds {
        exclusive_min: Some(0.0),
        ..Bounds::default()
    })
}
fn num() -> Rule {
    Rule::Num(Bounds::default())
}
fn num_range(min: f64, max: f64) -> Rule {
    Rule::Num(Bounds {
        min: Some(min),
        max: Some(max),
        exclusive_min: None,
    })
}
fn array(rule: Rule) -> Rule {
    Rule::Array(Box::new(rule))
}
fn map(rule: Rule) -> Rule {
    Rule::Map(Box::new(rule))
}
fn obj(fields: Vec<Field>) -> Rule {
    Rule::Object(fields)
}

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

fn thinking() -> Rule {
    obj(vec![
        req("type", e(&["enabled", "disabled"])),
        f("budgetTokens", num()),
    ])
}

fn model_entry_fields() -> Vec<Field> {
    vec![
        req("model", s()),
        f("reasoning", s()),
        f("temperature", num_range(0.0, 2.0)),
        f("top_p", num_range(0.0, 1.0)),
        f("max_tokens", pos_int()),
        f("provider_options", map(Rule::Any)),
        f("variant", s()),
        f("reasoningEffort", s()),
        f("thinking", thinking()),
        f("textVerbosity", e(&["low", "medium", "high"])),
        f("maxTokens", num()),
        f("providerOptions", map(Rule::Any)),
    ]
}

/// A model reference: a string or a model object.
pub fn model_ref() -> Rule {
    Rule::ByType(vec![s(), obj(model_entry_fields())])
}

fn model_profiles() -> Rule {
    map(obj(vec![
        f("display_name", s()),
        f("family", e(&["daily", "geeky"])),
        f("tier", e(&["normal", "heavy"])),
        f("models", array(model_ref())),
    ]))
}

fn catalog() -> Rule {
    map(obj(vec![
        req("model", s()),
        f("reasoning", s()),
        f("variant", s()),
        f("reasoningEffort", s()),
    ]))
}

fn categories() -> Rule {
    map(obj(vec![
        f("description", s()),
        f("model", s()),
        f("models", array(model_ref())),
        f("reasoning", s()),
        f("temperature", num_range(0.0, 2.0)),
        f("top_p", num_range(0.0, 1.0)),
        f("max_tokens", pos_int()),
        f("provider_options", map(Rule::Any)),
        f(
            "fallback_models",
            Rule::ByType(vec![s(), array(model_ref())]),
        ),
        f("variant", s()),
        f("maxTokens", num()),
        f("thinking", thinking()),
        f("reasoningEffort", s()),
        f("textVerbosity", e(&["low", "medium", "high"])),
        f("tools", map(Rule::Bool)),
        f("prompt_append", s()),
        f("max_prompt_tokens", pos_int()),
        f("is_unstable_agent", Rule::Bool),
        f("disable", Rule::Bool),
        f("warn_unavailable", Rule::Bool),
    ]))
}

fn agents() -> Rule {
    let permission_value = e(&["ask", "allow", "deny"]);
    map(obj(vec![
        f("description", s()),
        f("prompt", s()),
        f("model", s()),
        f("models", array(model_ref())),
        f("reasoning", s()),
        f("variant", s()),
        f("reasoningEffort", s()),
        f("tools", map(Rule::Bool)),
        f("execution_mode", e(&["in-process", "process"])),
        f("background", Rule::Bool),
        f("max_depth", int_min(0.0)),
        f("allowed_subagents", array(s())),
        f("disallowed_tools", array(s())),
        f("max_turns", int_min(0.0)),
        f("temperature", num_range(0.0, 2.0)),
        f("disable", Rule::Bool),
        f(
            "permission",
            map(Rule::ByType(vec![
                permission_value.clone(),
                map(permission_value),
            ])),
        ),
        f("prompt_append", s()),
    ]))
}

fn task() -> Rule {
    obj(vec![
        d(
            "isolation",
            obj(vec![
                d("enabled", Rule::Bool, json!(false)),
                d(
                    "backend",
                    e(&[
                        "auto",
                        "apfs",
                        "btrfs",
                        "zfs",
                        "reflink",
                        "overlayfs",
                        "block-clone",
                        "rcopy",
                    ]),
                    json!("auto"),
                ),
                d("apply", Rule::Bool, json!(true)),
                d("merge", e(&["patch", "branch"]), json!("patch")),
                d("commits", e(&["generic", "ai"]), json!("generic")),
            ]),
            json!({}),
        ),
        d(
            "default_execution_mode",
            e(&["auto", "in-process", "process"]),
            json!("auto"),
        ),
        d(
            "process_runner",
            e(&["host", "child-process"]),
            json!("host"),
        ),
        d(
            "host_engine_policy",
            e(&["upgrade", "fallback"]),
            json!("upgrade"),
        ),
        f("host_idle_exit_ms", pos_int()),
        d(
            "host_shard_prewarm",
            e(&["off", "first-turn", "session-start"]),
            json!("first-turn"),
        ),
        d("default_concurrency", int_min(0.0), json!(5)),
        d("global_concurrency", int_min(0.0), json!(8)),
        f("provider_concurrency", map(int_min(0.0))),
        f("model_concurrency", map(int_min(0.0))),
        d("max_depth", int_min(0.0), json!(1)),
        d(
            "residency_max_children",
            Rule::ByType(vec![int_min(0.0), Rule::Const(json!("unlimited"))]),
            json!("unlimited"),
        ),
        d("ttl_ms", pos_int(), json!(86_400_000)),
        d(
            "resident_idle_timeout_ms",
            int_range(1.0, MAX_SAFE_INTEGER),
            json!(900_000),
        ),
        f("state_dir", s()),
        f("reattach_on_reconcile", Rule::Bool),
        d("resume_children", Rule::Bool, json!(true)),
        d(
            "warnings",
            obj(vec![d("unavailable_categories", Rule::Bool, json!(true))]),
            json!({}),
        ),
        d(
            "wait",
            obj(vec![
                d("min_ms", pos_int(), json!(5000)),
                d("default_ms", pos_int(), json!(60_000)),
                d("max_ms", pos_int(), json!(600_000)),
            ]),
            json!({}),
        ),
        d(
            "team",
            obj(vec![
                d("max_members", int_range(1.0, 8.0), json!(8)),
                d("max_parallel_members", int_range(1.0, 8.0), json!(4)),
                d("max_wall_clock_minutes", pos_int(), json!(120)),
            ]),
            json!({}),
        ),
        f(
            "dag",
            obj(vec![
                d("max_nodes_per_run", pos_int(), json!(64)),
                d("max_runs_per_session", pos_int(), json!(16)),
                d("subscriber_ring", pos_int(), json!(1000)),
                d("heartbeat_ms", pos_int(), json!(15_000)),
                d("history_default_limit", pos_int(), json!(256)),
                d("history_max_limit", pos_int(), json!(1000)),
                d("retention_days", pos_int(), json!(7)),
                d("max_prompt_bytes", pos_int(), json!(262_144)),
            ]),
        ),
    ])
}

fn teams() -> Rule {
    let name = || pat("^[a-z0-9-]+$");
    let common = || {
        vec![
            req("name", name()),
            f("cwd", s()),
            f("worktreePath", s()),
            f("subscriptions", array(s())),
            d(
                "backendType",
                e(&["in-process", "tmux"]),
                json!("in-process"),
            ),
            f("color", s()),
            d("isActive", Rule::Bool, json!(true)),
        ]
    };
    let mut category = common();
    category.extend([
        req("kind", Rule::Const(json!("category"))),
        req("category", s1()),
        req("prompt", s1()),
    ]);
    let mut subagent = common();
    subagent.extend([
        req("kind", Rule::Const(json!("subagent_type"))),
        req("subagent_type", s1()),
        f("prompt", s()),
    ]);
    map(obj(vec![
        d("version", Rule::Const(json!(1)), json!(1)),
        f("name", name()),
        f("description", s()),
        f("createdAt", int_min(1.0)),
        f("leadAgentId", s()),
        f("teamAllowedPaths", array(s())),
        f("sessionPermission", s()),
        req(
            "members",
            array(Rule::Tagged {
                tag: "kind",
                variants: vec![
                    ("category", obj(category)),
                    ("subagent_type", obj(subagent)),
                ],
            }),
        ),
    ]))
}

/// Memory settings; `with_defaults` is false for the per-agent overrides.
fn memory_fields(with_defaults: bool) -> Vec<Field> {
    let dd = |name: &'static str, rule: Rule, default: Value| {
        if with_defaults {
            d(name, rule, default)
        } else {
            f(name, rule)
        }
    };
    vec![
        dd("enabled", Rule::Bool, json!(true)),
        dd("agent", s1(), json!("auto")),
        dd(
            "projection",
            obj(vec![
                dd("max_entries_per_directory", int_min(0.0), json!(40)),
                dd("max_bytes", int_min(0.0), json!(24576)),
            ]),
            json!({}),
        ),
        dd(
            "reflection",
            obj(vec![
                dd("enabled", Rule::Bool, json!(true)),
                dd(
                    "trigger",
                    obj(vec![
                        dd("step_count", int_min(0.0), json!(25)),
                        dd("on_compaction", Rule::Bool, json!(true)),
                    ]),
                    json!({}),
                ),
                dd("merge", e(&["auto", "integration"]), json!("auto")),
                dd("category", s1(), json!("quick")),
                dd("timeout_minutes", pos_int(), json!(15)),
                dd("sandbox", e(&["auto", "required", "off"]), json!("auto")),
            ]),
            json!({}),
        ),
        dd(
            "nudge",
            obj(vec![
                dd("enabled", Rule::Bool, json!(true)),
                dd("every_user_turns", int_min(1.0), json!(10)),
            ]),
            json!({}),
        ),
        dd(
            "facts",
            obj(vec![
                dd("enabled", Rule::Bool, json!(true)),
                dd("debounce_settles", int_min(1.0), json!(4)),
                f("model", s1()),
            ]),
            json!({}),
        ),
        dd(
            "dream",
            obj(vec![
                dd("enabled", Rule::Bool, json!(true)),
                dd("idle_minutes", int_min(0.0), json!(30)),
                dd("min_hours_between", int_min(1.0), json!(24)),
                dd("shutdown_launch", Rule::Bool, json!(true)),
                dd("auto_select_max", int_range(1.0, 10.0), json!(5)),
                dd("auto_select_max_chars", int_min(10_000.0), json!(150_000)),
            ]),
            json!({}),
        ),
        dd(
            "people",
            obj(vec![
                dd("enabled", Rule::Bool, json!(false)),
                dd("max_entries", int_range(1.0, 100.0), json!(40)),
                dd("max_entry_chars", int_range(50.0, 500.0), json!(200)),
            ]),
            json!({}),
        ),
        dd(
            "agreements",
            obj(vec![
                dd("enabled", Rule::Bool, json!(true)),
                dd(
                    "needs_human_threshold",
                    int_range(1.0, MAX_SAFE_INTEGER),
                    json!(3),
                ),
                dd("max_items", int_range(8.0, 256.0), json!(64)),
            ]),
            json!({}),
        ),
        dd(
            "soul",
            obj(vec![dd("edit_notice", Rule::Bool, json!(true))]),
            json!({}),
        ),
        dd(
            "write_notice",
            obj(vec![dd("enabled", Rule::Bool, json!(true))]),
            json!({}),
        ),
        dd(
            "sync",
            obj(vec![
                f("remote", s1()),
                dd("enabled", Rule::Bool, json!(true)),
            ]),
            json!({}),
        ),
        dd(
            "search",
            obj(vec![dd("enabled", Rule::Bool, json!(true))]),
            json!({}),
        ),
        dd(
            "recall",
            obj(vec![
                dd("enabled", Rule::Bool, json!(true)),
                dd("max_items", int_range(1.0, 5.0), json!(2)),
                dd("category", s1(), json!("quick")),
                dd(
                    "event_caps",
                    obj(vec![
                        dd("tool_args", int_min(0.0), json!(400)),
                        dd("result_head", int_min(0.0), json!(600)),
                        dd("assistant", int_min(0.0), json!(1500)),
                        dd("prompt", int_min(0.0), json!(4000)),
                    ]),
                    json!({}),
                ),
                dd("sidecar_max_tokens", pos_int(), json!(48_000)),
                dd("max_concurrent_wakes", pos_int(), json!(2)),
                dd("tool_budget", pos_int(), json!(8)),
                dd("query_expansion", Rule::Bool, json!(false)),
            ]),
            json!({}),
        ),
        dd("compile_warn_tokens", pos_int(), json!(30_000)),
    ]
}

fn memory() -> Rule {
    let mut fields = memory_fields(true);
    fields.push(d("agents", map(obj(memory_fields(false))), json!({})));
    obj(fields)
}

fn computer() -> Rule {
    obj(vec![
        f("enabled", Rule::Bool),
        f("display", s1()),
        f("max_width", pos_int()),
        f("max_height", pos_int()),
        f("screenshot_max_bytes", pos_int()),
        f("stop_hotkey", s1()),
        f("allow_host_relay_only_stop", Rule::Bool),
        f("macos_canary", e(&["session", "off"])),
        f("audit_log", obj(vec![f("enabled", Rule::Bool)])),
        f(
            "screenshot_gc",
            obj(vec![
                f("enabled", Rule::Bool),
                f("stale_ms", int_min(0.0)),
                f("scan_interval_ms", int_min(0.0)),
            ]),
        ),
        f("engine_path", s1()),
        f("cua_adapter", Rule::Bool),
    ])
}

fn side_panel() -> Rule {
    obj(vec![
        d("enabled", Rule::Bool, json!(false)),
        d(
            "width",
            Rule::ByType(vec![int_range(24.0, 160.0), pat("^([1-4][0-9]|50)%$")]),
            json!("26%"),
        ),
        d("min_columns", int_range(60.0, 400.0), json!(120)),
        d("clickable", Rule::Bool, json!(true)),
        d("usage_poll_seconds", int_range(60.0, 3600.0), json!(150)),
        d(
            "sections",
            obj(vec![
                d("session", Rule::Bool, json!(true)),
                d("goal", Rule::Bool, json!(true)),
                d("context", Rule::Bool, json!(true)),
                d("usage", Rule::Bool, json!(false)),
                d("agents", Rule::Bool, json!(true)),
                d("tools", Rule::Bool, json!(true)),
                d("files", Rule::Bool, json!(true)),
                d("memory", Rule::Bool, json!(true)),
            ]),
            json!({}),
        ),
    ])
}

/// The ordinary sections; `gateway` only where allowed.
fn sections(include_gateway: bool) -> Vec<Field> {
    let mut out = vec![
        f("side_panel", side_panel()),
        f(
            "formatOnMutation",
            obj(vec![
                d(
                    "mode",
                    e(&["off", "best-effort", "required"]),
                    json!("best-effort"),
                ),
                f("languages", map(Rule::Bool)),
                d("maxFileBytes", pos_int(), json!(1_048_576)),
                d("timeoutMs", pos_int(), json!(3000)),
            ]),
        ),
        f("categories", categories()),
        f("agents", agents()),
        f(
            "git_master",
            obj(vec![
                d(
                    "commit_footer",
                    Rule::ByType(vec![Rule::Bool, s()]),
                    json!(false),
                ),
                d("include_co_authored_by", Rule::Bool, json!(false)),
                d("git_env_prefix", s(), json!("GIT_MASTER=1")),
            ]),
        ),
        f("task", task()),
        f("teams", teams()),
        f("models", catalog()),
        f("model_profiles", model_profiles()),
        f("model_profile", s()),
        f("memory", memory()),
        f(
            "telemetry",
            obj(vec![d("enabled", Rule::Bool, json!(false))]),
        ),
        f("computer", computer()),
        f("disabled_skills", array(s())),
        f(
            "mcp",
            obj(vec![
                d("builtins", Rule::Bool, json!(true)),
                f("disabled_builtins", array(s())),
            ]),
        ),
    ];
    if include_gateway {
        out.push(f("gateway", map(Rule::Any)));
    }
    out
}

fn typed_harness_block() -> Rule {
    obj(sections(true))
}

fn profile_body() -> Rule {
    let mut fields = sections(false);
    fields.extend([
        f("[native]", typed_harness_block()),
        f("[senpi]", typed_harness_block()),
        f("[codex]", typed_harness_block()),
        f("[opencode]", map(Rule::Any)),
    ]);
    obj(fields)
}

fn root() -> Rule {
    let mut fields = sections(true);
    fields.extend([
        f("$schema", s()),
        f("[native]", typed_harness_block()),
        f("[senpi]", typed_harness_block()),
        f("[codex]", typed_harness_block()),
        f("[opencode]", map(Rule::Any)),
        f("profiles", map(profile_body())),
        f("_migrations", array(s())),
        f("legacy_migrations", map(Rule::Any)),
    ]);
    obj(fields)
}

/// The document rule (built once).
pub fn document_rule() -> &'static Rule {
    static RULE: OnceLock<Rule> = OnceLock::new();
    RULE.get_or_init(root)
}

/// The base defaults of a view: empty `agents`/`categories`/`teams` and a materialised `task`.
pub fn base_defaults() -> Value {
    let mut value = json!({"agents": {}, "categories": {}, "teams": {}, "task": {}});
    let _ = validate(&mut value, Mode::Full);
    value
}

/// Validates `value` in `mode`, applying defaults in [`Mode::Full`]. Returns every issue.
pub fn validate(value: &mut Value, mode: Mode) -> Vec<Issue> {
    let mut issues = Vec::new();
    let mut path = Vec::new();
    check(value, document_rule(), mode, false, &mut path, &mut issues);
    issues
}

/// Validates against an arbitrary rule (used by tests and sub-documents).
pub fn validate_with(value: &mut Value, rule: &Rule, mode: Mode) -> Vec<Issue> {
    let mut issues = Vec::new();
    let mut path = Vec::new();
    check(value, rule, mode, false, &mut path, &mut issues);
    issues
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn invalid(issues: &mut Vec<Issue>, path: &[Seg], message: String) {
    issues.push(Issue {
        kind: IssueKind::Invalid,
        path: path.to_vec(),
        message,
    });
}

fn check_bounds(number: f64, bounds: &Bounds) -> Option<String> {
    if let Some(min) = bounds.min
        && number < min
    {
        return Some(format!("must be at least {min}"));
    }
    if let Some(max) = bounds.max
        && number > max
    {
        return Some(format!("must be at most {max}"));
    }
    if let Some(min) = bounds.exclusive_min
        && number <= min
    {
        return Some(format!("must be greater than {min}"));
    }
    None
}

fn pattern_regex(pattern: &'static str) -> Option<Regex> {
    Regex::new(pattern).ok()
}

fn matches_type(value: &Value, rule: &Rule) -> bool {
    match rule {
        Rule::Any => true,
        Rule::Bool => value.is_boolean(),
        Rule::Str { .. } => value.is_string(),
        Rule::Int(_) | Rule::Num(_) => value.is_number(),
        Rule::Const(expected) => std::mem::discriminant(expected) == std::mem::discriminant(value),
        Rule::Array(_) => value.is_array(),
        Rule::Map(_) | Rule::Object(_) | Rule::Tagged { .. } => value.is_object(),
        Rule::ByType(alternatives) => alternatives.iter().any(|alt| matches_type(value, alt)),
    }
}

fn expected_name(rule: &Rule) -> String {
    match rule {
        Rule::Any => "any value".into(),
        Rule::Bool => "boolean".into(),
        Rule::Str { .. } => "string".into(),
        Rule::Int(_) => "integer".into(),
        Rule::Num(_) => "number".into(),
        Rule::Const(value) => format!("the constant {value}"),
        Rule::Array(_) => "array".into(),
        Rule::Map(_) | Rule::Object(_) | Rule::Tagged { .. } => "object".into(),
        Rule::ByType(alternatives) => alternatives
            .iter()
            .map(expected_name)
            .collect::<Vec<_>>()
            .join(" or "),
    }
}

fn check(
    value: &mut Value,
    rule: &Rule,
    mode: Mode,
    in_list: bool,
    path: &mut Path,
    issues: &mut Vec<Issue>,
) {
    match rule {
        Rule::Any => {}
        Rule::Bool => {
            if !value.is_boolean() {
                invalid(
                    issues,
                    path,
                    format!("expected boolean, got {}", type_name(value)),
                );
            }
        }
        Rule::Str {
            allowed,
            min_len,
            pattern,
        } => {
            let Some(text) = value.as_str() else {
                invalid(
                    issues,
                    path,
                    format!("expected string, got {}", type_name(value)),
                );
                return;
            };
            if let Some(allowed) = allowed
                && !allowed.contains(&text)
            {
                invalid(
                    issues,
                    path,
                    format!("must be one of {}", allowed.join(", ")),
                );
            } else if text.chars().count() < *min_len {
                invalid(
                    issues,
                    path,
                    format!("must have at least {min_len} character(s)"),
                );
            } else if let Some(pattern) = pattern
                && !pattern_regex(pattern).is_some_and(|re| re.is_match(text))
            {
                invalid(issues, path, format!("must match {pattern}"));
            }
        }
        Rule::Int(bounds) | Rule::Num(bounds) => {
            let Some(number) = value.as_f64() else {
                invalid(
                    issues,
                    path,
                    format!("expected {}, got {}", expected_name(rule), type_name(value)),
                );
                return;
            };
            if matches!(rule, Rule::Int(_)) && number.fract() != 0.0 {
                invalid(issues, path, "expected integer".into());
            } else if let Some(message) = check_bounds(number, bounds) {
                invalid(issues, path, message);
            }
        }
        Rule::Const(expected) => {
            if value != expected {
                invalid(issues, path, format!("must be {expected}"));
            }
        }
        Rule::Array(inner) => {
            let Value::Array(items) = value else {
                invalid(
                    issues,
                    path,
                    format!("expected array, got {}", type_name(value)),
                );
                return;
            };
            for (index, item) in items.iter_mut().enumerate() {
                path.push(Seg::Index(index));
                check(item, inner, mode, true, path, issues);
                path.pop();
            }
        }
        Rule::Map(inner) => {
            let Value::Object(entries) = value else {
                invalid(
                    issues,
                    path,
                    format!("expected object, got {}", type_name(value)),
                );
                return;
            };
            for (key, item) in entries.iter_mut() {
                path.push(Seg::Key(key.clone()));
                check(item, inner, mode, in_list, path, issues);
                path.pop();
            }
        }
        Rule::Object(fields) => check_object(value, fields, mode, in_list, path, issues),
        Rule::ByType(alternatives) => {
            match alternatives.iter().find(|alt| matches_type(value, alt)) {
                Some(alt) => check(value, alt, mode, in_list, path, issues),
                None => invalid(
                    issues,
                    path,
                    format!("expected {}, got {}", expected_name(rule), type_name(value)),
                ),
            }
        }
        Rule::Tagged { tag, variants } => {
            let Some(object) = value.as_object() else {
                invalid(
                    issues,
                    path,
                    format!("expected object, got {}", type_name(value)),
                );
                return;
            };
            let names: Vec<&str> = variants.iter().map(|(name, _)| *name).collect();
            match object.get(*tag).and_then(Value::as_str) {
                Some(found) => match variants.iter().find(|(name, _)| *name == found) {
                    Some((_, variant)) => check(value, variant, mode, in_list, path, issues),
                    None => {
                        path.push(Seg::Key((*tag).into()));
                        invalid(issues, path, format!("must be one of {}", names.join(", ")));
                        path.pop();
                    }
                },
                None => {
                    path.push(Seg::Key((*tag).into()));
                    invalid(issues, path, format!("required ({})", names.join(" or ")));
                    path.pop();
                }
            }
        }
    }
}

fn check_object(
    value: &mut Value,
    fields: &[Field],
    mode: Mode,
    in_list: bool,
    path: &mut Path,
    issues: &mut Vec<Issue>,
) {
    let Value::Object(object) = value else {
        invalid(
            issues,
            path,
            format!("expected object, got {}", type_name(value)),
        );
        return;
    };
    for key in object.keys() {
        if !fields.iter().any(|field| field.name == key) {
            let mut unknown = path.clone();
            unknown.push(Seg::Key(key.clone()));
            issues.push(Issue {
                kind: IssueKind::UnknownKey,
                path: unknown,
                message: "unknown key".into(),
            });
        }
    }
    for field in fields {
        if mode == Mode::Full
            && !object.contains_key(field.name)
            && let Some(default) = &field.default
        {
            object.insert(field.name.into(), default.clone());
        }
        path.push(Seg::Key(field.name.into()));
        match object.get_mut(field.name) {
            Some(child) => check(child, &field.rule, mode, in_list, path, issues),
            None => {
                if field.required && (mode == Mode::Full || in_list) {
                    invalid(issues, path, "required".into());
                }
            }
        }
        path.pop();
    }
}

/// Removes keys named `__proto__`, `constructor` or `prototype` at every depth; returns their paths.
pub fn strip_unsafe_keys(value: &mut Value) -> Vec<Path> {
    let mut out = Vec::new();
    let mut path = Vec::new();
    strip_unsafe(value, &mut path, &mut out);
    out
}

/// Whether `key` is never allowed in a document.
pub fn is_unsafe_key(key: &str) -> bool {
    matches!(key, "__proto__" | "constructor" | "prototype")
}

fn strip_unsafe(value: &mut Value, path: &mut Path, out: &mut Vec<Path>) {
    match value {
        Value::Object(object) => {
            let unsafe_keys: Vec<String> = object
                .keys()
                .filter(|k| is_unsafe_key(k))
                .cloned()
                .collect();
            for key in unsafe_keys {
                object.shift_remove(&key);
                let mut removed = path.clone();
                removed.push(Seg::Key(key));
                out.push(removed);
            }
            for (key, child) in object.iter_mut() {
                path.push(Seg::Key(key.clone()));
                strip_unsafe(child, path, out);
                path.pop();
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter_mut().enumerate() {
                path.push(Seg::Index(index));
                strip_unsafe(child, path, out);
                path.pop();
            }
        }
        _ => {}
    }
}

/// Looks a path up.
pub fn get_path<'a>(value: &'a Value, path: &[Seg]) -> Option<&'a Value> {
    path.iter()
        .try_fold(value, |current, seg| match (current, seg) {
            (Value::Object(map), Seg::Key(key)) => map.get(key),
            (Value::Array(items), Seg::Index(index)) => items.get(*index),
            _ => None,
        })
}

/// Mutable lookup.
pub fn get_path_mut<'a>(value: &'a mut Value, path: &[Seg]) -> Option<&'a mut Value> {
    path.iter()
        .try_fold(value, |current, seg| match (current, seg) {
            (Value::Object(map), Seg::Key(key)) => map.get_mut(key),
            (Value::Array(items), Seg::Index(index)) => items.get_mut(*index),
            _ => None,
        })
}

/// Removes the value at `path` (non-root). Returns whether something was removed.
pub fn remove_path(value: &mut Value, path: &[Seg]) -> bool {
    let Some((last, parent)) = path.split_last() else {
        return false;
    };
    match (get_path_mut(value, parent), last) {
        (Some(Value::Object(map)), Seg::Key(key)) => map.shift_remove(key).is_some(),
        (Some(Value::Array(items)), Seg::Index(index)) if *index < items.len() => {
            items.remove(*index);
            true
        }
        _ => false,
    }
}

/// Whether the object at the root has no keys.
pub fn is_empty_object(value: &Value) -> bool {
    value.as_object().is_some_and(Map::is_empty)
}
