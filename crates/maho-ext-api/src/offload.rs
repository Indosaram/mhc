//! Child-module registration, offload, held-wake, upload-redaction and subscription-limit contracts
//! shared by the extension API surface (senpi core/extensions/types.ts and tool-dispatch/offload.ts).
use crate::{ExtensionFailure, SourceInfo};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

/// Where an input entered the session: typed by a person, or sent by an extension.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputOrigin {
    Person,
    #[default]
    Extension,
}

/// Where a tool runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToolPlacement {
    #[default]
    Server,
    Child,
}

/// A file-backed module a child process loads to run a tool job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolChildModule {
    pub path: String,
    pub export: String,
}

/// The placement fields of an upstream `ToolDefinition` (`placement`, `serverPinned`, `childModule`, `childJob`), which the
/// dispatcher reads per call. maho's `ToolDefinition` carries `placement` / `server_pinned` / `child_job`; `child_module`
/// has no definition field, so the registrar supplies it.
#[derive(Clone, Default)]
pub struct ToolPlacementInfo {
    pub name: String,
    pub placement: ToolPlacement,
    pub server_pinned: bool,
    pub child_module: Option<ToolChildModule>,
    pub child_job: Option<maho_tools::definition::ToolChildJob>,
}

impl ToolPlacementInfo {
    /// Built from `definition.placement` / `server_pinned` / `child_job`.
    pub fn from_definition(definition: &maho_tools::definition::ToolDefinition) -> Self {
        Self {
            name: definition.name.clone(),
            placement: match definition.placement {
                Some(maho_tools::definition::ToolPlacement::Child) => ToolPlacement::Child,
                _ => ToolPlacement::Server,
            },
            server_pinned: definition.server_pinned == Some(true),
            child_module: None,
            child_job: definition.child_job.clone(),
        }
    }
}

/// One tool call handed to a dispatcher (senpi `ToolDispatchCall`): the definition's in-process body plus the placement
/// fields the dispatcher routes on.
pub struct ToolDispatchCall<'a> {
    pub definition: &'a maho_tools::definition::ToolDefinition,
    pub placement: ToolPlacementInfo,
    pub tool_call_id: String,
    pub params: serde_json::Value,
    pub signal: Option<crate::AbortSignal>,
    pub on_update: Option<maho_tools::definition::ToolUpdateCallback>,
    pub ctx: Option<&'a dyn maho_tools::definition::ToolContext>,
    /// The extension file that registered the tool; a relative `childModule.path` resolves against its directory.
    pub source_path: Option<String>,
}

/// What `ToolRunDispatch::run_tool` resolves to.
pub type ToolRunFuture<'a> = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<
                    maho_tools::definition::ToolResult,
                    Box<dyn std::error::Error + Send + Sync>,
                >,
            > + Send
            + 'a,
    >,
>;

/// The dispatcher as the extension host sees it (maho-core owns the implementation and sits above this crate).
pub trait ToolRunDispatch: Send + Sync {
    fn run_tool<'a>(&'a self, call: ToolDispatchCall<'a>) -> ToolRunFuture<'a>;
}

/// A file-backed child module an extension registers for `ctx.offload`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChildModuleRegistration {
    pub id: String,
    pub module: String,
    pub export: String,
    pub idempotent: bool,
}

/// A registration as stored: the registration plus the registering extension's source.
#[derive(Clone, Debug)]
pub struct RegisteredChildModule {
    pub registration: ChildModuleRegistration,
    pub source_info: SourceInfo,
}

impl RegisteredChildModule {
    /// The absolute module path: relative paths resolve against the registering extension's file.
    pub fn module_path(&self) -> PathBuf {
        let module = Path::new(&self.registration.module);
        if module.is_absolute() {
            return module.to_path_buf();
        }
        Path::new(&self.source_info.path)
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(module)
    }
}

/// Options for `ctx.offload`.
#[derive(Clone, Debug, Default)]
pub struct OffloadOptions {
    pub signal: Option<crate::AbortSignal>,
    pub deadline: Option<Duration>,
}

/// Stable failure codes an offload can carry as the message prefix.
pub const OFFLOAD_UNAVAILABLE: &str = "offload_unavailable";
pub const OFFLOAD_MODULE_UNAVAILABLE: &str = "offload_module_unavailable";
pub const OFFLOAD_BAD_OPTIONS: &str = "offload_bad_options";
pub const OFFLOAD_BAD_REGISTRATION: &str = "offload_bad_registration";
pub const TOOL_CHILD_DEADLINE: &str = "tool_child_deadline";

