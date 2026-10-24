//! `--tool-runtime in-process|child`: where a session's heavy tool work runs (senpi#3566).
//!
//! `in-process` runs every tool on the host's own event loop, as senpi always has. `child` moves heavy
//! tool work into one tool child per session. The flag is plumbing only until the child runtime lands;
//! the resolved value is advertised as `tool_runtime:<value>` in `get_protocol_info`.
//!
//! The kind and its capability helpers live in `maho_rpc::host_capabilities` (the RPC host advertises them and
//! maho-rpc sits below this crate); they are re-exported here under the upstream names.
use std::collections::HashMap;

pub use maho_rpc::host_capabilities::{
    OFFLOAD_API_CAPABILITY, TOOL_CHILD_PLACEMENTS, ToolRuntimeKind,
    is_host_owned_tool_runtime_capability, tool_runtime_capabilities, tool_runtime_capability,
};

const TOOL_RUNTIME_ENV_SUFFIX: &str = "TOOL_RUNTIME";

/// senpi `isToolRuntimeKind`.
pub fn is_tool_runtime_kind(value: &str) -> bool {
    ToolRuntimeKind::parse(value).is_some()
}

/// `resolveToolRuntime`'s outcome: the kind, or the startup error message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolRuntimeResolution {
    Ok(ToolRuntimeKind),
    Err(String),
}

/// Tool runtime of this process: `--tool-runtime` wins, then `SENPI_TOOL_RUNTIME`, then `in-process`.
/// An env value outside the two kinds is a startup error, the same as an unknown flag value: a typo
/// must never silently select the other runtime.
pub const TOOL_RUNTIME_UNSUPPORTED_HOST: &str = "tool_runtime_unsupported_host";

/// Hosts the child runtime cannot serve yet. Windows needs its own containment (Job Objects, the breakaway shim), which
/// lands separately (senpi#3566); until then `child` is refused there, so no half-gated launch path is ever reachable.
///
/// `has_ffi` is upstream's `bun:ffi` probe: the native Job Object calls (`windows-sys`) are always linked into a
/// Windows build, so the default probe answers true exactly on Windows.
pub fn tool_runtime_host_refusal(
    runtime: ToolRuntimeKind,
    platform: &str,
    has_ffi: impl FnOnce() -> bool,
) -> Option<String> {
    if runtime != ToolRuntimeKind::Child || platform != "win32" || has_ffi() {
        return None;
    }
    Some(format!(
        "{TOOL_RUNTIME_UNSUPPORTED_HOST}: --tool-runtime child on Windows needs the Bun runtime (bun:ffi holds the tool child in a Job Object); run senpi under Bun or use in-process"
    ))
}

/// senpi `ToolRuntimeUnsupportedHostError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRuntimeUnsupportedHostError {
    pub message: String,
}

impl ToolRuntimeUnsupportedHostError {
    pub const CODE: &'static str = TOOL_RUNTIME_UNSUPPORTED_HOST;
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
    pub fn code(&self) -> &'static str {
        Self::CODE
    }
}

impl std::fmt::Display for ToolRuntimeUnsupportedHostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ToolRuntimeUnsupportedHostError {}

/// The platform name upstream compares (`process.platform`).
pub fn process_platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else {
        std::env::consts::OS
    }
}

/// `resolveToolRuntime(parsed, process.env, process.platform)`.
pub fn resolve_tool_runtime(parsed: Option<ToolRuntimeKind>) -> ToolRuntimeResolution {
    resolve_tool_runtime_with(
        parsed,
        &maho_core::config::current_env(),
        process_platform(),
        || cfg!(windows),
    )
}

pub fn resolve_tool_runtime_with(
    parsed: Option<ToolRuntimeKind>,
    env: &HashMap<String, String>,
    platform: &str,
    has_ffi: impl FnOnce() -> bool,
) -> ToolRuntimeResolution {
    let resolved = match resolve_requested_tool_runtime(parsed, env) {
        ToolRuntimeResolution::Ok(value) => value,
        error => return error,
    };
    match tool_runtime_host_refusal(resolved, platform, has_ffi) {
        None => ToolRuntimeResolution::Ok(resolved),
        Some(message) => ToolRuntimeResolution::Err(message),
    }
}

