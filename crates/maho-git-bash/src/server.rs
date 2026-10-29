//! Tool list and tool calls (spec §5).

use crate::host::{Host, RunRequest};
use crate::lookup::resolve;
use crate::settings::{
    MAX_TIMEOUT_MS, MIN_TIMEOUT_MS, default_timeout_ms, parse_timeout_arg,
};
use serde_json::{Map, Value, json};

/// Settings supplied by the embedding host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerOptions {
    /// Default `run` timeout configured by the embedding host; wins over the environment.
    pub default_timeout_ms: Option<u64>,
}

/// One `tools/call` reply: a single text content item and an `isError` flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolReply {
    pub text: String,
    pub is_error: bool,
}

impl ToolReply {
    fn ok(value: &Value) -> Self {
        Self {
            text: pretty(value),
            is_error: false,
        }
    }

    fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }

    /// The MCP `tools/call` result object.
    pub fn to_json(&self) -> Value {
        json!({
            "content": [{ "type": "text", "text": self.text }],
            "isError": self.is_error,
        })
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// The tool handler, generic over the machine it runs on.
pub struct Server<H: Host> {
    host: H,
    options: ServerOptions,
}

impl<H: Host> Server<H> {
    pub fn new(host: H, options: ServerOptions) -> Self {
        Self { host, options }
    }

    pub fn host(&self) -> &H {
        &self.host
    }

    fn is_windows(&self) -> bool {
        self.host.platform() == "win32"
    }

    /// Spec §3: native Windows and a `bash.exe` was found.
    pub fn is_available(&self) -> bool {
        self.is_windows() && resolve(&self.host).is_found()
    }

    /// The effective default `run` timeout (§5.2).
    pub fn default_timeout_ms(&self) -> u64 {
        default_timeout_ms(self.options.default_timeout_ms, |name| {
            self.host.env_var(name)
        })
    }

    /// `tools/list` entries: `run` (only when available), `which_bash`, `diagnose`.
    pub fn tool_definitions(&self) -> Vec<Value> {
        let mut tools = Vec::new();
        if self.is_available() {
            tools.push(json!({
                "name": "run",
                "description": "Run a shell command through Git Bash (bash -lc) on Windows and return its exit code, stdout and stderr. A non-zero exit code is reported in the result, not as an error.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The bash command line to run."
                        },
                        "timeout": {
                            "type": "integer",
                            "minimum": MIN_TIMEOUT_MS,
                            "maximum": MAX_TIMEOUT_MS,
                            "description": format!(
                                "Time limit in milliseconds ({MIN_TIMEOUT_MS}..{MAX_TIMEOUT_MS}); the command is killed when it is reached. Default: {} ms.",
                                self.default_timeout_ms()
                            )
                        },
                        "workdir": {
                            "type": "string",
                            "description": "Directory to run the command in. Defaults to the server's working directory."
                        },
                        "description": {
                            "type": "string",
                            "description": "Short note on what the command does; informational only."
                        }
                    },
                    "required": ["command"],
                    "additionalProperties": false
                }
            }));
        }
        tools.push(json!({
            "name": "which_bash",
            "description": "Report which Git Bash bash.exe would be used, where it was found and which paths were checked.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }));
        tools.push(json!({
            "name": "diagnose",
            "description": "Report the platform, whether the run tool is enabled, a status word and the Git Bash lookup result.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }));
        tools
    }

    /// Handle one `tools/call`. Missing or non-object `arguments` are treated as `{}`.
    pub async fn call_tool(&self, name: &str, arguments: Option<&Value>) -> ToolReply {
        let empty = Map::new();
        let arguments = arguments.and_then(Value::as_object).unwrap_or(&empty);
        match name {
            "which_bash" => ToolReply::ok(&resolve(&self.host).to_json()),
            "diagnose" => ToolReply::ok(&self.diagnose()),
            "run" => self.run(arguments).await,
            other => ToolReply::error(format!("Unknown tool: {other}")),
        }
    }

    fn diagnose(&self) -> Value {
        let platform = self.host.platform();
        let resolution = resolve(&self.host);
        let windows = platform == "win32";
        let enabled = windows && resolution.is_found();
        let status = if !windows {
            format!("disabled (Git Bash is only used on native Windows; this host is {platform})")
        } else if enabled {
            "ready".to_owned()
        } else {
            "missing-git-bash".to_owned()
        };
        json!({
            "platform": platform,
            "enabled": enabled,
            "status": status,
            "resolution": resolution.to_json(),
        })
    }

    async fn run(&self, arguments: &Map<String, Value>) -> ToolReply {
        if !self.is_windows() {
            return ToolReply::error(
                "The run tool is Windows-only: it runs commands through Git Bash on native Windows.",
            );
        }

        let command = match arguments.get("command") {
            Some(Value::String(text)) if !text.trim().is_empty() => text.trim().to_owned(),
            _ => {
                return ToolReply::error("`command` is required and must be a non-blank string.");
            }
        };

        let workdir_value = match arguments.get("workdir") {
            None | Some(Value::Null) => arguments.get("cwd"),
            present => present,
        };
        let workdir = match workdir_value {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) if !text.trim().is_empty() => Some(text.clone()),
            Some(_) => {
                return ToolReply::error(
                    "`workdir` must be a non-blank string naming the directory to run in.",
                );
            }
        };

        let timeout_value = match arguments.get("timeout") {
            None | Some(Value::Null) => arguments.get("timeout_ms"),
            present => present,
        };
        let timeout_ms = match timeout_value {
            None | Some(Value::Null) => self.default_timeout_ms(),
            Some(value) => match parse_timeout_arg(value) {
                Some(timeout) => timeout,
                None => {
                    return ToolReply::error(format!(
                        "`timeout` must be an integer number of milliseconds between {MIN_TIMEOUT_MS} and {MAX_TIMEOUT_MS}."
                    ));
                }
            },
        };

        let resolution = resolve(&self.host);
        let bash = match resolution.bash_path() {
            Some(path) => path.to_owned(),
            None => {
                return ToolReply {
                    text: pretty(&resolution.to_json()),
                    is_error: true,
                };
            }
        };

        let request = RunRequest {
            bash,
            command,
            workdir,
            timeout_ms,
        };
        match self.host.run(request).await {
            Ok(output) => ToolReply::ok(&json!({
                "exitCode": output.exit_code,
                "stdout": output.stdout,
                "stderr": output.stderr,
                "timedOut": output.timed_out,
            })),
            Err(error) => ToolReply::error(error),
        }
    }
}
