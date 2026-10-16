use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum PackagesInstallError {
    #[error("environment installer unavailable")]
    Unavailable,
    #[error("environment_install_cancelled")]
    Cancelled,
    #[error("packages.install(): {0}")]
    InvalidArguments(String),
    #[error("environment_install_timeout: the install did not finish within {0}s and was stopped")]
    Timeout(f64),
}

pub type PackagesInstallFuture<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send + 'a>>;

pub trait PackagesInstallEnvironments: Send + Sync {
    fn install<'a>(&'a self, manager: &'a str, requirements: &'a [String], timeout_seconds: Option<f64>, signal: Option<maho_ext_api::AbortSignal>) -> PackagesInstallFuture<'a>;
}

pub async fn run_packages_install(args: &Value, environments: Option<&dyn PackagesInstallEnvironments>, signal: Option<maho_ext_api::AbortSignal>) -> Result<Value, PackagesInstallError> {
    let manager = args.get("manager").and_then(Value::as_str).filter(|manager| matches!(*manager, "pip" | "bun" | "npm"))
        .ok_or_else(|| PackagesInstallError::InvalidArguments("manager must be pip, bun, or npm".into()))?;
    let requirements_value = args.get("requirements").ok_or_else(|| PackagesInstallError::InvalidArguments("requirements must be non-empty".into()))?;
    let requirements = if let Some(single) = requirements_value.as_str() { vec![single.to_owned()] } else {
        requirements_value.as_array().ok_or_else(|| PackagesInstallError::InvalidArguments("requirements must be a string or list".into()))?
            .iter().map(|item| item.as_str().filter(|text| !text.trim().is_empty()).map(str::to_owned)
                .ok_or_else(|| PackagesInstallError::InvalidArguments("requirements must contain non-empty strings".into()))).collect::<Result<Vec<_>, _>>()?
    };
    let requirements = requirements.into_iter().filter(|requirement| !requirement.trim().is_empty()).collect::<Vec<_>>();
    if requirements.is_empty() { return Err(PackagesInstallError::InvalidArguments("requirements must not be empty".into())); }
    let timeout = args.get("timeout").map(|value| value.as_f64().filter(|number| number.is_finite() && *number > 0.0)
        .ok_or_else(|| PackagesInstallError::InvalidArguments("timeout must be positive".into()))).transpose()?.unwrap_or(600.0);
    let environments = environments.ok_or(PackagesInstallError::Unavailable)?;
    let future = environments.install(manager, &requirements, Some(timeout), signal.clone());
    let timeout_error = tokio::time::timeout(std::time::Duration::from_secs_f64(timeout), future);
    let value = if let Some(signal) = signal {
        tokio::select! { result = timeout_error => result.map_err(|_| PackagesInstallError::Timeout(timeout))?.map_err(|_| PackagesInstallError::Unavailable)?, _ = signal.cancelled() => return Err(PackagesInstallError::Cancelled) }
    } else { timeout_error.await.map_err(|_| PackagesInstallError::Timeout(timeout))?.map_err(|_| PackagesInstallError::Unavailable)? };
    Ok(value)
}