fn resolve_requested_tool_runtime(
    parsed: Option<ToolRuntimeKind>,
    env: &HashMap<String, String>,
) -> ToolRuntimeResolution {
    if let Some(value) = parsed {
        return ToolRuntimeResolution::Ok(value);
    }
    let from_env = match maho_core::brand::env_value(TOOL_RUNTIME_ENV_SUFFIX, env) {
        None => return ToolRuntimeResolution::Ok(ToolRuntimeKind::InProcess),
        Some(value) if value.is_empty() => {
            return ToolRuntimeResolution::Ok(ToolRuntimeKind::InProcess);
        }
        Some(value) => value,
    };
    if let Some(kind) = ToolRuntimeKind::parse(&from_env) {
        return ToolRuntimeResolution::Ok(kind);
    }
    // Name the variable that actually carried the value: a legacy prefix is read too (as for every brand setting).
    let name = maho_core::brand::brand_env_names(
        TOOL_RUNTIME_ENV_SUFFIX,
        Some(&maho_core::brand::brand_profile()),
    )
    .into_iter()
    .find(|candidate| env.contains_key(candidate))
    .unwrap_or_else(|| TOOL_RUNTIME_ENV_SUFFIX.to_owned());
    ToolRuntimeResolution::Err(format!(
        "{name} (--tool-runtime) must be in-process or child, got {}",
        serde_json::Value::String(from_env)
    ))
}

