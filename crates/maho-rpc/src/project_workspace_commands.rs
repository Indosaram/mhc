//! Port of senpi `packages/coding-agent/src/modes/rpc/project-workspace-commands.ts`.
//!
//! The session workspace's project trust and project-scope settings over RPC.

use crate::project_write_gate::{ProjectWriteRefusal, RPC_ERROR_PROJECT_NOT_TRUSTED};
use crate::rpc_types::RPC_ERROR_INVALID_ARGUMENT;
use futures::future::BoxFuture;
use maho_core::agent_session::AgentSession;
use maho_core::config::home_dir;
use maho_core::settings_manager::{SettingsScope, get_settings_path};
use maho_core::trust_manager::{
    ProjectTrustOption, ProjectTrustStore, ProjectTrustUpdate, get_project_trust_options,
    normalize_cwd,
};
use maho_core::trust_requiring_resources::has_trust_requiring_project_resources_async;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const RPC_ERROR_PROJECT_TRUST_LOCKED: &str = "project_trust_locked";
pub const RPC_ERROR_PROJECT_TRUST_EXTENSION_DECIDED: &str = "project_trust_extension_decided";
pub const RPC_ERROR_PROJECT_SETTINGS_WRITE_FAILED: &str = "project_settings_write_failed";
pub const RPC_ERROR_PROJECT_SETTINGS_UNREADABLE: &str = "project_settings_unreadable";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectTrustChoice {
    Trust,
    TrustParent,
    DoNotTrust,
    NoInherit,
}

