use super::{
    install_lock::with_root_lock, js_installer::*, js_revision_files::*, no_links::*,
    project_resolves::project_resolves, revision_store::*,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnvironmentMode {
    #[default]
    Managed,
    Project,
}
impl EnvironmentMode {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::Project => "project",
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct JsEnvironmentSettings {
    pub installer: JsInstallerChoice,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct EnvironmentSettings {
    pub auto_provision: Option<bool>,
    pub managed_root: Option<PathBuf>,
    pub js: JsEnvironmentSettings,
}
#[derive(Debug, Serialize)]
pub struct JsInstallReceipt {
    pub installer: String,
    pub mode: EnvironmentMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    pub added: Vec<String>,
    pub shadowed: Vec<String>,
}
pub struct JsEnvironmentsOptions {
    pub artifacts_dir: PathBuf,
    pub cwd: PathBuf,
    pub runtime: String,
    pub env: HashMap<String, String>,
    pub settings: EnvironmentSettings,
}
pub struct JsEnvironments {
    options: JsEnvironmentsOptions,
    state: Mutex<(EnvironmentMode, Option<PathBuf>)>,
    operations: tokio::sync::Mutex<()>,
}
impl JsEnvironments {
    pub fn new(options: JsEnvironmentsOptions) -> Self {
        Self {
            options,
            state: Mutex::new((EnvironmentMode::Managed, None)),
            operations: tokio::sync::Mutex::new(()),
        }
    }
    fn managed_root(&self) -> &Path {
        self.options
            .settings
            .managed_root
            .as_deref()
            .unwrap_or(&self.options.artifacts_dir)
    }
    fn managed_base(&self) -> PathBuf {
        self.managed_root()
            .join("environments/js")
            .join(&self.options.runtime)
    }
    pub fn package_root(&self) -> Result<Option<PathBuf>, EnvironmentError> {
        let state = self.state.lock().map_err(EnvironmentError::failed)?;
        Ok(match state.0 {
            EnvironmentMode::Managed => state.1.clone(),
            EnvironmentMode::Project => None,
        })
    }
    pub async fn set_mode(&self, mode: EnvironmentMode) -> Result<PathBuf, EnvironmentError> {
        let _operation = self.operations.lock().await;
        let base = self.managed_base();
        let root = match mode {
            EnvironmentMode::Managed => read_active_revision(&base)?.map(|revision| revision.dir),
            EnvironmentMode::Project => None,
        };
        let mut state = self.state.lock().map_err(EnvironmentError::failed)?;
        state.0 = mode;
        if mode == EnvironmentMode::Managed {
            state.1 = root;
        }
        Ok(match mode {
            EnvironmentMode::Managed => base,
            EnvironmentMode::Project => self.options.cwd.clone(),
        })
    }
    pub async fn install(
        &self,
        requested: &str,
        signal: &maho_ext_api::AbortSignal,
        on_output: Option<InstallOutput>,
        requested_installer: Option<JsInstallerChoice>,
    ) -> Result<JsInstallReceipt, EnvironmentError> {
        let operation = tokio::select! {biased;()=signal.cancelled()=>return Err(EnvironmentError::cancelled()),guard=self.operations.lock()=>guard};
        let result = self
            .install_locked(requested, signal, on_output, requested_installer)
            .await;
        drop(operation);
        result.map_err(|mut error| {
            let root = self.managed_root().to_string_lossy();
            let base = self.managed_base().to_string_lossy().into_owned();
            let cwd = self.options.cwd.to_string_lossy();
            error.message = error
                .message
                .replace(&base, "<root>")
                .replace(root.as_ref(), "<root>")
                .replace(cwd.as_ref(), "<cwd>");
            if signal.is_aborted() {
                EnvironmentError::cancelled()
            } else {
                error
            }
        })
    }
    async fn install_locked(
        &self,
        requested: &str,
        signal: &maho_ext_api::AbortSignal,
        on_output: Option<InstallOutput>,
        requested_installer: Option<JsInstallerChoice>,
    ) -> Result<JsInstallReceipt, EnvironmentError> {
        if self.options.settings.auto_provision == Some(false) {
            return Err(EnvironmentError {
                code: "environment_installer_unavailable",
                message:
                    "installs are turned off for this project (environments.autoProvision is false)"
                        .into(),
            });
        }
        let packages = parse_js_packages(requested)?
            .iter()
            .map(|spec| absolute_spec(spec, &self.options.cwd))
            .collect::<Vec<_>>();
        let (installer, command) = resolve_js_installer(
            requested_installer.unwrap_or(self.options.settings.js.installer),
            &self.options.env,
        )?;
        let mode = self.state.lock().map_err(EnvironmentError::failed)?.0;
        let root = match mode {
            EnvironmentMode::Project => self.options.cwd.clone(),
            EnvironmentMode::Managed => {
                let base = self.managed_base();
                assert_no_links_below(self.managed_root(), &base)?;
                mkdir_private(&base)?;
                assert_no_links_below(self.managed_root(), &base)?;
                std::fs::canonicalize(base)?
            }
        };
        let lock_root = match mode {
            EnvironmentMode::Managed => root.clone(),
            EnvironmentMode::Project => root.join(".omo/js-packages"),
        };
        mkdir_private(&lock_root)?;
        let _lock = with_root_lock(&lock_root, signal).await?;
        let staging = match mode {
            EnvironmentMode::Project => None,
            EnvironmentMode::Managed => Some(Arc::new(stage_next_revision(&root)?)),
        };
        let install_root = staging
            .as_ref()
            .map_or(root.as_path(), |stage| stage.dir.as_path());
        if mode == EnvironmentMode::Managed {
            carry_npmrc_settings(install_root)?;
            remove_entry(&install_root.join("bunfig.toml"))?;
            seed_package_json(install_root)?;
            for entry in [
                "package.json",
                "package-lock.json",
                "bun.lock",
                "bun.lockb",
                "node_modules",
                ".npmrc",
                "bunfig.toml",
            ] {
                assert_no_links_below(&root, &install_root.join(entry))?;
            }
        }
        let before = dependency_names(install_root)?;
        let recorded_specs = recorded_file_specs(install_root);
        let sources = recorded_install_sources(install_root);
        let run = async |remove: Option<String>| {
            run_js_install(JsInstall {
                installer,
                command: &command,
                root: install_root,
                packages: &packages,
                remove: remove.as_deref(),
                recorded_specs: &recorded_specs,
                cwd: &self.options.cwd,
                env: &self.options.env,
                signal,
                on_output: on_output.as_ref(),
            })
            .await
        };
        if mode == EnvironmentMode::Managed {
            for spec in &packages {
                let Some(name) = requested_package_name(spec).await else {
                    continue;
                };
                if !before.contains(&name) {
                    continue;
                }
                let path = Path::new(spec.strip_prefix("file:").unwrap_or(spec));
                let directory = if path.is_absolute() {
                    std::fs::canonicalize(path)
                        .ok()
                        .filter(|path| path.is_dir())
                } else {
                    None
                };
                if directory.as_ref() != sources.get(&name) {
                    run(Some(name)).await?;
                }
            }
        }
        run(None).await?;
        if mode == EnvironmentMode::Managed {
            assert_built_revision_links(install_root)?;
        }
        let added = dependency_names(install_root)?
            .into_iter()
            .filter(|name| !before.contains(name))
            .collect::<Vec<_>>();
        let revision = if let Some(staging) = staging {
            let revision = publish_next_revision(staging, signal).await?;
            self.state.lock().map_err(EnvironmentError::failed)?.1 = Some(revision.dir);
            Some(revision.number)
        } else {
            None
        };
        let shadowed = if mode == EnvironmentMode::Managed {
            added
                .iter()
                .filter(|name| project_resolves(&self.options.cwd, name))
                .cloned()
                .collect()
        } else {
            vec![]
        };
        Ok(JsInstallReceipt {
            installer: installer.into(),
            mode,
            revision,
            added,
            shadowed,
        })
    }
}
fn dependency_names(root: &Path) -> Result<Vec<String>, EnvironmentError> {
    let text = match std::fs::read(root.join("package.json")) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error.into()),
    };
    let manifest: Value = serde_json::from_slice(&text).map_err(EnvironmentError::failed)?;
    Ok(manifest
        .get("dependencies")
        .and_then(Value::as_object)
        .map_or_else(Vec::new, |dependencies| {
            dependencies.keys().cloned().collect()
        }))
}
impl crate::bridges::packages_install::PackagesInstallEnvironments for JsEnvironments {
    fn install<'a>(
        &'a self,
        manager: &'a str,
        requirements: &'a [String],
        _: Option<f64>,
        signal: Option<maho_ext_api::AbortSignal>,
    ) -> crate::bridges::packages_install::PackagesInstallFuture<'a> {
        Box::pin(async move {
            let choice=match manager {"bun"=>JsInstallerChoice::Bun,"npm"=>JsInstallerChoice::Npm,_=>return Err("environment_installer_unavailable: bun/npm installs run only in a JavaScript cell".into())};
            if requirements
                .iter()
                .any(|spec| spec.chars().any(char::is_whitespace))
            {
                return Err(
                    "environment_install_failed: a package spec cannot contain whitespace".into(),
                );
            }
            let signal = signal.unwrap_or_default();
            let receipt =
                JsEnvironments::install(self, &requirements.join(" "), &signal, None, Some(choice))
                    .await
                    .map_err(|error| error.to_string())?;
            Ok(json!(receipt))
        })
    }
}
