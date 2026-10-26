//! The synchronous filesystem side of `--print-prompt-sizes`, kept off the session path.

use maho_core::package_identity::find_nearest_package_identity;
use maho_core::prompt_size_inventory::PromptSizeInventory;
use maho_ext_mcp::config::load_mcp_config;
use maho_ext_mcp::config_schema::LoadMcpConfigOptions;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug)]
pub struct CliPromptSizeInventory;

impl PromptSizeInventory for CliPromptSizeInventory {
    fn package_name_of(&self, path: &str) -> Option<String> {
        find_nearest_package_identity(path).map(|id| id.package_name)
    }

    fn mcp_server_names(&self, cwd: &str, project_trusted: bool) -> Vec<String> {
        let env = BTreeMap::new();
        let agent_dir = maho_core::config::get_agent_dir();
        let options = LoadMcpConfigOptions {
            cwd: Path::new(cwd),
            agent_dir: Path::new(&agent_dir),
            env: &env,
            project_trusted,
            home_dir: None,
            claude_config_dir: None,
        };
        match load_mcp_config(options) {
            Ok(config) => config.servers.into_keys().collect(),
            Err(_) => Vec::new(),
        }
    }
}
