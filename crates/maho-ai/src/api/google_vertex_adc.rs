//! Port of senpi packages/ai/src/api/google-vertex-adc.ts.

use super::{GoogleRequestError, resolve_application_default_credentials};
use crate::types::ProviderEnv;
use crate::utils::provider_env::get_provider_env_value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VertexAdcAccess {
    pub access_token: String,
    pub project: String,
    pub quota_project: String,
}

fn resolve_projects(
    explicit: Option<&str>,
    env: Option<&ProviderEnv>,
    credentials: &serde_json::Value,
    discovered: Option<String>,
) -> Result<(String, String), GoogleRequestError> {
    let quota = credentials
        .get("quota_project_id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty());
    let project = explicit.filter(|value| !value.is_empty()).map(str::to_owned)
        .or_else(|| get_provider_env_value("GOOGLE_CLOUD_PROJECT", env))
        .or_else(|| get_provider_env_value("GCLOUD_PROJECT", env))
        .or_else(|| quota.map(str::to_owned))
        .or(discovered)
        .ok_or_else(|| GoogleRequestError::Message("Vertex AI requires a project ID. Set GOOGLE_CLOUD_PROJECT/GCLOUD_PROJECT or a quota project in Application Default Credentials.".into()))?;
    let quota_project = quota.map(str::to_owned).unwrap_or_else(|| project.clone());
    Ok((project, quota_project))
}

pub async fn get_vertex_adc_access(
    http: &reqwest::Client,
    project: Option<&str>,
    env: Option<&ProviderEnv>,
) -> Result<VertexAdcAccess, GoogleRequestError> {
    let key = get_provider_env_value("GOOGLE_APPLICATION_CREDENTIALS", env);
    let access_token = resolve_application_default_credentials(http, key.as_deref()).await?;
    let path = key.map(std::path::PathBuf::from).or_else(|| {
        dirs::home_dir()
            .map(|home| home.join(".config/gcloud/application_default_credentials.json"))
    });
    let credentials = if let Some(path) = path.filter(|path| path.exists()) {
        let text = std::fs::read_to_string(path)
            .map_err(|error| GoogleRequestError::Message(error.to_string()))?;
        serde_json::from_str(&text)
            .map_err(|error| GoogleRequestError::Message(error.to_string()))?
    } else {
        serde_json::Value::Null
    };
    let mut discovered = credentials
        .get("project_id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    if resolve_projects(project, env, &credentials, discovered.clone()).is_err() {
        let config_root = get_provider_env_value("CLOUDSDK_CONFIG", env)
            .map(std::path::PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".config/gcloud")));
        if let Some(root) = config_root {
            let active = match std::fs::read_to_string(root.join("active_config")) {
                Ok(active) => active,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => "default".into(),
                Err(error) => return Err(GoogleRequestError::Message(error.to_string())),
            };
            match std::fs::read_to_string(
                root.join("configurations")
                    .join(format!("config_{}", active.trim())),
            ) {
                Ok(text) => {
                    let mut core = false;
                    for line in text.lines().map(str::trim) {
                        if line.starts_with('[') {
                            core = line == "[core]";
                        }
                        if core
                            && let Some((key, value)) = line.split_once('=')
                            && key.trim() == "project"
                            && !value.trim().is_empty()
                        {
                            discovered = Some(value.trim().to_owned());
                            break;
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(GoogleRequestError::Message(error.to_string())),
            }
        }
    }
    if resolve_projects(project, env, &credentials, discovered.clone()).is_err() {
        let response = http
            .get("http://metadata.google.internal/computeMetadata/v1/project/project-id")
            .header("Metadata-Flavor", "Google")
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
            .map_err(|error| GoogleRequestError::Message(error.to_string()))?;
        if response.status().is_success() {
            let value = response
                .text()
                .await
                .map_err(|error| GoogleRequestError::Message(error.to_string()))?;
            if !value.is_empty() {
                discovered = Some(value);
            }
        }
    }
    let (project, quota_project) = resolve_projects(project, env, &credentials, discovered)?;
    Ok(VertexAdcAccess {
        access_token,
        project,
        quota_project,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_target_does_not_override_adc_billing_project() {
        let env = ProviderEnv::from_iter([("GOOGLE_CLOUD_PROJECT".into(), "environment".into())]);
        let credentials = serde_json::json!({"quota_project_id": "billing"});
        assert_eq!(
            resolve_projects(Some("target"), Some(&env), &credentials, None).expect("projects"),
            ("target".into(), "billing".into())
        );
        assert_eq!(
            resolve_projects(None, Some(&env), &credentials, None)
                .expect("projects")
                .0,
            "environment"
        );
    }
}
