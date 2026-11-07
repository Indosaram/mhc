//! Port of senpi packages/coding-agent/src/core/project-trust.ts.
//!
//! senpi emits the project_trust extension event through core/extensions/runner.ts. In maho the
//! runner lives in maho-ext-host (which depends on maho-ext-api, not maho-core), so the event
//! emission is injected as a callback rather than imported.

use maho_ext_api::{ExtensionFailure, ProjectTrustEventResult, TrustDecision};

use crate::config::{app_name, config_dir_name};
use crate::trust_manager::{
    ProjectTrustOption, ProjectTrustStore, get_project_trust_options,
    has_trust_requiring_project_resources,
};

/// senpi declares DefaultProjectTrust in settings-manager.ts; the data-only shape is declared here
/// because that module is owned by todo 16.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DefaultProjectTrust {
    Always,
    Never,
    #[default]
    Ask,
}

impl DefaultProjectTrust {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "always" => Some(Self::Always),
            "never" => Some(Self::Never),
            "ask" => Some(Self::Ask),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Interactive,
    Print,
    Json,
    Rpc,
}

impl AppMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Print => "print",
            Self::Json => "json",
            Self::Rpc => "rpc",
        }
    }
}

/// The UI seam the interactive trust prompt uses.
pub trait ProjectTrustContext: Send + Sync {
    fn has_ui(&self) -> bool;
    fn select(&self, title: &str, options: &[String]) -> Option<String>;
}

pub fn format_project_trust_prompt(cwd: &str) -> String {
    format!(
        "Trust project folder?\n{cwd}\n\nThis allows {} to load {} settings and resources, install missing project packages, and execute project extensions.",
        app_name(),
        config_dir_name()
    )
}

/// senpi `presetTrustsProject` (senpi#3414): a session whose permission preset is explicitly
/// `full-access` trusts its project for that session. Explicit means the session's `permission-preset`
/// flag (CLI, or `open_session.permissionPreset` through the launch profile), else the user's global
/// setting. Project settings never count: they are what trust gates. The implicit default never
/// counts either, so a plain launch keeps asking.
pub fn preset_trusts_project(
    preset_flag: Option<&maho_ext_api::FlagValue>,
    global_preset: Option<&serde_json::Value>,
) -> bool {
    match preset_flag {
        Some(maho_ext_api::FlagValue::String(preset)) => preset == "full-access",
        Some(maho_ext_api::FlagValue::Boolean(_)) | None => {
            global_preset.and_then(serde_json::Value::as_str) == Some("full-access")
        }
    }
}

/// Result of asking the extension runner for a trust decision.
pub type EmitProjectTrust =
    dyn Fn(&str) -> Result<Option<ProjectTrustEventResult>, ExtensionFailure> + Send + Sync;

pub struct ResolveProjectTrustedOptions<'a> {
    pub cwd: &'a str,
    pub trust_store: &'a ProjectTrustStore,
    pub trust_override: Option<bool>,
    pub default_project_trust: Option<DefaultProjectTrust>,
    pub emit_project_trust: Option<&'a EmitProjectTrust>,
    pub project_trust_context: &'a dyn ProjectTrustContext,
    pub on_extension_error: Option<&'a (dyn Fn(&str) + Send + Sync)>,
    /// Receives the error of a trust store that cannot be read; the project is then untrusted.
    pub on_trust_store_error: Option<&'a (dyn Fn(&str) + Send + Sync)>,
}

fn select_project_trust_option(
    cwd: &str,
    ctx: &dyn ProjectTrustContext,
) -> Option<ProjectTrustOption> {
    let options = get_project_trust_options(cwd, true);
    let labels: Vec<String> = options.iter().map(|option| option.label.clone()).collect();
    let selected = ctx.select(&format_project_trust_prompt(cwd), &labels)?;
    options.into_iter().find(|option| option.label == selected)
}