/// Refuses a registration that is not a file-backed module (`id`, `module`, `export` non-empty).
///
/// # Errors
/// `offload_bad_registration` when a required field is empty.
pub fn validate_child_module_registration(
    registration: &ChildModuleRegistration,
) -> Result<(), ExtensionFailure> {
    for (name, value) in [
        ("id", &registration.id),
        ("module", &registration.module),
        ("export", &registration.export),
    ] {
        if value.is_empty() {
            let hint = if name == "module" {
                " (a file path; inline functions are not accepted)"
            } else {
                ""
            };
            return Err(ExtensionFailure::new(format!(
                "{OFFLOAD_BAD_REGISTRATION}: registerChildModule needs a non-empty string {name}{hint}"
            )));
        }
    }
    Ok(())
}

/// Collects child modules across extensions in load order. The last extension to register an id
/// wins; each replacement is returned once as `Child module "<id>" from <new> replaces the one from <old>`.
pub fn merge_child_modules<'a>(
    extensions: impl IntoIterator<Item = (&'a str, &'a BTreeMap<String, RegisteredChildModule>)>,
    reported: &mut std::collections::BTreeSet<String>,
) -> (BTreeMap<String, RegisteredChildModule>, Vec<String>) {
    let mut modules: BTreeMap<String, (RegisteredChildModule, String)> = BTreeMap::new();
    let mut warnings = Vec::new();
    for (path, registered) in extensions {
        for (id, module) in registered {
            if let Some((_, previous)) = modules.get(id)
                && previous.as_str() != path
            {
                let message =
                    format!("Child module \"{id}\" from {path} replaces the one from {previous}");
                if reported.insert(message.clone()) {
                    warnings.push(message);
                }
            }
            modules.insert(id.clone(), (module.clone(), path.to_owned()));
        }
    }
    (
        modules
            .into_iter()
            .map(|(id, (module, _))| (id, module))
            .collect(),
        warnings,
    )
}

/// Text redacted for upload and how many values of each category were removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadRedaction {
    pub text: String,
    pub counts: BTreeMap<String, usize>,
}

/// One usage window of a subscription account.
#[derive(Clone, Debug, PartialEq)]
pub struct LimitWindow {
    /// Stable key within an account: `<limit>:<window>`.
    pub id: String,
    pub label: String,
    pub window_minutes: Option<u64>,
    /// 0-100.
    pub used_percent: f64,
    pub resets_at: Option<String>,
}

/// The latest limit windows one subscription account reported.
#[derive(Clone, Debug, PartialEq)]
pub struct AccountLimits {
    pub provider: String,
    pub account: String,
    pub windows: Vec<LimitWindow>,
    pub reported_at: String,
    pub plan: Option<String>,
    pub error: Option<String>,
}

pub type SubscriptionLimitsListener = Arc<dyn Fn(&AccountLimits) + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;

    fn registration(id: &str) -> ChildModuleRegistration {
        ChildModuleRegistration {
            id: id.into(),
            module: "worker.js".into(),
            export: "run".into(),
            idempotent: false,
        }
    }
    fn module(id: &str, base: &str) -> RegisteredChildModule {
        RegisteredChildModule {
            registration: registration(id),
            source_info: SourceInfo {
                path: base.into(),
                ..Default::default()
            },
        }
    }

    #[test]
    fn empty_registration_fields_are_refused() {
        assert!(validate_child_module_registration(&registration("a")).is_ok());
        let mut bad = registration("a");
        bad.module.clear();
        let error = validate_child_module_registration(&bad)
            .err()
            .map(|e| e.message);
        assert!(
            error
                .is_some_and(|m| m.starts_with("offload_bad_registration") && m.contains("module"))
        );
    }

    #[test]
    fn last_registration_wins_and_replacement_is_reported_once() {
        let first: BTreeMap<_, _> = [("job".to_owned(), module("job", "/a/ext.js"))].into();
        let second: BTreeMap<_, _> = [("job".to_owned(), module("job", "/b/ext.js"))].into();
        let mut reported = std::collections::BTreeSet::new();
        let (merged, warnings) = merge_child_modules(
            [("/a/ext.js", &first), ("/b/ext.js", &second)],
            &mut reported,
        );
        assert_eq!(merged["job"].source_info.path, "/b/ext.js");
        assert_eq!(
            warnings,
            vec![r#"Child module "job" from /b/ext.js replaces the one from /a/ext.js"#.to_owned()]
        );
        let (_, again) = merge_child_modules(
            [("/a/ext.js", &first), ("/b/ext.js", &second)],
            &mut reported,
        );
        assert!(again.is_empty());
    }

    #[test]
    fn relative_module_resolves_against_registering_file() {
        assert_eq!(
            module("job", "/a/dir/ext.js").module_path(),
            PathBuf::from("/a/dir/worker.js")
        );
    }
}
