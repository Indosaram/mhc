use super::{
    env_expander::{ExpandEnvVarsOptions, expand_env_vars_in_object},
    types::*,
};
use std::fmt;

#[derive(Debug)]
pub enum McpTransformError {
    Json(serde_json::Error),
    MissingUrl {
        name: String,
        transport: &'static str,
    },
    MissingCommand(String),
}
impl fmt::Display for McpTransformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => error.fmt(f),
            Self::MissingUrl { name, transport } => write!(
                f,
                "MCP server \"{name}\" requires url for type \"{transport}\""
            ),
            Self::MissingCommand(name) => {
                write!(f, "MCP server \"{name}\" requires command for stdio type")
            }
        }
    }
}
impl std::error::Error for McpTransformError {}
impl From<serde_json::Error> for McpTransformError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

pub fn transform_mcp_server(
    name: &str,
    server: &ClaudeCodeMcpServer,
) -> Result<McpServerConfig, McpTransformError> {
    let expanded = expand_env_vars_in_object(
        &serde_json::to_value(server)?,
        ExpandEnvVarsOptions::default(),
    );
    let expanded: ClaudeCodeMcpServer = serde_json::from_value(expanded)?;
    match expanded.transport.unwrap_or(McpTransport::Stdio) {
        transport @ (McpTransport::Http | McpTransport::Sse) => {
            let transport = match transport {
                McpTransport::Http => "http",
                McpTransport::Sse => "sse",
                McpTransport::Stdio => "stdio",
            };
            let url = expanded.url.filter(|url| !url.is_empty()).ok_or_else(|| {
                McpTransformError::MissingUrl {
                    name: name.to_owned(),
                    transport,
                }
            })?;
            let oauth = expanded
                .oauth
                .filter(|oauth| oauth.client_id.is_some() || oauth.scopes.is_some());
            Ok(McpServerConfig::Remote {
                url,
                headers: expanded.headers.filter(|headers| !headers.is_empty()),
                oauth,
                enabled: true,
            })
        }
        McpTransport::Stdio => {
            let command = expanded
                .command
                .filter(|command| !command.is_empty())
                .ok_or_else(|| McpTransformError::MissingCommand(name.to_owned()))?;
            let mut commands = vec![command];
            commands.extend(expanded.args.unwrap_or_default());
            Ok(McpServerConfig::Local {
                command: commands,
                environment: expanded.env.filter(|env| !env.is_empty()),
                enabled: true,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn transport_requirements_and_empty_optional_fields_are_enforced() {
        for transport in ["http", "sse"] {
            let missing = serde_json::from_value(json!({"type":transport})).expect("server");
            assert!(matches!(
                transform_mcp_server("missing", &missing),
                Err(McpTransformError::MissingUrl { .. })
            ));
            let valid = serde_json::from_value(
                json!({"type":transport,"url":"https://mcp.test","headers":{},"oauth":{}}),
            )
            .expect("server");
            assert_eq!(
                serde_json::to_value(transform_mcp_server("remote", &valid).expect("remote"))
                    .expect("value"),
                json!({"type":"remote","url":"https://mcp.test","enabled":true})
            );
        }
        assert!(matches!(
            transform_mcp_server("missing", &ClaudeCodeMcpServer::default()),
            Err(McpTransformError::MissingCommand(_))
        ));
        let valid = serde_json::from_value(json!({"command":"node","args":["server.js"],"env":{}}))
            .expect("server");
        assert_eq!(
            serde_json::to_value(transform_mcp_server("local", &valid).expect("local"))
                .expect("value"),
            json!({"type":"local","command":["node","server.js"],"enabled":true})
        );
    }

    #[test]
    fn remote_oauth_and_headers_survive_transformation() {
        let server = serde_json::from_value(json!({"type":"http","url":"https://mcp.example.com","headers":{"Authorization":"Bearer test"},"oauth":{"clientId":"client-id","scopes":["read","write"]}})).expect("server");
        let config = transform_mcp_server("remote-oauth", &server).expect("transformed");
        assert_eq!(
            serde_json::to_value(config).expect("config"),
            json!({"type":"remote","url":"https://mcp.example.com","headers":{"Authorization":"Bearer test"},"oauth":{"clientId":"client-id","scopes":["read","write"]},"enabled":true})
        );
    }

    #[test]
    fn sensitive_references_are_removed_from_local_and_remote_configs() {
        for (source, expected) in [
            (
                json!({"command":"npx","args":["mcp-server"],"env":{"AUTH_TOKEN":"${MHC_MCP_TRANSFORM_SECRET}"}}),
                json!({"type":"local","command":["npx","mcp-server"],"environment":{"AUTH_TOKEN":""},"enabled":true}),
            ),
            (
                json!({"type":"http","url":"https://mcp.example.com/${MHC_MCP_TRANSFORM_SECRET}"}),
                json!({"type":"remote","url":"https://mcp.example.com/","enabled":true}),
            ),
        ] {
            let server = serde_json::from_value(source).expect("server");
            assert_eq!(
                serde_json::to_value(transform_mcp_server("secure", &server).expect("transformed"))
                    .expect("config"),
                expected
            );
        }
    }
}