fn save_project_trust_prompt_result(trust_store: &ProjectTrustStore, result: &ProjectTrustOption) {
    if !result.updates.is_empty() {
        let _ = trust_store.set_many(&result.updates);
    }
}

pub async fn resolve_project_trusted(options: ResolveProjectTrustedOptions<'_>) -> bool {
    if let Some(trust_override) = options.trust_override {
        return trust_override;
    }
    if !has_trust_requiring_project_resources(options.cwd, None) {
        return true;
    }

    if let Some(emit) = options.emit_project_trust {
        match emit(options.cwd) {
            Ok(Some(result)) => {
                let trusted = result.trusted == TrustDecision::Yes;
                if result.remember == Some(true) {
                    let _ = options.trust_store.set(options.cwd, Some(trusted));
                }
                return trusted;
            }
            Ok(None) => {}
            Err(error) => {
                if let Some(on_error) = options.on_extension_error {
                    on_error(&format!("Extension project_trust error: {error}"));
                }
            }
        }
    }

    // senpi `trustStore.get` throws on an unreadable or invalid trust.json: the project is not trusted, and
    // neither `defaultProjectTrust` nor the prompt may decide past it.
    match options.trust_store.get(options.cwd) {
        Ok(Some(decision)) => return decision,
        Ok(None) => {}
        Err(error) => {
            let message = format!(
                "Project trust store unreadable; not trusting {}: {error}",
                options.cwd
            );
            match options.on_trust_store_error {
                Some(report) => report(&message),
                None => {
                    let _ =
                        crate::output_guard::maho_write_stderr(&format!("Warning: {message}\n"));
                }
            }
            return false;
        }
    }

    match options.default_project_trust.unwrap_or_default() {
        DefaultProjectTrust::Always => return true,
        DefaultProjectTrust::Never => return false,
        DefaultProjectTrust::Ask => {}
    }

    if !options.project_trust_context.has_ui() {
        return false;
    }

    match select_project_trust_option(options.cwd, options.project_trust_context) {
        Some(selected) => {
            let trusted = selected.trusted;
            save_project_trust_prompt_result(options.trust_store, &selected);
            trusted
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust_manager::ProjectTrustStore;

    struct NoUi;

    impl ProjectTrustContext for NoUi {
        fn has_ui(&self) -> bool {
            false
        }
        fn select(&self, _title: &str, _options: &[String]) -> Option<String> {
            None
        }
    }

    struct YesUi;

    impl ProjectTrustContext for YesUi {
        fn has_ui(&self) -> bool {
            true
        }
        fn select(&self, _title: &str, options: &[String]) -> Option<String> {
            options.first().cloned()
        }
    }

    fn store() -> ProjectTrustStore {
        let dir = tempfile::tempdir().expect("tempdir");
        ProjectTrustStore::new(&dir.path().to_string_lossy())
    }

    /// senpi project-trust.ts `presetTrustsProject` (senpi#3414).
    #[test]
    fn only_an_explicit_full_access_preset_trusts_the_project() {
        let full = serde_json::json!("full-access");
        let ask = serde_json::json!("ask");
        let flag = |value: &str| maho_ext_api::FlagValue::String(value.to_owned());
        assert!(preset_trusts_project(Some(&flag("full-access")), None));
        // The session's own flag wins over the global setting, both ways.
        assert!(!preset_trusts_project(Some(&flag("ask")), Some(&full)));
        assert!(preset_trusts_project(
            Some(&flag("full-access")),
            Some(&ask)
        ));
        assert!(preset_trusts_project(None, Some(&full)));
        assert!(!preset_trusts_project(None, Some(&ask)));
        // The implicit default never counts.
        assert!(!preset_trusts_project(None, None));
        assert!(!preset_trusts_project(
            Some(&maho_ext_api::FlagValue::Boolean(true)),
            None
        ));
    }

    #[test]
    fn the_prompt_names_the_product_and_config_dir() {
        let prompt = format_project_trust_prompt("/work");
        assert!(prompt.contains("/work"));
        assert!(prompt.contains("maho"));
        assert!(prompt.contains(".omo"));
    }

    #[tokio::test]
    async fn an_override_wins() {
        let trust_store = store();
        assert!(
            resolve_project_trusted(ResolveProjectTrustedOptions {
                cwd: "/definitely/not/a/project",
                trust_store: &trust_store,
                trust_override: Some(true),
                default_project_trust: None,
                emit_project_trust: None,
                project_trust_context: &NoUi,
                on_extension_error: None,
                on_trust_store_error: None,
            })
            .await
        );
    }

    #[tokio::test]
    async fn a_project_without_trust_requiring_resources_is_trusted() {
        let trust_store = store();
        assert!(
            resolve_project_trusted(ResolveProjectTrustedOptions {
                cwd: "/definitely/not/a/project",
                trust_store: &trust_store,
                trust_override: None,
                default_project_trust: None,
                emit_project_trust: None,
                project_trust_context: &NoUi,
                on_extension_error: None,
                on_trust_store_error: None,
            })
            .await
        );
    }

    #[tokio::test]
    async fn the_extension_result_is_honored_and_remembered() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".omo")).expect("mkdir");
        std::fs::write(tmp.path().join(".omo/settings.json"), "{}").expect("write");
        let cwd = tmp.path().to_string_lossy().into_owned();
        let trust_store = store();
        let emit = |_cwd: &str| {
            Ok(Some(ProjectTrustEventResult {
                trusted: TrustDecision::Yes,
                remember: Some(true),
            }))
        };
        assert!(
            resolve_project_trusted(ResolveProjectTrustedOptions {
                cwd: &cwd,
                trust_store: &trust_store,
                trust_override: None,
                default_project_trust: None,
                emit_project_trust: Some(&emit),
                project_trust_context: &NoUi,
                on_extension_error: None,
                on_trust_store_error: None,
            })
            .await
        );
        assert_eq!(trust_store.get(&cwd).expect("get"), Some(true));
    }

    #[tokio::test]
    async fn an_unreadable_trust_store_fails_closed_before_the_default_and_the_prompt() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".omo")).expect("mkdir");
        std::fs::write(tmp.path().join(".omo/settings.json"), "{}").expect("write");
        let cwd = tmp.path().to_string_lossy().into_owned();
        let agent = tempfile::tempdir().expect("agent dir");
        std::fs::write(agent.path().join("trust.json"), "not json").expect("corrupt trust.json");
        let trust_store = ProjectTrustStore::new(&agent.path().to_string_lossy());
        let reported = std::sync::Mutex::new(Vec::new());
        let report = |message: &str| {
            reported
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(message.to_owned())
        };
        // "always" would trust, and the UI would answer "Trust": neither is reached.
        assert!(
            !resolve_project_trusted(ResolveProjectTrustedOptions {
                cwd: &cwd,
                trust_store: &trust_store,
                trust_override: None,
                default_project_trust: Some(DefaultProjectTrust::Always),
                emit_project_trust: None,
                project_trust_context: &YesUi,
                on_extension_error: None,
                on_trust_store_error: Some(&report),
            })
            .await
        );
        let reported = reported
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(reported.len(), 1);
        assert!(reported[0].contains("trust store"));
    }

    #[tokio::test]
    async fn an_untrusted_default_is_returned_for_a_ui_less_session() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".omo")).expect("mkdir");
        std::fs::write(tmp.path().join(".omo/settings.json"), "{}").expect("write");
        let cwd = tmp.path().to_string_lossy().into_owned();
        let trust_store = store();
        assert!(
            !resolve_project_trusted(ResolveProjectTrustedOptions {
                cwd: &cwd,
                trust_store: &trust_store,
                trust_override: None,
                default_project_trust: Some(DefaultProjectTrust::Never),
                emit_project_trust: None,
                project_trust_context: &YesUi,
                on_extension_error: None,
                on_trust_store_error: None,
            })
            .await
        );
    }
}
