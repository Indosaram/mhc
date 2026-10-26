//! The persisted half of store extension registrations.
//!
//! Upstream counterpart: `thread/gateway/extension-registrations.ts`: the descriptor `register()`
//! upserts by name, the session-op declaration rules both `register()` and every reader apply, and
//! the worker's lazy import of a persisted module.

use std::collections::BTreeSet;
use std::path::Path;

use regex::Regex;
use serde_json::Value;

use super::session_args_schema::schema_problem;
use super::store_extensions::{DeclaredSessionOp, ExtensionModuleRegistry, PersistedDescriptor, SessionCallableOp, StoreExtensionOperation, StoreExtensionRegistration};
use super::store_ops::{SqlAccess, StoreError, row_str, v_str};

pub const RESERVED_CALLER_KEYS: [&str; 2] = ["caller_session_durable_id", "caller_created_target"];
pub const AWAIT_TIMEOUT_MAX_MS: i64 = 120_000;
/// The longest composed tool name a provider takes as a function name.
pub const REGISTERED_TOOL_NAME_MAX: usize = 64;

fn await_request_id_pattern() -> &'static Regex {
    static PATTERN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^tor_[a-f0-9]{32}$").unwrap_or_else(|error| panic!("static pattern: {error}")))
}

fn tool_name_pattern() -> &'static Regex {
    static PATTERN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^[a-z][a-z0-9_]{1,40}$").unwrap_or_else(|error| panic!("static pattern: {error}")))
}

fn wake_dir_pattern() -> &'static Regex {
    static PATTERN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^[a-z0-9][a-z0-9_-]*$").unwrap_or_else(|error| panic!("static pattern: {error}")))
}

fn wake_file_pattern() -> &'static Regex {
    static PATTERN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]*$").unwrap_or_else(|error| panic!("static pattern: {error}")))
}

pub fn is_await_request_id(value: &str) -> bool {
    await_request_id_pattern().is_match(value)
}

/// The name a declared op's tool is registered under: its own namespace, so no declaration can take
/// a core or omo tool's name (none of those starts with `ext_`).
pub fn registered_tool_name(extension: &str, tool_name: &str) -> String {
    format!("ext_{extension}_{tool_name}")
}

fn non_empty(value: Option<&str>) -> bool {
    value.is_some_and(|text| !text.is_empty())
}

fn internal_entry_problem(entry: &SessionCallableOp) -> Option<String> {
    let op = &entry.op;
    if entry.internal != Some(true) {
        return Some(format!("{op}: internal must be true when present"));
    }
    for field in ["toolName", "await", "wake"] {
        let present = match field {
            "toolName" => entry.tool_name.is_some(),
            "await" => entry.await_spec.is_some(),
            _ => entry.wake.is_some(),
        };
        if present {
            return Some(format!("{op}: an internal op registers no tool, so it cannot declare {field}"));
        }
    }
    if entry.description.as_deref().is_some_and(str::is_empty) {
        return Some(format!("{op}: description must be a non-empty string when present"));
    }
    None
}

