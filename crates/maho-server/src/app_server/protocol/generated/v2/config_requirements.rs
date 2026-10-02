pub type ConfigRequirementsAllowedPermissionProfiles1 = std::collections::BTreeMap<String, bool>;

pub type ConfigRequirementsFeatureRequirements2 = std::collections::BTreeMap<String, bool>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigRequirements {
    #[serde(rename = "allowedApprovalPolicies", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub allowed_approval_policies: Option<Vec<Box<crate::app_server::protocol::generated::v2::ask_for_approval::AskForApproval>>>,
    #[serde(rename = "allowedSandboxModes", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub allowed_sandbox_modes: Option<Vec<Box<crate::app_server::protocol::generated::v2::sandbox_mode::SandboxMode>>>,
    #[serde(rename = "allowedWindowsSandboxImplementations", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub allowed_windows_sandbox_implementations: Option<Vec<Box<crate::app_server::protocol::generated::v2::windows_sandbox_setup_mode::WindowsSandboxSetupMode>>>,
    #[serde(rename = "allowedPermissionProfiles", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub allowed_permission_profiles: Option<ConfigRequirementsAllowedPermissionProfiles1>,
    #[serde(rename = "defaultPermissions")]
    pub default_permissions: Option<String>,
    #[serde(rename = "allowedWebSearchModes", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub allowed_web_search_modes: Option<Vec<Box<crate::app_server::protocol::generated::web_search_mode::WebSearchMode>>>,
    #[serde(rename = "allowManagedHooksOnly", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub allow_managed_hooks_only: Option<bool>,
    #[serde(rename = "allowAppshots", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub allow_appshots: Option<bool>,
    #[serde(rename = "allowRemoteControl", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub allow_remote_control: Option<bool>,
    #[serde(rename = "computerUse", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub computer_use: Option<Box<crate::app_server::protocol::generated::v2::computer_use_requirements::ComputerUseRequirements>>,
    #[serde(rename = "featureRequirements", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub feature_requirements: Option<ConfigRequirementsFeatureRequirements2>,
    #[serde(rename = "enforceResidency", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub enforce_residency: Option<Box<crate::app_server::protocol::generated::v2::residency_requirement::ResidencyRequirement>>,
    #[serde(rename = "models", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub models: Option<Box<crate::app_server::protocol::generated::v2::models_requirements::ModelsRequirements>>,
    #[serde(rename = "sqliteHome", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub sqlite_home: Option<Box<crate::app_server::protocol::generated::path_uri::PathUri>>,
    #[serde(rename = "logDir", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub log_dir: Option<Box<crate::app_server::protocol::generated::path_uri::PathUri>>,
    #[serde(rename = "modelCatalogJson", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub model_catalog_json: Option<Box<crate::app_server::protocol::generated::path_uri::PathUri>>,
    #[serde(rename = "checkForUpdateOnStartup", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub check_for_update_on_startup: Option<bool>,
    #[serde(rename = "allowLoginShell", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub allow_login_shell: Option<bool>,
    #[serde(rename = "feedback", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub feedback: Option<Box<crate::app_server::protocol::generated::v2::feedback_requirements::FeedbackRequirements>>,
    #[serde(rename = "windowsSandboxPrivateDesktop", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub windows_sandbox_private_desktop: Option<bool>,
}
