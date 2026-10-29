//! The public store-extension channel: registration, migration and op dispatch.
//!
//! Upstream counterpart: `thread/gateway/store-extension-ops.ts`. A compiled module is resolved from
//! the binary's registry by the `moduleUrl` a registration names, so no code is imported at call time.

use std::collections::BTreeMap;

use regex::Regex;
use serde_json::Value;

use super::extension_registrations::{declaration_problem, declared_op_names, import_extension_module, module_exports, persist_descriptor, read_descriptor, tool_name_taken};
use super::extension_transaction::{SenderProvenance, extension_transaction};
use super::schema::GatewaySchemaVersionError;
use super::store_extensions::{
    ExtensionModuleRegistry, StoreExtensionOperation, StoreExtensionRefusalCode, StoreExtensionRegistration, StoreExtensionResult,
};
use super::store_ops::{SqlAccess, StoreContext, StoreError, row_i64, transaction, v_i64, v_str};

fn namespace_pattern() -> &'static Regex {
    static PATTERN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^[a-z][a-z0-9_]{1,31}$").unwrap_or_else(|error| panic!("static pattern: {error}")))
}

#[derive(Clone)]
pub struct Registered {
    pub descriptor: StoreExtensionRegistration,
    pub module: BTreeMap<String, StoreExtensionOperation>,
}

fn refusal<T>(code: StoreExtensionRefusalCode, message: impl Into<String>) -> StoreExtensionResult<T> {
    StoreExtensionResult::refused(code, message)
}

fn from_error<T>(error: StoreError) -> StoreExtensionResult<T> {
    let message = error.message();
    if error.is_lock_wait_exceeded() {
        return refusal(StoreExtensionRefusalCode::GatewayLockWaitExceeded, message);
    }
    if matches!(error, StoreError::Schema(_)) {
        return refusal(StoreExtensionRefusalCode::GatewaySchemaTooNew, message);
    }
    refusal(StoreExtensionRefusalCode::ExtensionOperationFailed, message)
}

/// A namespace may not collide with a core table.
pub fn assert_extension_name(ctx: &StoreContext, name: &str) -> Result<(), StoreError> {
    let reserved = super::schema::GATEWAY_TABLES.contains(&name) || super::schema::GATEWAY_TABLES.iter().any(|table| table.starts_with(&format!("{name}_")));
    if reserved {
        return Err(StoreError::Message(format!("Namespace {name} is reserved by the gateway store.")));
    }
    let _ = ctx;
    Ok(())
}

pub struct StoreExtensions {
    registered: BTreeMap<String, Registered>,
}

impl Default for StoreExtensions {
    fn default() -> Self {
        Self::new()
    }
}

impl StoreExtensions {
    pub fn new() -> Self {
        Self { registered: BTreeMap::new() }
    }

    /// `restored_at`: a restarted worker replaying a registration this process made then.
    pub fn register(
        &mut self,
        ctx: &StoreContext,
        registry: &ExtensionModuleRegistry,
        descriptor: &StoreExtensionRegistration,
        now: i64,
        restored_at: Option<i64>,
    ) -> StoreExtensionResult<i64> {
        if !namespace_pattern().is_match(&descriptor.name) || descriptor.migrations.iter().any(|step| step.iter().any(|sql| sql.is_empty())) {
            return refusal(StoreExtensionRefusalCode::InvalidArguments, "An extension needs a valid namespace and an array of SQL migration steps.");
        }
        if let Some(problem) = declaration_problem(descriptor, None) {
            return refusal(StoreExtensionRefusalCode::InvalidArguments, format!("Extension {}: {problem}.", descriptor.name));
        }
        if let Err(error) = assert_extension_name(ctx, &descriptor.name) {
            return from_error(error);
        }
        let module = match import_extension_module(&descriptor.module_url, registry) {
            Ok(module) => module,
            Err(error) => return refusal(StoreExtensionRefusalCode::ExtensionImportFailed, format!("Cannot import extension {}: {error}", descriptor.name)),
        };
        if let Some(problem) = declaration_problem(descriptor, Some(&module_exports(&module))) {
            return refusal(StoreExtensionRefusalCode::InvalidArguments, format!("Extension {}: {problem}.", descriptor.name));
        }
        if let Some(taken) = tool_name_taken(ctx, descriptor) {
            return refusal(StoreExtensionRefusalCode::InvalidArguments, format!("Extension {}: {taken}.", descriptor.name));
        }
        let version = match self.ensure(ctx, descriptor, now) {
            StoreExtensionResult::Ok { value } => value,
            refused => {
                // Migration failures remain retryable on call; a downgrade must preserve the prior registration.
                if !matches!(&refused, StoreExtensionResult::Refused { code, .. } if *code == StoreExtensionRefusalCode::GatewaySchemaTooNew) {
                    self.registered.insert(descriptor.name.clone(), Registered { descriptor: descriptor.clone(), module });
                }
                return refused;
            }
        };
        let raced = match transaction(ctx, "extension_register", || persist_descriptor(ctx, descriptor, now, restored_at)) {
            Ok(raced) => raced,
            Err(error) => return from_error(error),
        };
        if let Some(raced) = raced {
            return refusal(StoreExtensionRefusalCode::InvalidArguments, format!("Extension {}: {raced}.", descriptor.name));
        }
        self.registered.insert(descriptor.name.clone(), Registered { descriptor: descriptor.clone(), module });
        StoreExtensionResult::ok(version)
    }