impl ProjectTrustChoice {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trust => "trust",
            Self::TrustParent => "trust_parent",
            Self::DoNotTrust => "do_not_trust",
            Self::NoInherit => "no_inherit",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "trust" => Some(Self::Trust),
            "trust_parent" => Some(Self::TrustParent),
            "do_not_trust" => Some(Self::DoNotTrust),
            "no_inherit" => Some(Self::NoInherit),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcProjectTrustChoice {
    pub choice: ProjectTrustChoice,
    pub label: String,
    pub trusted: bool,
    /// The folder the choice saves its decision for.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSavedProjectTrust {
    pub path: String,
    pub trusted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcProjectTrust {
    pub cwd: String,
    /// Whether this session runs with the project trusted now.
    pub project_trusted: bool,
    /// Whether the folder has anything trust gates; without it the project counts as trusted.
    pub requires_trust: bool,
    /// The nearest saved decision at or above the folder, as `/trust` shows it.
    pub saved: Option<RpcSavedProjectTrust>,
    /// The launch named the trust (`--approve`/`--no-approve`); `set_project_trust` is refused.
    pub locked: bool,
    pub choices: Vec<RpcProjectTrustChoice>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcProjectSettings {
    pub cwd: String,
    pub project_trusted: bool,
    pub path: String,
    pub exists: bool,
    pub settings: Option<serde_json::Map<String, Value>>,
    pub error: Option<String>,
}

/// Optional injected extension `project_trust` evaluator (since `maho-ext-host` is a dev-dep only).
pub type TrustExtensionDecider = dyn Fn(&str) -> BoxFuture<'static, Option<bool>> + Send + Sync;

/// Validates a top-level project settings key (`^[A-Za-z][A-Za-z0-9_]*$`).
pub fn is_valid_settings_key(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphabetic() && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn choice_of(option: &ProjectTrustOption) -> ProjectTrustChoice {
    if !option.trusted {
        return ProjectTrustChoice::DoNotTrust;
    }
    if option.updates.len() > 1 {
        ProjectTrustChoice::TrustParent
    } else {
        ProjectTrustChoice::Trust
    }
}

fn no_inherit_option(cwd: &str) -> ProjectTrustOption {
    let trust_path = normalize_cwd(cwd);
    ProjectTrustOption {
        label: "Do not inherit parent trust".to_owned(),
        trusted: false,
        updates: vec![ProjectTrustUpdate {
            path: trust_path.clone(),
            decision: None,
        }],
        saved_path: Some(trust_path),
    }
}

fn trust_choices(cwd: &str) -> Vec<(ProjectTrustChoice, ProjectTrustOption)> {
    get_project_trust_options(cwd, false)
        .into_iter()
        .map(|option| (choice_of(&option), option))
        .collect()
}

fn settable_trust_choices(cwd: &str) -> Vec<(ProjectTrustChoice, ProjectTrustOption)> {
    let mut choices = trust_choices(cwd);
    choices.push((ProjectTrustChoice::NoInherit, no_inherit_option(cwd)));
    choices
}

pub async fn read_project_trust(session: &AgentSession) -> RpcProjectTrust {
    let cwd = session.cwd();
    let home = home_dir();
    let entry = ProjectTrustStore::new(&session.agent_dir())
        .get_entry(&cwd)
        .ok()
        .flatten();
    let (project_trusted, locked) = session.with_settings_manager(|settings| {
        (
            settings.is_project_trusted(),
            settings.is_project_trust_locked(),
        )
    });
    let requires_trust = has_trust_requiring_project_resources_async(&cwd, Some(&home)).await;
    RpcProjectTrust {
        cwd: cwd.clone(),
        project_trusted,
        requires_trust,
        saved: entry.map(|e| RpcSavedProjectTrust {
            path: e.path,
            trusted: e.decision,
        }),
        locked,
        choices: trust_choices(&cwd)
            .into_iter()
            .map(|(choice, option)| RpcProjectTrustChoice {
                choice,
                label: option.label,
                trusted: option.trusted,
                path: option.saved_path.unwrap_or_else(|| cwd.clone()),
            })
            .collect(),
    }
}

pub async fn write_project_trust<Order, Fut>(
    session: &AgentSession,
    choice: Option<&Value>,
    in_trust_change_order: Order,
    extension_decider: Option<&TrustExtensionDecider>,
) -> Result<RpcProjectTrust, ProjectWriteRefusal>
where
    Order: FnOnce(BoxFuture<'static, Result<RpcProjectTrust, ProjectWriteRefusal>>) -> Fut,
    Fut: std::future::Future<Output = Result<RpcProjectTrust, ProjectWriteRefusal>>,
{
    let cwd = session.cwd();
    let settable = settable_trust_choices(&cwd);
    let parsed_choice = choice
        .and_then(Value::as_str)
        .and_then(ProjectTrustChoice::parse);
    let Some((selected_choice, selected_option)) =
        parsed_choice.and_then(|c| settable.into_iter().find(|(entry_c, _)| *entry_c == c))
    else {
        let valid_names: Vec<&str> = settable_trust_choices(&cwd)
            .into_iter()
            .map(|(c, _)| c.as_str())
            .collect();
        return Err(ProjectWriteRefusal {
            message: format!(
                "set_project_trust requires 'choice' to be one of: {}.",
                valid_names.join(", ")
            ),
            error_code: RPC_ERROR_INVALID_ARGUMENT.to_owned(),
            error_data: None,
        });
    };
    if session.with_settings_manager(|s| s.is_project_trust_locked()) {
        return Err(ProjectWriteRefusal {
            message: "This engine was launched with an explicit project trust flag; a saved decision cannot change it.".to_owned(),
            error_code: RPC_ERROR_PROJECT_TRUST_LOCKED.to_owned(),
            error_data: None,
        });
    }
    let session_cloned = session.clone();
    let agent_dir = session.agent_dir();
    let decider_fut = extension_decider.map(|decider| decider(&cwd));
    let task: BoxFuture<'static, Result<RpcProjectTrust, ProjectWriteRefusal>> = Box::pin(
        async move {
            let home = home_dir();
            let requires_trust =
                has_trust_requiring_project_resources_async(&cwd, Some(&home)).await;
            let decided = if requires_trust {
                match decider_fut {
                    Some(fut) => fut.await,
                    None => None,
                }
            } else {
                None
            };
            let no_inherit = selected_choice == ProjectTrustChoice::NoInherit;
            if !no_inherit
                && let Some(decided_trust) = decided
                && decided_trust != selected_option.trusted
            {
                return Err(ProjectWriteRefusal {
                    message: format!(
                        "A project_trust extension handler decides this folder is {}; nothing was saved.",
                        if decided_trust {
                            "trusted"
                        } else {
                            "untrusted"
                        }
                    ),
                    error_code: RPC_ERROR_PROJECT_TRUST_EXTENSION_DECIDED.to_owned(),
                    error_data: None,
                });
            }
            ProjectTrustStore::new(&agent_dir)
                .set_many(&selected_option.updates)
                .map_err(|err| ProjectWriteRefusal {
                    message: format!("Saving project trust failed: {err}"),
                    error_code: RPC_ERROR_PROJECT_SETTINGS_WRITE_FAILED.to_owned(),
                    error_data: None,
                })?;
            let (was_trusted, from_full_access) = session_cloned
                .with_settings_manager(|s| (s.is_project_trusted(), s.is_trust_from_full_access()));
            let trusted = !requires_trust
                || decided.unwrap_or(if no_inherit {
                    from_full_access
                } else {
                    selected_option.trusted
                });
            if was_trusted != trusted {
                session_cloned.with_settings_manager_mut(|s| s.set_project_trusted(trusted));
                if let Err(err) = session_cloned.apply_project_trust_change().await {
                    return Err(ProjectWriteRefusal {
                        message: format!("Applying project trust change failed: {err}"),
                        error_code: RPC_ERROR_PROJECT_SETTINGS_WRITE_FAILED.to_owned(),
                        error_data: None,
                    });
                }
            }
            Ok(read_project_trust(&session_cloned).await)
        },
    );
    in_trust_change_order(task).await
}

pub fn read_project_settings(session: &AgentSession) -> RpcProjectSettings {
    let cwd = session.cwd();
    let path = get_settings_path(
        &cwd,
        &session.agent_dir(),
        SettingsScope::Project,
        &home_dir(),
    );
    let (project_trusted, file) = session.with_settings_manager(|settings| {
        (
            settings.is_project_trusted(),
            settings.read_project_settings_file(&path),
        )
    });
    RpcProjectSettings {
        cwd,
        project_trusted,
        path: file.path,
        exists: file.exists,
        settings: file.settings,
        error: file.error,
    }
}

pub async fn write_project_setting(
    session: &AgentSession,
    key: Option<&Value>,
    value: Option<&Value>,
) -> Result<RpcProjectSettings, ProjectWriteRefusal> {
    let (Some(key_str), Some(val)) = (key.and_then(Value::as_str), value) else {
        return Err(ProjectWriteRefusal {
            message: "set_project_setting requires an identifier 'key' and a JSON 'value' (null removes the key).".to_owned(),
            error_code: RPC_ERROR_INVALID_ARGUMENT.to_owned(),
            error_data: None,
        });
    };
    if !is_valid_settings_key(key_str) {
        return Err(ProjectWriteRefusal {
            message: "set_project_setting requires an identifier 'key' and a JSON 'value' (null removes the key).".to_owned(),
            error_code: RPC_ERROR_INVALID_ARGUMENT.to_owned(),
            error_data: None,
        });
    }
    if !session.with_settings_manager(|s| s.is_project_trusted()) {
        return Err(ProjectWriteRefusal {
            message: "Project is not trusted; refusing to write project settings. Trust it with set_project_trust first.".to_owned(),
            error_code: RPC_ERROR_PROJECT_NOT_TRUSTED.to_owned(),
            error_data: None,
        });
    }
    let file = read_project_settings(session);
    if let Some(error) = file.error {
        return Err(ProjectWriteRefusal {
            message: error,
            error_code: RPC_ERROR_PROJECT_SETTINGS_UNREADABLE.to_owned(),
            error_data: None,
        });
    }
    let mut patch = serde_json::Map::new();
    patch.insert(key_str.to_owned(), val.clone());
    if let Err(err) = session.with_settings_manager_mut(|s| s.set(SettingsScope::Project, &patch)) {
        return Err(ProjectWriteRefusal {
            message: format!("Writing {} failed: {err}", file.path),
            error_code: RPC_ERROR_PROJECT_SETTINGS_WRITE_FAILED.to_owned(),
            error_data: None,
        });
    }
    Ok(read_project_settings(session))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_key_validation_blocks_prototype_and_path_keys() {
        assert!(is_valid_settings_key("defaultModel"));
        assert!(is_valid_settings_key("foo_bar2"));
        assert!(!is_valid_settings_key(""));
        assert!(!is_valid_settings_key("1abc"));
        assert!(!is_valid_settings_key("__proto__"));
        assert!(!is_valid_settings_key("a.b"));
        assert!(!is_valid_settings_key("a/b"));
    }

    #[test]
    fn trust_choice_round_trips() {
        for choice in [
            ProjectTrustChoice::Trust,
            ProjectTrustChoice::TrustParent,
            ProjectTrustChoice::DoNotTrust,
            ProjectTrustChoice::NoInherit,
        ] {
            assert_eq!(ProjectTrustChoice::parse(choice.as_str()), Some(choice));
        }
        assert_eq!(ProjectTrustChoice::parse("other"), None);
    }
}
