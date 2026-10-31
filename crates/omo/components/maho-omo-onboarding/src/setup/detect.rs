//! Sibling-harness detection (upstream `omo-native/bin/lib/setup-detect.js`): which agent harnesses
//! this machine has, which providers and credential types each holds (ids and types, never a value)
//! and how many API keys are importable.

use super::SetupRuntime;
use super::agent_dir::canonical_agent_dir;
use super::sorted_unique;
use super::sqlite_rows::{open_read_only, read_row, read_rows};
use serde_json::Value;
use std::path::{Path, PathBuf};
use yaml_lines::{enabled_model_line, model_role_line, section_header};

pub const KNOWN_AUTH_SCHEMA_VERSIONS: [i64; 2] = [4, 7];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Harness {
    pub id: String,
    pub installed: bool,
    pub providers: Vec<String>,
    pub credential_types: Vec<String>,
    pub entry_count: usize,
    pub importable_count: usize,
    pub model_hint: String,
    pub notices: Vec<String>,
}

/// `detectHarnesses`' result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inventory {
    pub harnesses: Vec<Harness>,
}

fn harness(id: &str, installed: bool, model_hint: &str) -> Harness {
    Harness {
        id: id.to_owned(),
        installed,
        providers: Vec::new(),
        credential_types: Vec::new(),
        entry_count: 0,
        importable_count: 0,
        model_hint: model_hint.to_owned(),
        notices: Vec::new(),
    }
}

fn read_auth_json(id: &str, path: &Path) -> Harness {
    if !path.exists() {
        return harness(id, false, "none");
    }
    let mut result = harness(id, true, "none");
    let parsed = std::fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|text| serde_json::from_str::<Value>(&text).map_err(|e| e.to_string()));
    match parsed {
        Ok(Value::Object(entries)) => {
            let mut providers = Vec::new();
            let mut types = Vec::new();
            for (provider, entry) in entries {
                providers.push(provider);
                let kind = entry
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned();
                if id == "opencode" && kind == "api" {
                    result.importable_count += 1;
                }
                types.push(kind);
            }
            result.providers = sorted_unique(providers);
            result.credential_types = sorted_unique(types);
            result.entry_count = result.providers.len();
        }
        Ok(_) => result
            .notices
            .push("could not parse auth.json: expected an object".into()),
        Err(error) => result
            .notices
            .push(format!("could not parse auth.json: {error}")),
    }
    result
}

fn read_senpi_models(path: &Path, result: &mut Harness) {
    if !path.exists() {
        return;
    }
    let parsed = std::fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|text| serde_json::from_str::<Value>(&text).map_err(|e| e.to_string()));
    match parsed {
        Ok(document) => {
            if let Some(providers) = document.get("providers").and_then(Value::as_object) {
                let ids = sorted_unique(providers.keys().cloned());
                result.model_hint = if ids.is_empty() {
                    "providers: none".into()
                } else {
                    format!("providers: {}", ids.join(", "))
                };
            }
        }
        Err(error) => result
            .notices
            .push(format!("could not parse models.json: {error}")),
    }
}

/// The three line shapes `readGajaeModels` matches with regular expressions.
mod yaml_lines {
    /// `/^enabledModels\s*:/` and `/^modelRoles\s*:/`.
    pub fn section_header(line: &str, name: &str) -> bool {
        line.strip_prefix(name)
            .is_some_and(|rest| rest.trim_start().starts_with(':'))
    }

    /// `/^[^\s#][^:]*\s*:/`: any other top-level key.
    pub fn top_level_key(line: &str) -> bool {
        let mut chars = line.chars();
        matches!(chars.next(), Some(c) if !c.is_whitespace() && c != '#') && line.contains(':')
    }

    /// `/^\s+-\s+(.+?)\s*$/` with surrounding quotes stripped.
    pub fn enabled_model_line(line: &str) -> Option<String> {
        if !line.starts_with(char::is_whitespace) {
            return None;
        }
        let rest = line.trim_start().strip_prefix('-')?;
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        let value = rest.trim();
        if value.is_empty() {
            return None;
        }
        let value = value.strip_prefix(['\'', '"']).unwrap_or(value);
        let value = value.strip_suffix(['\'', '"']).unwrap_or(value);
        Some(value.to_owned())
    }

    /// `/^\s+([^#][^:]*?)\s*:/`.
    pub fn model_role_line(line: &str) -> Option<String> {
        if !line.starts_with(char::is_whitespace) {
            return None;
        }
        let rest = line.trim_start();
        if rest.starts_with('#') {
            return None;
        }
        let (name, _) = rest.split_once(':')?;
        let name = name.trim();
        (!name.is_empty()).then(|| name.to_owned())
    }
}