    /// Whether this exact registration is the one calls for its name now use.
    pub fn holds(&self, descriptor: &StoreExtensionRegistration) -> bool {
        self.registered.get(&descriptor.name).is_some_and(|registered| registered.descriptor == *descriptor)
    }

    fn ensure(&self, ctx: &StoreContext, descriptor: &StoreExtensionRegistration, now: i64) -> StoreExtensionResult<i64> {
        let name = &descriptor.name;
        let migrations = &descriptor.migrations;
        let current = ctx.one("SELECT version FROM extension_schema WHERE name = ?", &[v_str(name)]).ok().flatten();
        if let Some(current) = &current
            && row_i64(current, "version") == migrations.len() as i64
        {
            return StoreExtensionResult::ok(migrations.len() as i64);
        }
        loop {
            let step = transaction(ctx, "extension_migrate", || {
                let row = ctx.one("SELECT version FROM extension_schema WHERE name = ?", &[v_str(name)])?;
                let version = row.as_ref().map(|row| row_i64(row, "version")).unwrap_or(0);
                if version > migrations.len() as i64 {
                    return Err(GatewaySchemaVersionError::new(version, migrations.len() as i64, format!("Extension {name}")).into());
                }
                if version == migrations.len() as i64 {
                    if row.is_none() {
                        ctx.run("INSERT INTO extension_schema (name, version, updated_at) VALUES (?, 0, ?)", &[v_str(name), v_i64(now)])?;
                    }
                    return Ok((version, false));
                }
                for statement in &migrations[version as usize] {
                    ctx.exec(statement)?;
                }
                ctx.run(
                    "INSERT INTO extension_schema (name, version, updated_at) VALUES (?, ?, ?) ON CONFLICT(name) DO UPDATE SET version = excluded.version, updated_at = excluded.updated_at",
                    &[v_str(name), v_i64(version + 1), v_i64(now)],
                )?;
                Ok((version + 1, true))
            });
            match step {
                Ok((version, applied)) => {
                    if !applied {
                        return StoreExtensionResult::ok(version);
                    }
                }
                Err(error) => return from_error(error),
            }
        }
    }

    /// The public channel (connectors, the CLI, the SDK): a session-callable op is never reachable here.
    pub fn call(&self, ctx: &StoreContext, registry: &ExtensionModuleRegistry, name: &str, op: &str, args: &Value, now: i64) -> StoreExtensionResult<Value> {
        let own = self.registered.get(name);
        let persisted = read_descriptor(ctx, name).map(|descriptor| descriptor.named(name));
        if declared_op_names(own.map(|registered| &registered.descriptor)).contains(op) || declared_op_names(persisted.as_ref()).contains(op) {
            return refusal(
                StoreExtensionRefusalCode::CallerNotAllowed,
                format!("{name}.{op} is session-callable: only the session's own tool reaches it, with the caller the engine resolved."),
            );
        }
        let entry = match own {
            Some(registered) => registered.clone(),
            None => match self.load(ctx, registry, name) {
                Ok(registered) => registered,
                Err(refused) => return refused,
            },
        };
        self.run(ctx, &entry.descriptor, &entry.module, op, args.clone(), now)
    }

