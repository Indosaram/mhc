use maho_ext_api::{Extension, ExtensionApi, McpLifecycle, McpServerDeclaration, McpTransport};
use std::{collections::BTreeMap, path::PathBuf};

pub const PROJECT_CWD_ENV: &str = "OMO_AST_GREP_PROJECT_CWD";

/// The native runtime replaces the staged JavaScript CLI, retaining its MCP protocol.
pub struct AstGrepComponentOptions {
    pub env: BTreeMap<String, String>,
    pub entry: PathBuf,
}

impl Default for AstGrepComponentOptions {
    fn default() -> Self {
        let entry = std::env::current_exe()
            .map(|path| path.with_file_name("ast-grep-mcp"))
            .unwrap_or_else(|_| PathBuf::from("ast-grep-mcp"));
        Self {
            env: std::env::vars().collect(),
            entry,
        }
    }
}

#[derive(Default)]
pub struct AstGrepComponent {
    pub options: AstGrepComponentOptions,
}

impl Extension for AstGrepComponent {
    fn register(&self, api: &mut ExtensionApi) {
        if !self.options.entry.is_file() {
            eprintln!(
                "omo-senpi ast-grep skipped: staged MCP runtime is missing ({})",
                self.options.entry.display()
            );
            return;
        }
        let cwd = self
            .options
            .env
            .get(PROJECT_CWD_ENV)
            .cloned()
            .unwrap_or_else(|| api.cwd.to_string_lossy().into_owned());
        api.register_mcp_server(
            "_ast_grep",
            McpServerDeclaration {
                transport: Some(McpTransport::Stdio),
                command: Some(self.options.entry.to_string_lossy().into_owned()),
                args: Some(vec!["mcp".into()]),
                env: Some(BTreeMap::from([(PROJECT_CWD_ENV.into(), cwd)])),
                enabled: Some(true),
                lifecycle: Some(McpLifecycle::Eager),
                ..Default::default()
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::{
        EventBus, ExtensionRuntime, ExtensionSessionProfile, LoadedExtension, SourceInfo,
    };

    fn api() -> ExtensionApi {
        ExtensionApi::new(
            LoadedExtension::new(
                "ast-grep",
                PathBuf::from("/workspace/project"),
                SourceInfo::default(),
            ),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            ExtensionRuntime::default(),
        )
    }

    fn staged(env: BTreeMap<String, String>) -> (tempfile::NamedTempFile, AstGrepComponent) {
        let file = tempfile::NamedTempFile::new().unwrap();
        let component = AstGrepComponent {
            options: AstGrepComponentOptions {
                env,
                entry: file.path().into(),
            },
        };
        (file, component)
    }

    fn assert_registration(env: BTreeMap<String, String>) {
        let (_file, component) = staged(env);
        let mut api = api();
        component.register(&mut api);
        let server = &api.registered.mcp_servers[0];
        assert_eq!(server.name, "_ast_grep");
        assert_eq!(server.config.transport, Some(McpTransport::Stdio));
        assert_eq!(server.config.args, Some(vec!["mcp".into()]));
        assert_eq!(
            server.config.command.as_deref(),
            component.options.entry.to_str()
        );
        assert_eq!(
            server.config.env.as_ref().unwrap()[PROJECT_CWD_ENV],
            "/workspace/project"
        );
        assert_eq!(server.config.enabled, Some(true));
        assert_eq!(server.config.lifecycle, Some(McpLifecycle::Eager));
    }

    #[test]
    fn registers_parent_session() {
        assert_registration(BTreeMap::new());
    }

    #[test]
    fn registers_child_session() {
        assert_registration(BTreeMap::from([(
            "SENPI_CODING_AGENT_SESSION_DIR".into(),
            "/tmp/senpi-child".into(),
        )]));
    }

    #[test]
    fn preserves_explicit_project_cwd() {
        let (_file, component) = staged(BTreeMap::from([(
            PROJECT_CWD_ENV.into(),
            "/explicit/project".into(),
        )]));
        let mut api = api();
        component.register(&mut api);
        assert_eq!(
            api.registered.mcp_servers[0].config.env.as_ref().unwrap()[PROJECT_CWD_ENV],
            "/explicit/project"
        );
    }

    #[test]
    fn missing_entry_skips_registration() {
        let dir = tempfile::tempdir().unwrap();
        let component = AstGrepComponent {
            options: AstGrepComponentOptions {
                env: BTreeMap::new(),
                entry: dir.path().join("absent"),
            },
        };
        let mut api = api();
        component.register(&mut api);
        assert!(api.registered.mcp_servers.is_empty());
    }

    #[test]
    fn directory_is_not_runtime_entry() {
        let dir = tempfile::tempdir().unwrap();
        let component = AstGrepComponent {
            options: AstGrepComponentOptions {
                env: BTreeMap::new(),
                entry: dir.path().into(),
            },
        };
        let mut api = api();
        component.register(&mut api);
        assert!(api.registered.mcp_servers.is_empty());
    }
}
