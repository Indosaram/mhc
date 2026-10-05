use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Listen {
    Stdio {
        url: String,
    },
    Unix {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    Ws {
        url: String,
        host: String,
        port: u16,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WsAuth {
    Off,
    TokenFile { path: String },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DaemonVerb {
    Start,
    Stop,
    Status,
    Restart,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CliArgs {
    Server {
        listen: Listen,
        #[serde(rename = "wsAuth", skip_serializing_if = "Option::is_none")]
        ws_auth: Option<WsAuth>,
        #[serde(rename = "jsonLogs")]
        json_logs: bool,
    },
    Daemon {
        verb: DaemonVerb,
        listen: Listen,
    },
    UsageError {
        message: String,
    },
}
pub const LISTEN_USAGE: &str =
    "Invalid --listen value. Use stdio://, unix://, unix:///abs/path, or ws://IP:PORT.";
pub fn format_usage(app_name: &str) -> String {
    let forms = "stdio://|unix://|unix:///abs/path|ws://IP:PORT";
    format!(
        "Usage: {app_name} app-server [--listen <{forms}>] [--ws-auth <token-file|off>] [--json-logs]\n       {app_name} app-server daemon <start|stop|status|restart> [--listen <{forms}>]"
    )
}
fn parse_listen(value: &str) -> Option<Listen> {
    match value {
        "stdio://" => return Some(Listen::Stdio { url: value.into() }),
        "unix://" => {
            return Some(Listen::Unix {
                url: value.into(),
                path: None,
            });
        }
        _ => {}
    }
    if value.starts_with("unix:///") {
        return Some(Listen::Unix {
            url: value.into(),
            path: Some(value[7..].into()),
        });
    }
    if !value.starts_with("ws://") {
        return None;
    }
    let parsed = url::Url::parse(value).ok()?;
    let host = parsed.host_str()?;
    // WHATWG hostname retains brackets for IPv6; node:isIP rejects brackets.
    if parsed.scheme() != "ws"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some_and(|q| !q.is_empty())
        || parsed.fragment().is_some_and(|q| !q.is_empty())
        || host.parse::<Ipv4Addr>().is_err()
    {
        return None;
    }
    let port = parsed.port()?;
    if port == 0 {
        return None;
    }
    Some(Listen::Ws {
        url: value.into(),
        host: host.into(),
        port,
    })
}
fn usage(message: impl Into<String>) -> CliArgs {
    CliArgs::UsageError {
        message: message.into(),
    }
}
pub fn parse_cli_args(args: &[String]) -> CliArgs {
    let daemon = args.first().is_some_and(|value| value == "daemon");
    let mut verb = None;
    let mut index = 0;
    let mut listen = Listen::Stdio {
        url: "stdio://".into(),
    };
    if daemon {
        verb = match args.get(1).map(String::as_str) {
            Some("start") => Some(DaemonVerb::Start),
            Some("stop") => Some(DaemonVerb::Stop),
            Some("status") => Some(DaemonVerb::Status),
            Some("restart") => Some(DaemonVerb::Restart),
            _ => return usage("Usage: app-server daemon <start|stop|status|restart>."),
        };
        listen = Listen::Ws {
            url: "ws://127.0.0.1:18800".into(),
            host: "127.0.0.1".into(),
            port: 18800,
        };
        index = 2;
    }
    let mut ws_auth = None;
    let mut json_logs = false;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--listen" => {
                let Some(parsed) = args.get(index + 1).and_then(|value| parse_listen(value)) else {
                    return usage(LISTEN_USAGE);
                };
                listen = parsed;
                index += 2;
            }
            "--ws-auth" if !daemon => {
                let Some(value) = args.get(index + 1) else {
                    return usage("--ws-auth requires <token-file|off>.");
                };
                ws_auth = Some(if value == "off" {
                    WsAuth::Off
                } else {
                    WsAuth::TokenFile {
                        path: value.clone(),
                    }
                });
                index += 2;
            }
            "--json-logs" if !daemon => {
                json_logs = true;
                index += 1;
            }
            _ => {
                return usage(format!(
                    "Unexpected app-server{} argument: {arg}",
                    if daemon { " daemon" } else { "" }
                ));
            }
        }
    }
    if let Some(verb) = verb {
        CliArgs::Daemon { verb, listen }
    } else {
        CliArgs::Server {
            listen,
            ws_auth,
            json_logs,
        }
    }
}