    /// An extension another process registered, from its persisted descriptor.
    pub fn load(&self, ctx: &StoreContext, registry: &ExtensionModuleRegistry, name: &str) -> Result<Registered, StoreExtensionResult<Value>> {
        let Some(persisted) = read_descriptor(ctx, name) else {
            return Err(refusal(StoreExtensionRefusalCode::ExtensionUnknownName, format!("No store extension is registered as {name}.")));
        };
        if let Err(error) = assert_extension_name(ctx, name) {
            return Err(from_error(error));
        }
        match import_extension_module(&persisted.module_url, registry) {
            Ok(module) => Ok(Registered { descriptor: persisted.named(name), module }),
            Err(error) => Err(refusal(StoreExtensionRefusalCode::ExtensionImportFailed, format!("Cannot import extension {name}: {error}"))),
        }
    }

    /// One op in its own transaction: the args are built inside it, and `effects` run only after COMMIT.
    pub fn run(
        &self,
        ctx: &StoreContext,
        descriptor: &StoreExtensionRegistration,
        module: &BTreeMap<String, StoreExtensionOperation>,
        op: &str,
        args: Value,
        now: i64,
    ) -> StoreExtensionResult<Value> {
        let Some(operation) = module.get(op) else {
            return refusal(StoreExtensionRefusalCode::ExtensionUnknownOp, format!("Extension {} exports no operation {op}.", descriptor.name));
        };
        let version = match self.ensure(ctx, descriptor, now) {
            StoreExtensionResult::Ok { value } => value,
            StoreExtensionResult::Refused { code, message } => return StoreExtensionResult::Refused { code, message },
        };
        let mut value: Option<Value> = None;
        let outcome = transaction(ctx, "extension_call", || {
            let row = ctx.one("SELECT version FROM extension_schema WHERE name = ?", &[v_str(&descriptor.name)])?;
            let locked = row.as_ref().map(|row| row_i64(row, "version")).unwrap_or(0);
            if locked != version {
                return Err(GatewaySchemaVersionError::new(locked, descriptor.migrations.len() as i64, format!("Extension {}", descriptor.name)).into());
            }
            let scope = extension_transaction(ctx, &descriptor.name, now, None::<SenderProvenance>);
            let result = operation(&scope.tx, args.clone());
            scope.finish();
            match result {
                Ok(result) => {
                    value = Some(result);
                    Ok(())
                }
                Err(error) => Err(StoreError::Message(format!("{}: {}", error.code.as_str(), error.message))),
            }
        });
        match outcome {
            Ok(()) => StoreExtensionResult::ok(value.unwrap_or(Value::Null)),
            Err(error) => from_error(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    fn context() -> StoreContext {
        let ctx = StoreContext::new(rusqlite::Connection::open_in_memory().expect("memory database"), StoreContext::test_config(), StoreContext::test_identity());
        for step in crate::thread::schema::GATEWAY_MIGRATIONS {
            for statement in *step {
                ctx.exec(statement).expect("migration step");
            }
        }
        ctx
    }

    fn registry(path: &str) -> ExtensionModuleRegistry {
        let mut module: BTreeMap<String, StoreExtensionOperation> = BTreeMap::new();
        module.insert(
            "core".to_owned(),
            Arc::new(|tx: &crate::thread::store_extensions::StoreExtensionTransaction, args: Value| {
                (tx.exec)("CREATE TABLE IF NOT EXISTS alpha_notes (id INTEGER PRIMARY KEY, note TEXT)", &[]).map_err(crate::thread::store_extensions::StoreExtensionError::failed)?;
                let note = args.get("note").and_then(Value::as_str).unwrap_or_default().to_owned();
                (tx.exec)("INSERT INTO alpha_notes (note) VALUES (?)", &[json!(note)]).map_err(crate::thread::store_extensions::StoreExtensionError::failed)?;
                Ok(json!({ "ok": true }))
            }),
        );
        module.insert(
            "session_open".to_owned(),
            Arc::new(|_tx: &crate::thread::store_extensions::StoreExtensionTransaction, _args: Value| Ok(json!({ "status": "opened" }))),
        );
        let mut registry = ExtensionModuleRegistry::new();
        registry.insert(path.to_owned(), module);
        registry
    }

    fn descriptor(path: &str) -> StoreExtensionRegistration {
        StoreExtensionRegistration {
            name: "alpha".into(),
            migrations: vec![vec!["CREATE TABLE IF NOT EXISTS alpha_notes (id INTEGER PRIMARY KEY, note TEXT)".to_owned()]],
            module_url: path.into(),
            session_callable: None,
            wake_dir: None,
        }
    }

    #[test]
    fn a_registration_runs_its_migration_and_persists_its_descriptor() {
        let ctx = context();
        let directory = tempfile::tempdir().expect("temp dir");
        let module = directory.path().join("alpha.mjs");
        std::fs::write(&module, b"// compiled\n").expect("module");
        let path = format!("file://{}", module.display());
        let mut extensions = StoreExtensions::new();
        let registry = registry(&path);
        assert_eq!(extensions.register(&ctx, &registry, &descriptor(&path), 1, None), StoreExtensionResult::ok(1));
        let persisted = read_descriptor(&ctx, "alpha").expect("persisted descriptor");
        assert_eq!(persisted.module_url, path);
        assert_eq!(extensions.call(&ctx, &registry, "alpha", "core", &json!({ "note": "hello" }), 2), StoreExtensionResult::ok(json!({ "ok": true })));
        let rows = ctx.all("SELECT note FROM alpha_notes", &[]).expect("rows");
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn an_unknown_module_or_op_is_refused_as_data() {
        let ctx = context();
        let directory = tempfile::tempdir().expect("temp dir");
        let module = directory.path().join("alpha.mjs");
        std::fs::write(&module, b"// compiled\n").expect("module");
        let path = format!("file://{}", module.display());
        let mut extensions = StoreExtensions::new();
        let registry = registry(&path);
        assert!(matches!(
            extensions.register(&ctx, &ExtensionModuleRegistry::new(), &descriptor(&path), 1, None),
            StoreExtensionResult::Refused { code: StoreExtensionRefusalCode::ExtensionImportFailed, .. }
        ));
        extensions.register(&ctx, &registry, &descriptor(&path), 1, None);
        assert!(matches!(
            extensions.call(&ctx, &registry, "alpha", "missing", &json!({}), 2),
            StoreExtensionResult::Refused { code: StoreExtensionRefusalCode::ExtensionUnknownOp, .. }
        ));
        assert!(matches!(
            extensions.call(&ctx, &registry, "beta", "core", &json!({}), 2),
            StoreExtensionResult::Refused { code: StoreExtensionRefusalCode::ExtensionUnknownName, .. }
        ));
    }

    #[test]
    fn a_session_callable_op_is_never_reachable_from_the_public_channel() {
        let ctx = context();
        let directory = tempfile::tempdir().expect("temp dir");
        let module = directory.path().join("alpha.mjs");
        std::fs::write(&module, b"// compiled\n").expect("module");
        let path = format!("file://{}", module.display());
        let registry = registry(&path);
        let mut extensions = StoreExtensions::new();
        let mut declared = descriptor(&path);
        declared.session_callable = Some(vec![crate::thread::store_extensions::SessionCallableOp {
            op: "session_open".into(),
            parameters: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            target_arg: None,
            internal: None,
            tool_name: Some("open".into()),
            description: Some("Open.".into()),
            await_spec: None,
            wake: None,
        }]);
        assert_eq!(extensions.register(&ctx, &registry, &declared, 1, None), StoreExtensionResult::ok(1));
        assert!(matches!(
            extensions.call(&ctx, &registry, "alpha", "session_open", &json!({}), 2),
            StoreExtensionResult::Refused { code: StoreExtensionRefusalCode::CallerNotAllowed, .. }
        ));
    }

    #[test]
    fn a_reserved_namespace_is_refused() {
        let ctx = context();
        assert!(assert_extension_name(&ctx, "deliveries").is_err());
        assert!(assert_extension_name(&ctx, "alpha").is_ok());
    }
}