/// The kind maho-core's dispatcher selects on (declared there because maho-core sits below this crate).
pub fn to_core_tool_runtime(
    runtime: ToolRuntimeKind,
) -> maho_core::tool_dispatch::tool_dispatcher::ToolRuntimeKind {
    match runtime {
        ToolRuntimeKind::InProcess => {
            maho_core::tool_dispatch::tool_dispatcher::ToolRuntimeKind::InProcess
        }
        ToolRuntimeKind::Child => maho_core::tool_dispatch::tool_dispatcher::ToolRuntimeKind::Child,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn refusal(resolution: &ToolRuntimeResolution) -> &str {
        match resolution {
            ToolRuntimeResolution::Err(message) => message,
            ToolRuntimeResolution::Ok(_) => "",
        }
    }

    // test/suite/tool-runtime-capabilities.test.ts
    #[test]
    fn advertises_the_tool_childs_placements_only_under_child_and_the_offload_api_in_both() {
        assert_eq!(
            tool_runtime_capabilities(ToolRuntimeKind::InProcess),
            vec!["tool_runtime:in-process", "offload_api"]
        );
        assert_eq!(
            tool_runtime_capabilities(ToolRuntimeKind::Child),
            vec![
                "tool_runtime:child",
                "placement:launch-barrier",
                "placement:job-policy",
                "placement:hostcall",
                "placement:offload",
                "placement:terminal",
                "offload_api",
            ]
        );
        assert!(is_host_owned_tool_runtime_capability("placement:x"));
        assert!(!is_host_owned_tool_runtime_capability("custom:keep"));
    }

    // test/suite/rpc-tool-runtime-flag.test.ts
    #[test]
    fn defaults_to_in_process_with_neither_the_flag_nor_the_env_set() {
        assert_eq!(
            resolve_tool_runtime_with(None, &env(&[]), "linux", || false),
            ToolRuntimeResolution::Ok(ToolRuntimeKind::InProcess)
        );
    }

    #[test]
    fn takes_the_flag_then_the_env_and_the_flag_beats_the_env() {
        assert_eq!(
            resolve_tool_runtime_with(Some(ToolRuntimeKind::Child), &env(&[]), "linux", || false),
            ToolRuntimeResolution::Ok(ToolRuntimeKind::Child)
        );
        assert_eq!(
            resolve_tool_runtime_with(
                None,
                &env(&[("SENPI_TOOL_RUNTIME", "child")]),
                "linux",
                || false
            ),
            ToolRuntimeResolution::Ok(ToolRuntimeKind::Child)
        );
        assert_eq!(
            resolve_tool_runtime_with(
                Some(ToolRuntimeKind::InProcess),
                &env(&[("SENPI_TOOL_RUNTIME", "child")]),
                "linux",
                || false
            ),
            ToolRuntimeResolution::Ok(ToolRuntimeKind::InProcess)
        );
        assert_eq!(
            resolve_tool_runtime_with(None, &env(&[("SENPI_TOOL_RUNTIME", "")]), "linux", || false),
            ToolRuntimeResolution::Ok(ToolRuntimeKind::InProcess)
        );
    }

    #[test]
    fn rejects_an_unknown_env_value_naming_the_variable_that_carried_it() {
        let from_env = resolve_tool_runtime_with(
            None,
            &env(&[("SENPI_TOOL_RUNTIME", "bogus")]),
            "linux",
            || false,
        );
        assert!(refusal(&from_env).starts_with("SENPI_TOOL_RUNTIME "));
        assert!(refusal(&from_env).contains("in-process or child"));
        assert!(refusal(&from_env).ends_with("got \"bogus\""));
        let from_legacy =
            resolve_tool_runtime_with(None, &env(&[("PI_TOOL_RUNTIME", "bogus")]), "linux", || {
                false
            });
        assert!(refusal(&from_legacy).starts_with("PI_TOOL_RUNTIME "));
    }

    // test/suite/tool-runtime-win32-refusal.test.ts
    #[test]
    fn child_on_win32_is_refused_without_ffi_from_the_flag_and_the_env_and_allowed_elsewhere() {
        let prefix = format!("{TOOL_RUNTIME_UNSUPPORTED_HOST}: ");
        let from_flag =
            resolve_tool_runtime_with(Some(ToolRuntimeKind::Child), &env(&[]), "win32", || false);
        assert!(refusal(&from_flag).starts_with(&prefix));
        let from_env = resolve_tool_runtime_with(
            None,
            &env(&[("SENPI_TOOL_RUNTIME", "child")]),
            "win32",
            || false,
        );
        assert!(refusal(&from_env).starts_with(&prefix));
        assert_eq!(
            resolve_tool_runtime_with(Some(ToolRuntimeKind::InProcess), &env(&[]), "win32", || {
                false
            }),
            ToolRuntimeResolution::Ok(ToolRuntimeKind::InProcess)
        );
        for platform in ["darwin", "linux"] {
            assert_eq!(
                resolve_tool_runtime_with(
                    Some(ToolRuntimeKind::Child),
                    &env(&[]),
                    platform,
                    || false
                ),
                ToolRuntimeResolution::Ok(ToolRuntimeKind::Child)
            );
        }
    }

    #[test]
    fn allows_child_on_a_win32_host_with_ffi_and_names_the_way_out_without() {
        assert_eq!(
            resolve_tool_runtime_with(Some(ToolRuntimeKind::Child), &env(&[]), "win32", || true),
            ToolRuntimeResolution::Ok(ToolRuntimeKind::Child)
        );
        assert!(tool_runtime_host_refusal(ToolRuntimeKind::Child, "win32", || true).is_none());
        let message = tool_runtime_host_refusal(ToolRuntimeKind::Child, "win32", || false);
        assert!(message.as_deref().is_some_and(|message| {
            message.starts_with("tool_runtime_unsupported_host: ")
                && message.contains("use in-process")
        }));
        assert!(tool_runtime_host_refusal(ToolRuntimeKind::InProcess, "win32", || false).is_none());
    }

    #[test]
    fn the_unsupported_host_error_carries_its_code() {
        let error = ToolRuntimeUnsupportedHostError::new("x");
        assert_eq!(error.code(), "tool_runtime_unsupported_host");
        assert_eq!(error.to_string(), "x");
    }

    // test/suite/rpc-tool-runtime-flag.test.ts: the `--tool-runtime` flag in `parseArgs`.
    fn parse(values: &[&str]) -> super::super::args::Args {
        let argv = values
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>();
        super::super::args::parse_args(&argv, false).unwrap_or_default()
    }

    #[test]
    fn the_flag_selects_a_kind_and_an_unknown_value_is_an_error_diagnostic() {
        assert_eq!(
            parse(&["--tool-runtime", "child"]).tool_runtime,
            Some(ToolRuntimeKind::Child)
        );
        assert_eq!(
            parse(&["--tool-runtime", "in-process"]).tool_runtime,
            Some(ToolRuntimeKind::InProcess)
        );
        assert_eq!(parse(&[]).tool_runtime, None);
        let bogus = parse(&["--tool-runtime", "bogus"]);
        assert_eq!(bogus.tool_runtime, None);
        assert!(
            bogus
                .diagnostics
                .iter()
                .any(|d| d.error && d.message == "--tool-runtime must be in-process or child")
        );
        let flag_follows = parse(&["--tool-runtime", "--help"]);
        assert!(flag_follows.help);
        assert!(flag_follows.diagnostics.iter().any(|d| d.error));
    }
}
