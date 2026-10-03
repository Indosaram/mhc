use super::{args::Args, help_flags_cache::HelpFlagsScope};
use std::path::{Path, PathBuf};
pub fn is_plain_help_request(parsed: &Args) -> bool { parsed.help && !parsed.print && parsed.mode.is_none() }
pub fn resolve_help_project_trust(parsed: &Args, cwd: &str, agent_dir: &str) -> Result<bool, String> {
    if let Some(decision) = parsed.project_trust_override { return Ok(decision); }
    if !maho_core::trust_manager::has_trust_requiring_project_resources(cwd, std::env::var("HOME").ok().as_deref()) { return Ok(true); }
    Ok(maho_core::trust_manager::ProjectTrustStore::new(agent_dir).get(cwd)? == Some(true))
}
pub fn help_flags_scope(parsed: &Args, cwd: &Path, agent_dir: &Path, project_trusted: bool) -> HelpFlagsScope {
    HelpFlagsScope { cwd: cwd.to_owned(), agent_dir: agent_dir.to_owned(), cli_extension_paths: parsed.extensions.iter().map(PathBuf::from).collect(), no_extensions: parsed.no_extensions, project_trusted }
}
pub fn cached_help_flags(argv: &[String], grok: bool, cwd: &Path, agent_dir: &Path, version: &str) -> Option<Vec<serde_json::Value>> {
    let parsed = super::args::parse_args(argv, grok).ok()?;
    if !is_plain_help_request(&parsed) || !parsed.diagnostics.is_empty() { return None; }
    if parsed.no_extensions { return Some(Vec::new()); }
    let trusted = resolve_help_project_trust(&parsed, cwd.to_str()?, agent_dir.to_str()?).ok()?;
    super::help_flags_cache::read_help_flags_cache(&help_flags_scope(&parsed, cwd, agent_dir, trusted), version)
}
