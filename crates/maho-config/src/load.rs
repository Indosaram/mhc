//! Loading: read every layer, merge, resolve the view (spec §4–§7).

use crate::catalog::resolve_catalog_references;
use crate::diagnostics::{Diagnostic, DiagnosticKind, DiagnosticTarget, display_path};
use crate::env::{Env, account_home, home_dir_with, process_env, resolve_profile_name};
use crate::fs::{ConfigFs, RealFs};
use crate::merge::{deep_merge, native_overlay, without_control_keys};
use crate::paths::{Scope, resolve_paths};
use crate::schema::{Mode, base_defaults};
use crate::validate::{prune_until_valid, validate_layer};
use maho_support::parse_jsonc_lenient;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One candidate file and what happened to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRecord {
    pub path: PathBuf,
    pub scope: Scope,
    pub exists: bool,
    pub loaded: bool,
}

/// A raw canonicalised layer (control keys intact) with its source.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub source: SourceRecord,
    pub value: Value,
}

/// The result of a load. Loading never fails.
#[derive(Clone, Debug)]
pub struct LoadResult {
    pub view: Value,
    pub diagnostics: Vec<Diagnostic>,
    pub layers: Vec<Layer>,
    pub sources: Vec<SourceRecord>,
    pub profile: Option<String>,
    /// The home directory used for every path.
    pub home: PathBuf,
}

/// Which product reads the file. `senpi` is a legacy spelling of `native`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Harness {
    Native,
    Other(String),
}

impl Harness {
    pub fn parse(name: &str) -> Self {
        match name.trim() {
            "native" | "senpi" | "" => Harness::Native,
            other => Harness::Other(other.to_owned()),
        }
    }

    fn overlay(&self, container: &Value) -> Value {
        match self {
            Harness::Native => native_overlay(container),
            Harness::Other(name) => container
                .get(format!("[{name}]").as_str())
                .filter(|v| v.is_object())
                .map(without_control_keys)
                .unwrap_or_else(|| Value::Object(Map::new())),
        }
    }
}

/// Loader inputs.
#[derive(Clone)]
pub struct LoadOptions {
    pub cwd: PathBuf,
    pub env: Env,
    pub harness: Harness,
    pub profile: Option<String>,
    pub fs: Arc<dyn ConfigFs>,
    /// The operating-system account home (`None` to compute it).
    pub account_home: Option<PathBuf>,
    /// Resolve model catalog references in the view (§6.5).
    pub resolve_catalog: bool,
}

impl LoadOptions {
    /// Options for `cwd` with the process environment, harness `native`, the real filesystem.
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            env: process_env(),
            harness: Harness::Native,
            profile: None,
            fs: Arc::new(RealFs),
            account_home: account_home(),
            resolve_catalog: false,
        }
    }

    pub fn with_env(mut self, env: Env) -> Self {
        self.env = env;
        self
    }
    pub fn with_profile(mut self, profile: Option<String>) -> Self {
        self.profile = profile;
        self
    }
    pub fn with_fs(mut self, fs: Arc<dyn ConfigFs>) -> Self {
        self.fs = fs;
        self
    }
    pub fn with_account_home(mut self, account_home: Option<PathBuf>) -> Self {
        self.account_home = account_home;
        self
    }
    pub fn with_harness(mut self, harness: Harness) -> Self {
        self.harness = harness;
        self
    }
    pub fn with_catalog(mut self, resolve: bool) -> Self {
        self.resolve_catalog = resolve;
        self
    }
}

/// `load` with harness `native` and catalog resolution (§6.5).
pub fn load_native(cwd: &Path, env: &Env) -> LoadResult {
    load(
        &LoadOptions::new(cwd)
            .with_env(env.clone())
            .with_catalog(true),
    )
}