fn entry_problem(entry: &SessionCallableOp, extension: &str, wake_dir: Option<&str>) -> Option<String> {
    if entry.op.is_empty() {
        return Some("op must be a non-empty string".to_owned());
    }
    let internal = entry.internal.is_some();
    if internal {
        if let Some(problem) = internal_entry_problem(entry) {
            return Some(problem);
        }
    } else {
        let tool_name = entry.tool_name.clone().unwrap_or_default();
        if !tool_name_pattern().is_match(&tool_name) {
            return Some(format!("toolName {tool_name} must match {}", tool_name_pattern().as_str()));
        }
        let composed = registered_tool_name(extension, &tool_name);
        if composed.chars().count() > REGISTERED_TOOL_NAME_MAX {
            return Some(format!("{composed} is longer than {REGISTERED_TOOL_NAME_MAX} characters"));
        }
        if !non_empty(entry.description.as_deref()) {
            return Some(format!("{tool_name}: description must be a non-empty string"));
        }
    }
    let label = if internal { entry.op.clone() } else { entry.tool_name.clone().unwrap_or_default() };
    let parameters = &entry.parameters;
    if parameters.get("type").and_then(Value::as_str) != Some("object") || parameters.get("additionalProperties").and_then(Value::as_bool) != Some(false) {
        return Some(format!("{label}: parameters must be an object schema with additionalProperties: false"));
    }
    let properties = match parameters.get("properties") {
        None => &Value::Object(Default::default()),
        Some(value) if value.is_object() => value,
        Some(_) => return Some(format!("{label}: parameters.properties must be an object")),
    };
    let reserved = RESERVED_CALLER_KEYS.iter().find(|key| properties.get(**key).is_some());
    if let Some(_key) = reserved {
        return Some(format!("{label}: parameters must not declare {}; the store stamps them", RESERVED_CALLER_KEYS.join(" or ")));
    }
    if let Some(problem) = schema_problem(parameters, "parameters") {
        return Some(format!("{label}: {problem}"));
    }
    if let Some(target_arg) = &entry.target_arg
        && properties.get(target_arg.as_str()).is_none()
    {
        return Some(format!("{label}: targetArg {target_arg} must name a field of parameters.properties"));
    }
    if let Some(awaited) = &entry.await_spec {
        if awaited.status_op.is_empty() || awaited.expire_op.is_empty() {
            return Some(format!("{label}: await needs statusOp and expireOp"));
        }
        if awaited.timeout_ms < 1 || awaited.timeout_ms > AWAIT_TIMEOUT_MAX_MS {
            return Some(format!("{label}: await.timeoutMs must be an integer from 1 to {AWAIT_TIMEOUT_MAX_MS}"));
        }
        if wake_dir.is_none() {
            return Some(format!("{label}: await needs the registration's wakeDir"));
        }
    }
    if let Some(wake) = &entry.wake {
        if !wake_file_pattern().is_match(wake) {
            return Some(format!("{label}: wake must be a plain file name matching {}", wake_file_pattern().as_str()));
        }
        if wake_dir.is_none() {
            return Some(format!("{label}: wake needs the registration's wakeDir"));
        }
    }
    None
}

/// Why a declaration is invalid, or none; `exports` (the compiled module's op names) also checks
/// every named op exists.
pub fn declaration_problem(descriptor: &StoreExtensionRegistration, exports: Option<&BTreeSet<String>>) -> Option<String> {
    let name = &descriptor.name;
    if let Some(wake_dir) = &descriptor.wake_dir
        && !wake_dir_pattern().is_match(wake_dir)
    {
        return Some(format!("wakeDir must be one path segment matching {}", wake_dir_pattern().as_str()));
    }
    let Some(entries) = &descriptor.session_callable else {
        return None;
    };
    let mut names: BTreeSet<String> = BTreeSet::new();
    // The worker resolves a session call by `op`, so a second entry naming the same op would never apply.
    let mut ops: BTreeSet<String> = BTreeSet::new();
    for entry in entries {
        if let Some(problem) = entry_problem(entry, name, descriptor.wake_dir.as_deref()) {
            return Some(problem);
        }
        if entry.is_tool() {
            let tool_name = entry.tool_name.clone().unwrap_or_default();
            if !names.insert(tool_name.clone()) {
                return Some(format!("toolName {tool_name} is declared twice"));
            }
        }
        if !ops.insert(entry.op.clone()) {
            return Some(format!("op {} is declared twice", entry.op));
        }
        let Some(exports) = exports else {
            continue;
        };
        let awaited = entry.await_spec.as_ref().filter(|_| entry.is_tool());
        let mut exported_names = vec![entry.op.clone()];
        if let Some(awaited) = awaited {
            exported_names.push(awaited.status_op.clone());
            exported_names.push(awaited.expire_op.clone());
        }
        for exported in exported_names {
            if !exports.contains(&exported) {
                let label = if entry.is_tool() { entry.tool_name.clone().unwrap_or_default() } else { entry.op.clone() };
                return Some(format!("{label}: the module exports no operation {exported}"));
            }
        }
    }
    None
}

/// Every op name a declaration reaches (op, statusOp, expireOp): the public channel refuses each.
pub fn declared_op_names(descriptor: Option<&StoreExtensionRegistration>) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let Some(entries) = descriptor.and_then(|descriptor| descriptor.session_callable.as_ref()) else {
        return names;
    };
    for entry in entries {
        names.insert(entry.op.clone());
        if let Some(awaited) = entry.await_spec.as_ref().filter(|_| entry.is_tool()) {
            names.insert(awaited.status_op.clone());
            names.insert(awaited.expire_op.clone());
        }
    }
    names
}

