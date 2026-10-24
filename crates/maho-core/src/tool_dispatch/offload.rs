//! `ctx.offload` (senpi#3566 todo 8): a registered child module, resolved to the `module` job shape todo 4 ships
//! (`{ modulePath, export, args }`), runs through the session's runtime-selected dispatcher. Under `in-process` the
//! server runs the same module's export (`in_process_module.rs`); under `child` the dispatcher hands the job to the
//! session's tool child. Both report a module that cannot be loaded as `offload_module_unavailable`.
use super::tool_dispatcher::{ToolChildModuleJob, ToolDispatcher};
use maho_ext_api::{ChildModuleRegistration, OffloadOptions, RegisteredChildModule};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

/// Any error a runner, module or tool body can fail with; an [`OffloadError`] is downcast from it by its callers.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OffloadErrorCode {
    OffloadModuleUnavailable,
    OffloadUnknownModule,
    OffloadBadRegistration,
    OffloadBadOptions,
    OffloadSessionClosed,
    ToolChildDeadline,
    ToolRuntimeUnavailable,
}

impl OffloadErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OffloadModuleUnavailable => "offload_module_unavailable",
            Self::OffloadUnknownModule => "offload_unknown_module",
            Self::OffloadBadRegistration => "offload_bad_registration",
            Self::OffloadBadOptions => "offload_bad_options",
            Self::OffloadSessionClosed => "offload_session_closed",
            Self::ToolChildDeadline => "tool_child_deadline",
            Self::ToolRuntimeUnavailable => "tool_runtime_unavailable",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct OffloadError {
    pub code: OffloadErrorCode,
    /// The full message, `<code>: <detail>`.
    pub message: String,
}

impl OffloadError {
    pub fn new(code: OffloadErrorCode, message: impl AsRef<str>) -> Self {
        Self {
            code,
            message: format!("{}: {}", code.as_str(), message.as_ref()),
        }
    }
}

/// A registration as it is stored: a file-backed module only (an inline function is refused at registration).
///
/// # Errors
/// `offload_bad_registration` when `id`, `module` or `export` is empty.
pub fn validate_child_module_registration(
    registration: &ChildModuleRegistration,
) -> Result<(), OffloadError> {
    for (name, value) in [
        ("id", &registration.id),
        ("module", &registration.module),
        ("export", &registration.export),
    ] {
        if value.is_empty() {
            return Err(OffloadError::new(
                OffloadErrorCode::OffloadBadRegistration,
                format!(
                    "registerChildModule needs a non-empty string {name}{}",
                    if name == "module" {
                        " (a file path; inline functions are not accepted)"
                    } else {
                        ""
                    }
                ),
            ));
        }
    }
    Ok(())
}

/// `path.resolve(base, path)`: an absolute `path` stands alone, a relative one joins `base`; `.` and `..` fold away.
pub fn resolve_path(base: &Path, path: &str) -> String {
    let joined = base.join(path);
    let mut result = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            component => result.push(component.as_os_str()),
        }
    }
    result.to_string_lossy().into_owned()
}

/// The absolute module path a registration names: relative paths resolve against the registering extension's file.
pub fn child_module_path(entry: &RegisteredChildModule) -> String {
    let module = &entry.registration.module;
    if Path::new(module).is_absolute() {
        return module.clone();
    }
    let dir = Path::new(&entry.source_info.path)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    resolve_path(dir, module)
}

pub fn offload_job(
    entry: &RegisteredChildModule,
    args: Value,
    deadline_ms: Option<u64>,
) -> ToolChildModuleJob {
    ToolChildModuleJob {
        module_path: child_module_path(entry),
        export: entry.registration.export.clone(),
        args,
        idempotent: entry.registration.idempotent.then_some(true),
        deadline_ms: deadline_ms.map(|ms| ms as f64),
    }
}

/// The module files registered child modules name: granted to the session's child alongside its tools' modules.
pub fn child_module_roots(modules: &BTreeMap<String, RegisteredChildModule>) -> Vec<String> {
    modules
        .values()
        .filter(|entry| {
            Path::new(&entry.registration.module).is_absolute()
                || !entry.source_info.path.starts_with('<')
        })
        .map(child_module_path)
        .collect()
}

/// `ctx.offload(id, args, options)`: the registered module, run where the session's dispatcher places it.
///
/// # Errors
/// `offload_bad_options`, `offload_unknown_module`, or whatever the dispatcher's `offload` fails with.
pub async fn offload(
    dispatcher: &dyn ToolDispatcher,
    modules: &BTreeMap<String, RegisteredChildModule>,
    id: &str,
    args: Value,
    options: Option<OffloadOptions>,
) -> Result<Value, BoxError> {
    let deadline_ms = match options.as_ref().and_then(|options| options.deadline) {
        Some(deadline) => {
            let ms = deadline.as_millis();
            if ms == 0 {
                return Err(OffloadError::new(
                    OffloadErrorCode::OffloadBadOptions,
                    format!("deadlineMs must be a finite number of ms above 0, not {ms}"),
                )
                .into());
            }
            Some(u64::try_from(ms).unwrap_or(u64::MAX))
        }
        None => None,
    };
    let Some(entry) = modules.get(id) else {
        return Err(OffloadError::new(
            OffloadErrorCode::OffloadUnknownModule,
            format!("no child module is registered as \"{id}\""),
        )
        .into());
    };
    let signal = options.and_then(|options| options.signal);
    let outcome = dispatcher
        .offload(offload_job(entry, args, deadline_ms), signal)
        .await?;
    Ok(outcome.value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::SourceInfo;

    fn entry(module: &str, source: &str) -> RegisteredChildModule {
        RegisteredChildModule {
            registration: ChildModuleRegistration {
                id: "job".into(),
                module: module.into(),
                export: "run".into(),
                idempotent: true,
            },
            source_info: SourceInfo {
                path: source.into(),
                ..Default::default()
            },
        }
    }

    #[test]
    fn empty_fields_are_refused_with_the_upstream_message() {
        let mut registration = entry("w.js", "/a/ext.js").registration;
        registration.module.clear();
        let error = validate_child_module_registration(&registration).err();
        assert_eq!(
            error.map(|e| e.message),
            Some("offload_bad_registration: registerChildModule needs a non-empty string module (a file path; inline functions are not accepted)".to_owned())
        );
    }

    #[cfg(unix)]
    #[test]
    fn relative_module_resolves_against_the_registering_file_and_carries_options() {
        let entry = entry("../w/body.js", "/a/dir/ext.js");
        assert_eq!(child_module_path(&entry), "/a/w/body.js");
        let job = offload_job(&entry, Value::Null, Some(5));
        assert_eq!(job.idempotent, Some(true));
        assert_eq!(job.deadline_ms, Some(5.0));
        assert_eq!(job.export, "run");
    }

    #[test]
    fn inline_source_roots_are_left_out() {
        let modules: BTreeMap<_, _> = [
            ("a".to_owned(), entry("w.js", "<inline:1>")),
            ("b".to_owned(), entry("w.js", "/a/ext.js")),
        ]
        .into();
        assert_eq!(child_module_roots(&modules).len(), 1);
    }
}