fn read_layer(
    path: &Path,
    scope: Scope,
    fs: &dyn ConfigFs,
    home: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) -> (SourceRecord, Option<Value>) {
    let mut record = SourceRecord {
        path: path.to_path_buf(),
        scope,
        exists: fs.exists(path),
        loaded: false,
    };
    if !record.exists {
        return (record, None);
    }
    let shown = display_path(path, home);
    let target = DiagnosticTarget::File(path.to_path_buf());
    let text = match fs.read(path) {
        Ok(text) => text,
        Err(error) => {
            diagnostics.push(Diagnostic::new(
                DiagnosticKind::Read,
                target,
                format!("{shown}: not loaded: cannot read the file ({error})"),
                Vec::new(),
            ));
            return (record, None);
        }
    };
    let parsed = parse_jsonc_lenient(&text);
    if !parsed.errors.is_empty() {
        let errors: Vec<String> = parsed.errors.iter().map(ToString::to_string).collect();
        diagnostics.push(Diagnostic::new(
            DiagnosticKind::Parse,
            target,
            format!(
                "{shown}: not loaded: JSONC syntax error(s): {}",
                errors.join("; ")
            ),
            Vec::new(),
        ));
        return (record, None);
    }
    let outcome = validate_layer(parsed.value, target, &shown);
    diagnostics.extend(outcome.diagnostics);
    record.loaded = outcome.value.is_some();
    (record, outcome.value)
}

/// Loads and resolves the configuration. Never fails; every problem becomes a diagnostic.
pub fn load(options: &LoadOptions) -> LoadResult {
    let fs = options.fs.as_ref();
    let home = home_dir_with(&options.env, &options.cwd);
    let mut diagnostics = Vec::new();
    let mut sources = Vec::new();
    let mut layers = Vec::new();
    for candidate in resolve_paths(
        &options.cwd,
        &options.env,
        fs,
        options.account_home.as_deref(),
    ) {
        let (record, value) = read_layer(
            &candidate.path,
            candidate.scope,
            fs,
            &home,
            &mut diagnostics,
        );
        sources.push(record.clone());
        if let Some(value) = value {
            layers.push(Layer {
                source: record,
                value,
            });
        }
    }
    let mut merged = Value::Object(Map::new());
    for layer in &layers {
        deep_merge(&mut merged, &layer.value);
    }

    let requested = resolve_profile_name(&options.env, options.profile.as_deref());
    let mut profile_body = None;
    let mut profile = None;
    if let Some(name) = requested {
        match merged.get("profiles").and_then(|p| p.get(&name)).filter(|v| v.is_object()) {
            Some(body) => {
                profile_body = Some(body.clone());
                profile = Some(name);
            }
            None => diagnostics.push(Diagnostic::new(
                DiagnosticKind::Profile,
                DiagnosticTarget::Config(format!("profiles.{name}")),
                format!("profile \"{name}\" is not defined (profiles.{name}); using the base configuration"),
                vec![format!("profiles.{name}")],
            )),
        }
    }

    let mut view = without_control_keys(&merged);
    deep_merge(&mut view, &options.harness.overlay(&merged));
    if let Some(body) = &profile_body {
        deep_merge(&mut view, &without_control_keys(body));
        deep_merge(&mut view, &options.harness.overlay(body));
    }
    let mut view = without_control_keys(&view);
    if let Value::Object(map) = &mut view {
        map.shift_remove("$schema");
    }

    let mut resolved = base_defaults();
    deep_merge(&mut resolved, &view);
    match prune_until_valid(&mut resolved, Mode::Full) {
        Ok(pruned) => {
            for item in pruned {
                diagnostics.push(Diagnostic::new(
                    DiagnosticKind::InvalidValue,
                    DiagnosticTarget::Merged,
                    format!(
                        "merged config: ignored invalid value at {} ({})",
                        item.key, item.message
                    ),
                    vec![item.key],
                ));
            }
        }
        Err(rejected) => {
            diagnostics.push(Diagnostic::new(
                DiagnosticKind::Validation,
                DiagnosticTarget::Merged,
                format!(
                    "merged config: reset to defaults: invalid value(s) at {}",
                    rejected.issues.join(", ")
                ),
                rejected.issues,
            ));
            resolved = base_defaults();
        }
    }
    let mut view = without_control_keys(&resolved);
    if options.resolve_catalog {
        let (catalog_view, catalog_diagnostics) = resolve_catalog_references(&view);
        view = catalog_view;
        diagnostics.extend(catalog_diagnostics);
    }
    LoadResult {
        view,
        diagnostics,
        layers,
        sources,
        profile,
        home,
    }
}
