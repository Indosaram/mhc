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
    #[error("{0}")]
    Install(String),
}
impl PackagesInstallError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unavailable => "environment_installer_unavailable",
            Self::Cancelled => "environment_install_cancelled",
            Self::Timeout(_) => "environment_install_timeout",
            Self::InvalidArguments(_) => "environment_install_failed",
            Self::Install(error) => match error.split(':').next() {
                Some("environment_installer_unavailable") => "environment_installer_unavailable",
                Some("environment_install_cancelled") => "environment_install_cancelled",
                Some("environment_resolution_conflict") => "environment_resolution_conflict",
                _ => "environment_install_failed",
            },
        }
    }
}

pub type PackagesInstallFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send + 'a>>;

pub trait PackagesInstallEnvironments: Send + Sync {
    fn install<'a>(
        &'a self,
        manager: &'a str,
        requirements: &'a [String],
        timeout_seconds: Option<f64>,
        signal: Option<maho_ext_api::AbortSignal>,
    ) -> PackagesInstallFuture<'a>;
}

pub async fn run_packages_install(
    args: &Value,
    environments: Option<&dyn PackagesInstallEnvironments>,
    signal: Option<maho_ext_api::AbortSignal>,
) -> Result<Value, PackagesInstallError> {
    let manager = args
        .get("manager")
        .and_then(Value::as_str)
        .filter(|manager| matches!(*manager, "pip" | "bun" | "npm"))
        .ok_or_else(|| {
            PackagesInstallError::InvalidArguments("manager must be pip, bun, or npm".into())
        })?;
    let requirements_value = args.get("requirements").ok_or_else(|| {
        PackagesInstallError::InvalidArguments("requirements must be non-empty".into())
    })?;
    let requirements = if let Some(single) = requirements_value.as_str() {
        vec![single.to_owned()]
    } else {
        requirements_value
            .as_array()
            .ok_or_else(|| {
                PackagesInstallError::InvalidArguments(
                    "requirements must be a string or list".into(),
                )
            })?
            .iter()
            .map(|item| {
                item.as_str()
                    .filter(|text| !text.trim().is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        PackagesInstallError::InvalidArguments(
                            "requirements must contain non-empty strings".into(),
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    let requirements = requirements
        .into_iter()
        .map(|requirement| requirement.trim().to_owned())
        .filter(|requirement| !requirement.is_empty())
        .collect::<Vec<_>>();
    if requirements.is_empty() {
        return Err(PackagesInstallError::InvalidArguments(
            "requirements must not be empty".into(),
        ));
    }
    let timeout = args
        .get("timeout")
        .map(|value| {
            value
                .as_f64()
                .filter(|number| number.is_finite() && *number > 0.0)
                .ok_or_else(|| {
                    PackagesInstallError::InvalidArguments("timeout must be positive".into())
                })
        })
        .transpose()?
        .unwrap_or(600.0);
    let environments = environments.ok_or(PackagesInstallError::Unavailable)?;
    let duration = std::time::Duration::try_from_secs_f64(timeout)
        .map_err(|_| PackagesInstallError::InvalidArguments("timeout is too large".into()))?;
    let cancellation = maho_ext_api::AbortSignal::default();
    struct CancelOnDrop(maho_ext_api::AbortSignal);
    impl Drop for CancelOnDrop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _cancel = CancelOnDrop(cancellation.clone());
    let future = environments.install(
        manager,
        &requirements,
        Some(timeout),
        Some(cancellation.clone()),
    );
    tokio::pin!(future);
    let error = tokio::select! {
        biased;
        ()=async {match &signal {Some(signal)=>signal.cancelled().await,None=>std::future::pending().await}}=>PackagesInstallError::Cancelled,
        result=&mut future=>return result.map_err(PackagesInstallError::Install),
        ()=tokio::time::sleep(duration)=>PackagesInstallError::Timeout(timeout),
    };
    cancellation.abort();
    if tokio::time::timeout(std::time::Duration::from_secs(1), &mut future)
        .await
        .is_err()
    {
        maho_ext_api::diagnostic(
            "package installer did not finish cancellation within the cleanup deadline",
        );
    }
    Err(error)
}