fn parse_descriptor(name: &str, json: &str) -> Option<PersistedDescriptor> {
    let value: Value = serde_json::from_str(json).ok()?;
    let descriptor: PersistedDescriptor = serde_json::from_value(value).ok()?;
    let registration = descriptor.named(name);
    declaration_problem(&registration, None).is_none().then_some(descriptor)
}

/// The persisted descriptor of one extension; a row that no longer passes the declaration rules reads as none.
pub fn read_descriptor(sql: &dyn SqlAccess, name: &str) -> Option<PersistedDescriptor> {
    let row = sql.one("SELECT descriptor_json FROM extension_registrations WHERE name = ?", &[v_str(name)]).ok().flatten()?;
    parse_descriptor(name, &row_str(&row, "descriptor_json"))
}

/// Every declared tool op with its registered tool name, in extension-name order; one read, no import.
pub fn list_session_ops(sql: &dyn SqlAccess) -> Vec<DeclaredSessionOp> {
    let rows = sql.all("SELECT name, descriptor_json FROM extension_registrations ORDER BY name", &[]).unwrap_or_default();
    rows.iter()
        .flat_map(|row| {
            let extension = row_str(row, "name");
            let descriptor = parse_descriptor(&extension, &row_str(row, "descriptor_json"));
            descriptor
                .and_then(|descriptor| descriptor.session_callable)
                .unwrap_or_default()
                .into_iter()
                .filter(SessionCallableOp::is_tool)
                .map(|entry| {
                    let registered_name = registered_tool_name(&extension, entry.tool_name.as_deref().unwrap_or_default());
                    DeclaredSessionOp { entry, extension: extension.clone(), registered_name }
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Why the registration's tools cannot take their names: another extension already composes one of them.
pub fn tool_name_taken(sql: &dyn SqlAccess, descriptor: &StoreExtensionRegistration) -> Option<String> {
    let mine: BTreeSet<String> = descriptor
        .session_callable
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|entry| entry.is_tool())
        .map(|entry| registered_tool_name(&descriptor.name, entry.tool_name.as_deref().unwrap_or_default()))
        .collect();
    list_session_ops(sql)
        .into_iter()
        .find(|entry| entry.extension != descriptor.name && mine.contains(&entry.registered_name))
        .map(|entry| format!("tool {} is already declared by extension {}", entry.registered_name, entry.extension))
}

/// Upserts the descriptor (newest registration wins); refuses a tool name another extension already
/// composes. `restored_at` marks a worker restart replaying a registration made at that time.
pub fn persist_descriptor(sql: &dyn SqlAccess, descriptor: &StoreExtensionRegistration, now: i64, restored_at: Option<i64>) -> Result<Option<String>, StoreError> {
    if let Some(taken) = tool_name_taken(sql, descriptor) {
        return Ok(Some(taken));
    }
    let persisted = PersistedDescriptor::of(descriptor);
    let json = serde_json::to_string(&persisted).unwrap_or_else(|_| "{}".to_owned());
    let newer_only = if restored_at.is_some() { " WHERE excluded.updated_at >= extension_registrations.updated_at" } else { "" };
    sql.run(
        &format!(
            "INSERT INTO extension_registrations (name, descriptor_json, updated_at) VALUES (?, ?, ?) ON CONFLICT(name) DO UPDATE SET descriptor_json = excluded.descriptor_json, updated_at = excluded.updated_at{newer_only}"
        ),
        &[v_str(&descriptor.name), v_str(json), super::store_ops::v_i64(restored_at.unwrap_or(now))],
    )?;
    Ok(None)
}

/// Imports a compiled extension module: a `file:` URL to .js/.mjs/.cjs only, resolved in the
/// binary's registry, and only while the file it names still exists.
pub fn import_extension_module(module_url: &str, registry: &ExtensionModuleRegistry) -> Result<std::collections::BTreeMap<String, StoreExtensionOperation>, String> {
    let Some(rest) = module_url.strip_prefix("file:") else {
        return Err("moduleUrl must name a compiled JavaScript file URL.".to_owned());
    };
    let path = rest.trim_start_matches("//");
    let extension = Path::new(path).extension().and_then(|value| value.to_str()).unwrap_or_default();
    if !matches!(extension, "js" | "mjs" | "cjs") {
        return Err("moduleUrl must name a compiled JavaScript file URL.".to_owned());
    }
    if !Path::new(path).exists() {
        return Err(format!("The extension module {module_url} does not exist."));
    }
    registry.get(module_url).cloned().ok_or_else(|| format!("The extension module {module_url} is not compiled into this binary."))
}

/// The op names a compiled module exports.
pub fn module_exports(module: &std::collections::BTreeMap<String, StoreExtensionOperation>) -> BTreeSet<String> {
    module.keys().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::store_ops::{SqlRow, row_opt_str};
    use serde_json::json;

    fn descriptor(name: &str, entries: Vec<SessionCallableOp>) -> StoreExtensionRegistration {
        StoreExtensionRegistration {
            name: name.into(),
            migrations: vec![],
            module_url: "file:///ext/alpha.mjs".into(),
            session_callable: Some(entries),
            wake_dir: Some("alpha".into()),
        }
    }

    fn tool_entry(tool_name: &str) -> SessionCallableOp {
        SessionCallableOp {
            op: format!("open_{tool_name}"),
            parameters: json!({ "type": "object", "properties": { "target": { "type": "string" } }, "additionalProperties": false }),
            target_arg: Some("target".into()),
            internal: None,
            tool_name: Some(tool_name.into()),
            description: Some("Open a request.".into()),
            await_spec: None,
            wake: None,
        }
    }

    #[test]
    fn a_well_formed_declaration_has_no_problem() {
        assert_eq!(declaration_problem(&descriptor("alpha", vec![tool_entry("open")]), None), None);
    }

    #[test]
    fn the_composed_tool_name_is_namespaced_and_bounded() {
        assert_eq!(registered_tool_name("alpha", "open"), "ext_alpha_open");
        let long_extension = "x".repeat(32);
        let long_tool = "t".repeat(30);
        assert!(declaration_problem(&descriptor(&long_extension, vec![tool_entry(&long_tool)]), None).is_some_and(|problem| problem.contains("is longer than")));
        assert!(declaration_problem(&descriptor("alpha", vec![tool_entry("Bad-Name")]), None).is_some_and(|problem| problem.contains("must match")));
    }

    #[test]
    fn reserved_caller_keys_and_unknown_keywords_are_refused() {
        let mut entry = tool_entry("open");
        entry.parameters = json!({ "type": "object", "properties": { "caller_session_durable_id": { "type": "string" } }, "additionalProperties": false });
        assert!(declaration_problem(&descriptor("alpha", vec![entry]), None).is_some_and(|problem| problem.contains("the store stamps them")));
        let mut entry = tool_entry("open");
        entry.parameters = json!({ "type": "object", "properties": {}, "additionalProperties": false, "oneOf": [] });
        assert!(declaration_problem(&descriptor("alpha", vec![entry]), None).is_some_and(|problem| problem.contains("which the store cannot check")));
    }

    #[test]
    fn duplicates_and_internal_misdeclarations_are_refused() {
        let mut second = tool_entry("open");
        second.op = "open_other".into();
        assert!(declaration_problem(&descriptor("alpha", vec![tool_entry("open"), second]), None).is_some_and(|problem| problem.contains("is declared twice")));
        let mut internal = tool_entry("open");
        internal.internal = Some(true);
        assert!(declaration_problem(&descriptor("alpha", vec![internal]), None).is_some_and(|problem| problem.contains("cannot declare toolName")));
    }

    #[test]
    fn an_await_needs_its_ops_a_bounded_timeout_and_a_wake_dir() {
        let mut entry = tool_entry("open");
        entry.await_spec = Some(super::super::store_extensions::AwaitSpec { status_op: "status".into(), expire_op: "expire".into(), timeout_ms: 1_000 });
        assert_eq!(declaration_problem(&descriptor("alpha", vec![entry.clone()]), None), None);
        entry.await_spec = Some(super::super::store_extensions::AwaitSpec { status_op: "status".into(), expire_op: "expire".into(), timeout_ms: AWAIT_TIMEOUT_MAX_MS + 1 });
        assert!(declaration_problem(&descriptor("alpha", vec![entry.clone()]), None).is_some_and(|problem| problem.contains("await.timeoutMs")));
        entry.await_spec.as_mut().expect("await spec").timeout_ms = 1_000;
        let mut without_wake_dir = descriptor("alpha", vec![entry]);
        without_wake_dir.wake_dir = None;
        assert!(declaration_problem(&without_wake_dir, None).is_some_and(|problem| problem.contains("wakeDir")));
    }

    #[test]
    fn declared_op_names_reach_the_await_ops() {
        let mut entry = tool_entry("open");
        entry.await_spec = Some(super::super::store_extensions::AwaitSpec { status_op: "status".into(), expire_op: "expire".into(), timeout_ms: 10 });
        let names = declared_op_names(Some(&descriptor("alpha", vec![entry])));
        assert_eq!(names.into_iter().collect::<Vec<_>>(), vec!["expire", "open_open", "status"]);
    }

    #[test]
    fn a_persisted_descriptor_is_listed_by_every_reader() {
        let ctx = crate::thread::store_ops::StoreContext::new(
            rusqlite::Connection::open_in_memory().expect("memory database"),
            crate::thread::store_ops::StoreContext::test_config(),
            crate::thread::store_ops::StoreContext::test_identity(),
        );
        crate::thread::store_ops::migrate(&ctx).expect("migrations");
        let registration = descriptor("alpha_beta", vec![tool_entry("open")]);
        assert_eq!(persist_descriptor(&ctx, &registration, 1, None).expect("persist"), None);
        let listed = list_session_ops(&ctx);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].registered_name, "ext_alpha_beta_open");
        assert_eq!(read_descriptor(&ctx, "alpha_beta").map(|descriptor| descriptor.module_url), Some("file:///ext/alpha.mjs".into()));
        assert!(tool_name_taken(&ctx, &descriptor("alpha", vec![tool_entry("beta_open")])).is_some_and(|taken| taken.contains("already declared")));
    }

    #[test]
    fn a_row_that_no_longer_passes_the_rules_reads_as_none() {
        let ctx = crate::thread::store_ops::StoreContext::new(
            rusqlite::Connection::open_in_memory().expect("memory database"),
            crate::thread::store_ops::StoreContext::test_config(),
            crate::thread::store_ops::StoreContext::test_identity(),
        );
        crate::thread::store_ops::migrate(&ctx).expect("migrations");
        let registration = descriptor("alpha", vec![tool_entry("open")]);
        assert_eq!(persist_descriptor(&ctx, &registration, 1, None).expect("persist"), None);
        assert!(read_descriptor(&ctx, "alpha").is_some());
        let mut invalid = PersistedDescriptor::of(&registration);
        invalid.session_callable.as_mut().expect("entries")[0].tool_name = Some("Bad-Name".into());
        ctx.run("UPDATE extension_registrations SET descriptor_json = ? WHERE name = 'alpha'", &[v_str(serde_json::to_string(&invalid).expect("descriptor json"))]).expect("update");
        assert!(read_descriptor(&ctx, "alpha").is_none());
        ctx.run("UPDATE extension_registrations SET descriptor_json = ? WHERE name = 'alpha'", &[v_str("not json")]).expect("update");
        assert!(read_descriptor(&ctx, "alpha").is_none());
    }

    #[test]
    fn an_uncompiled_or_missing_module_is_refused() {
        let registry = ExtensionModuleRegistry::new();
        assert!(import_extension_module("https://example.invalid/x.mjs", &registry).is_err());
        assert!(import_extension_module("file:///nowhere/x.mjs", &registry).is_err());
        assert!(import_extension_module("file:///tmp/x.txt", &registry).is_err());
    }

    #[test]
    fn row_helpers_read_persisted_columns() {
        let mut row = SqlRow::new();
        row.insert("name".into(), Value::from("alpha"));
        assert_eq!(row_str(&row, "name"), "alpha");
        assert_eq!(row_opt_str(&row, "missing"), None);
    }
}
