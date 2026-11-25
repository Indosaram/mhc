use std::path::{Path, PathBuf};
pub struct MonitorPermissionInput {
    pub input: serde_json::Value,
    approved_parent: Option<PathBuf>,
}
impl MonitorPermissionInput {
    pub fn new(input: serde_json::Value) -> Self {
        Self {
            input,
            approved_parent: None,
        }
    }
    pub fn set_approved_monitor_parent(&mut self, parent: PathBuf) {
        self.approved_parent = Some(parent);
    }
    pub fn get_approved_monitor_parent(&self) -> Option<&Path> {
        self.approved_parent.as_deref()
    }
}
pub fn monitor_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// Carry the parent the permission parser approved onto a copied monitor input (`senpi#5795`).
/// Only copies when `from` and `to` name the same `path` string.
pub fn transfer_approved_monitor_parent(
    from: &MonitorPermissionInput,
    to: &mut MonitorPermissionInput,
) {
    let Some(parent) = from.get_approved_monitor_parent() else {
        return;
    };
    let Some(from_path) = from.input.get("path").and_then(serde_json::Value::as_str) else {
        return;
    };
    let Some(to_path) = to.input.get("path").and_then(serde_json::Value::as_str) else {
        return;
    };
    if from_path == to_path {
        to.set_approved_monitor_parent(parent.to_path_buf());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn approval_is_not_a_model_supplied_json_field() {
        let mut input =
            MonitorPermissionInput::new(serde_json::json!({"approvedParent":"/untrusted"}));
        assert!(input.get_approved_monitor_parent().is_none());
        input.set_approved_monitor_parent("/approved".into());
        assert_eq!(
            input.get_approved_monitor_parent(),
            Some(Path::new("/approved"))
        );
        assert_eq!(monitor_parent(Path::new("file")), Path::new("."));
        assert_eq!(monitor_parent(Path::new("/tmp/file")), Path::new("/tmp"));
    }

    #[test]
    fn transfer_copies_approved_parent_only_for_matching_path() {
        let mut from = MonitorPermissionInput::new(serde_json::json!({"path": "/tmp/a.log"}));
        from.set_approved_monitor_parent("/tmp".into());

        let mut same = MonitorPermissionInput::new(serde_json::json!({"path": "/tmp/a.log"}));
        transfer_approved_monitor_parent(&from, &mut same);
        assert_eq!(same.get_approved_monitor_parent(), Some(Path::new("/tmp")));

        let mut different = MonitorPermissionInput::new(serde_json::json!({"path": "/tmp/b.log"}));
        transfer_approved_monitor_parent(&from, &mut different);
        assert!(different.get_approved_monitor_parent().is_none());
    }
}
