use std::{future::Future, path::Path, pin::Pin, sync::{Arc, OnceLock}};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedTool { Fd, Rg }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedToolStatus { pub warning: bool, pub message: String }
pub type ManagedToolStatusCallback = Arc<dyn Fn(ManagedToolStatus) + Send + Sync>;
pub type ManagedToolResolver = Arc<dyn Fn(ManagedTool, Option<ManagedToolStatusCallback>) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync>;
static RESOLVER: OnceLock<ManagedToolResolver> = OnceLock::new();
/// Installs the host's senpi `ensureTool` (managed-bin probe plus download); false if one is already installed.
pub fn set_managed_tool_resolver(resolver: ManagedToolResolver) -> bool { RESOLVER.set(resolver).is_ok() }
pub async fn ensure_tool(tool: ManagedTool) -> Option<String> { ensure_tool_with_status(tool, None).await }
pub async fn ensure_tool_with_status(tool: ManagedTool, on_status: Option<ManagedToolStatusCallback>) -> Option<String> {
    if let Some(resolver) = RESOLVER.get() { return resolver(tool, on_status).await; }
    find_on_path(tool, std::env::var_os("PATH").as_deref())
}
pub fn find_on_path(tool: ManagedTool, path: Option<&std::ffi::OsStr>) -> Option<String> {
    let names: &[&str] = match tool { ManagedTool::Fd => &["fd", "fdfind"], ManagedTool::Rg => &["rg"] };
    let path = path?;
    for name in names {
        let file = if cfg!(windows) { format!("{name}.exe") } else { (*name).to_owned() };
        if std::env::split_paths(path).any(|directory| Path::new(&directory).join(&file).is_file()) { return Some((*name).to_owned()); }
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fd_falls_back_to_debian_fdfind() {
        let directory = tempfile::tempdir().expect("directory");
        std::fs::write(directory.path().join(if cfg!(windows) { "fdfind.exe" } else { "fdfind" }), b"").expect("fdfind");
        assert_eq!(find_on_path(ManagedTool::Fd, Some(directory.path().as_os_str())), Some("fdfind".to_owned()));
        assert_eq!(find_on_path(ManagedTool::Rg, Some(directory.path().as_os_str())), None);
    }
}
