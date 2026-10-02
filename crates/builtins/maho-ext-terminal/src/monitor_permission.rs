use std::path::{Path,PathBuf};
pub struct MonitorPermissionInput {pub input:serde_json::Value,approved_parent:Option<PathBuf>}
impl MonitorPermissionInput {
    pub fn new(input:serde_json::Value)->Self {Self {input,approved_parent:None}}
    pub fn set_approved_monitor_parent(&mut self,parent:PathBuf) {self.approved_parent=Some(parent);}
    pub fn get_approved_monitor_parent(&self)->Option<&Path> {self.approved_parent.as_deref()}
}
pub fn monitor_parent(path:&Path)->&Path {path.parent().filter(|parent|!parent.as_os_str().is_empty()).unwrap_or_else(||Path::new("."))}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn approval_is_not_a_model_supplied_json_field() {
        let mut input=MonitorPermissionInput::new(serde_json::json!({"approvedParent":"/untrusted"}));
        assert!(input.get_approved_monitor_parent().is_none());input.set_approved_monitor_parent("/approved".into());assert_eq!(input.get_approved_monitor_parent(),Some(Path::new("/approved")));
        assert_eq!(monitor_parent(Path::new("file")),Path::new("."));assert_eq!(monitor_parent(Path::new("/tmp/file")),Path::new("/tmp"));
    }
}