fn read_gajae_models(path: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return "none".into();
    };
    let mut enabled = Vec::new();
    let mut roles = Vec::new();
    let mut section: Option<&str> = None;
    for line in text.lines() {
        if section_header(line, "enabledModels") {
            section = Some("enabledModels");
            continue;
        }
        if section_header(line, "modelRoles") {
            section = Some("modelRoles");
            continue;
        }
        if yaml_lines::top_level_key(line) {
            section = None;
            continue;
        }
        match section {
            Some("enabledModels") => enabled.extend(enabled_model_line(line)),
            Some("modelRoles") => roles.extend(model_role_line(line)),
            _ => {}
        }
    }
    let mut hints = Vec::new();
    if !enabled.is_empty() {
        hints.push(format!(
            "enabledModels: {}",
            sorted_unique(enabled).join(", ")
        ));
    }
    if !roles.is_empty() {
        hints.push(format!("modelRoles: {}", sorted_unique(roles).join(", ")));
    }
    if hints.is_empty() {
        "none".into()
    } else {
        hints.join("; ")
    }
}

fn inspect_sqlite(id: &str, path: &Path, model_hint: &str) -> Harness {
    let mut result = harness(id, path.exists(), model_hint);
    if !result.installed {
        return result;
    }
    let inspected = (|| -> Result<(), String> {
        let database = open_read_only(path)?;
        let version = read_row(
            &database,
            &["version"],
            "SELECT version FROM auth_schema_version",
        )?
        .and_then(|row| row.get("version").cloned());
        if !version
            .as_ref()
            .and_then(Value::as_i64)
            .is_some_and(|v| KNOWN_AUTH_SCHEMA_VERSIONS.contains(&v))
        {
            result.notices.push(format!(
                "auth schema version {} is unknown; credentials not inspected",
                super::js_string(version.as_ref())
            ));
            return Ok(());
        }
        let rows = read_rows(
            &database,
            &["provider", "credential_type", "disabled_cause"],
            "SELECT provider, credential_type, disabled_cause FROM auth_credentials",
        )?;
        let text = |row: &serde_json::Map<String, Value>, key: &str| {
            row.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        result.providers = sorted_unique(rows.iter().map(|row| text(row, "provider")));
        result.credential_types =
            sorted_unique(rows.iter().map(|row| text(row, "credential_type")));
        result.entry_count = rows.len();
        result.importable_count = rows
            .iter()
            .filter(|row| {
                row.get("disabled_cause").is_some_and(Value::is_null)
                    && text(row, "credential_type") == "api_key"
            })
            .count();
        Ok(())
    })();
    if let Err(error) = inspected {
        result
            .notices
            .push(format!("could not inspect agent.db: {error}"));
    }
    result
}

/// Every filesystem input detection reads, in a stable order.
pub fn detected_file_paths(runtime: &SetupRuntime) -> [PathBuf; 7] {
    let home = Path::new(&runtime.home);
    let agent_dir = canonical_agent_dir(&runtime.env, &runtime.home);
    let data_home = runtime
        .env_value("XDG_DATA_HOME")
        .map_or_else(|| home.join(".local").join("share"), PathBuf::from);
    [
        agent_dir.join("auth.json"),
        agent_dir.join("models.json"),
        data_home.join("opencode").join("auth.json"),
        home.join(".omp").join("agent").join("agent.db"),
        home.join(".omp").join("agent").join("models.db"),
        home.join(".gjc").join("agent").join("agent.db"),
        home.join(".gjc").join("agent").join("config.yml"),
    ]
}

/// The four harnesses: senpi (the engine's own store), opencode, oh-my-pi, gajae-code.
pub fn detect_harnesses(runtime: &SetupRuntime) -> Inventory {
    let [
        senpi_auth,
        senpi_models,
        opencode_auth,
        omp,
        omp_models,
        gjc,
        gjc_config,
    ] = detected_file_paths(runtime);
    let mut senpi = read_auth_json("senpi", &senpi_auth);
    read_senpi_models(&senpi_models, &mut senpi);
    let opencode = read_auth_json("opencode", &opencode_auth);
    let omp_hint = if omp_models.exists() {
        "models.db: yes"
    } else {
        "models.db: no"
    };
    let gjc_hint = read_gajae_models(&gjc_config);
    Inventory {
        harnesses: vec![
            senpi,
            opencode,
            inspect_sqlite("oh-my-pi", &omp, omp_hint),
            inspect_sqlite("gajae-code", &gjc, &gjc_hint),
        ],
    }
}

/// True when the engine holds no credentials but a sibling harness is installed.
pub fn needs_setup_suggestion(inventory: &Inventory) -> bool {
    let senpi = inventory.harnesses.iter().find(|h| h.id == "senpi");
    let sibling = inventory
        .harnesses
        .iter()
        .any(|h| h.id != "senpi" && h.installed);
    senpi.map_or(0, |h| h.entry_count) == 0 && sibling
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeMap;

    pub(crate) fn create_database(
        path: &Path,
        version: i64,
        rows: &[(&str, &str, &str, Option<&str>)],
    ) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let db = rusqlite::Connection::open(path).expect("open");
        db.execute_batch(&format!(
            "CREATE TABLE auth_schema_version (id INTEGER PRIMARY KEY, version INTEGER NOT NULL);
             INSERT INTO auth_schema_version VALUES (1, {version});
             CREATE TABLE auth_credentials (id INTEGER PRIMARY KEY, provider TEXT NOT NULL,
               credential_type TEXT NOT NULL, data TEXT NOT NULL, disabled_cause TEXT DEFAULT NULL);"
        ))
        .expect("schema");
        for (provider, kind, data, disabled) in rows {
            db.execute(
                "INSERT INTO auth_credentials (provider, credential_type, data, disabled_cause) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![provider, kind, data, disabled],
            )
            .expect("insert");
        }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    fn runtime(root: &Path) -> SetupRuntime {
        let mut env = BTreeMap::new();
        env.insert(
            "SENPI_CODING_AGENT_DIR".into(),
            root.join("senpi-agent").to_string_lossy().into_owned(),
        );
        env.insert(
            "XDG_DATA_HOME".into(),
            root.join("xdg").to_string_lossy().into_owned(),
        );
        SetupRuntime::new(root.join("home").to_string_lossy(), env)
    }

    /// Upstream "all four harness stores": ids and types, never values.
    #[test]
    fn all_four_stores_report_ids_and_types_but_no_values() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write(
            &root.join("senpi-agent/auth.json"),
            r#"{"senpi-beta":{"type":"oauth","access":"SENPI-SECRET"},"senpi-alpha":{"type":"api_key","key":"SENPI-SECRET"}}"#,
        );
        write(
            &root.join("senpi-agent/models.json"),
            r#"{"providers":{"senpi-model-b":{},"senpi-model-a":{}}}"#,
        );
        write(
            &root.join("xdg/opencode/auth.json"),
            r#"{"open-oauth":{"type":"oauth","access":"OPENCODE-SECRET"},"open-api":{"type":"api","key":"OPENCODE-SECRET"}}"#,
        );
        create_database(
            &root.join("home/.omp/agent/agent.db"),
            7,
            &[
                ("omp-api", "api_key", "SQLITE-SECRET", None),
                ("omp-disabled", "oauth", "SQLITE-SECRET", Some("expired")),
            ],
        );
        write(&root.join("home/.omp/agent/models.db"), "models-fixture");
        create_database(
            &root.join("home/.gjc/agent/agent.db"),
            4,
            &[("gjc-oauth", "oauth", "SQLITE-SECRET", None)],
        );
        write(
            &root.join("home/.gjc/agent/config.yml"),
            "modelRoles:\n  default: anthropic/fixture\n  fast: openai/fixture\nenabledModels:\n  - fixture-one\n  - fixture-two\ncycleOrder:\n  - default\n",
        );
        let inventory = detect_harnesses(&runtime(root));
        let row = |h: &Harness| {
            format!(
                "{} | {} | {} | {} | {}",
                h.id,
                if h.installed { "yes" } else { "no" },
                h.providers.join(", "),
                h.credential_types.join(", "),
                h.model_hint
            )
        };
        let rows: Vec<String> = inventory.harnesses.iter().map(row).collect();
        assert_eq!(
            rows,
            vec![
                "senpi | yes | senpi-alpha, senpi-beta | api_key, oauth | providers: senpi-model-a, senpi-model-b",
                "opencode | yes | open-api, open-oauth | api, oauth | none",
                "oh-my-pi | yes | omp-api, omp-disabled | api_key, oauth | models.db: yes",
                "gajae-code | yes | gjc-oauth | oauth | enabledModels: fixture-one, fixture-two; modelRoles: default, fast",
            ]
        );
        assert_eq!(inventory.harnesses[1].importable_count, 1);
        assert_eq!(inventory.harnesses[2].importable_count, 1);
        for secret in ["SENPI-SECRET", "OPENCODE-SECRET", "SQLITE-SECRET"] {
            assert!(!format!("{inventory:?}").contains(secret));
        }
        assert!(!needs_setup_suggestion(&inventory));
    }

    #[test]
    fn absent_stores_report_not_installed_and_unknown_schema_is_detection_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let inventory = detect_harnesses(&runtime(dir.path()));
        assert!(inventory.harnesses.iter().all(|h| !h.installed));
        assert!(!needs_setup_suggestion(&inventory));
        create_database(
            &dir.path().join("home/.omp/agent/agent.db"),
            99,
            &[("must-not-parse", "api_key", "x", None)],
        );
        let inventory = detect_harnesses(&runtime(dir.path()));
        let omp = &inventory.harnesses[2];
        assert!(omp.installed && omp.providers.is_empty() && omp.credential_types.is_empty());
        assert_eq!(
            omp.notices,
            vec!["auth schema version 99 is unknown; credentials not inspected"]
        );
        assert!(needs_setup_suggestion(&inventory));
    }
}
