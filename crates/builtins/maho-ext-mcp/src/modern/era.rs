//! Port of senpi mcp/modern/era.ts: dual-era discovery (`server/discover`) over HTTP and stdio.
use super::wire::{
    MODERN_PROTOCOL_VERSION, ModernClientInfo, ModernError, ModernErrorKind, PROTOCOL_VERSION_KEY,
    build_modern_request, classify_modern_error, derive_modern_headers,
};
use crate::errors::McpError;
use serde_json::{Map, Value};
use std::{collections::BTreeMap, future::Future};

/// Upstream SDK `LATEST_PROTOCOL_VERSION`: the initialize-based (legacy) era's newest version.
pub const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";
/// Upstream SDK `SUPPORTED_PROTOCOL_VERSIONS`: the implemented initialize-based versions.
pub const SDK_SUPPORTED_PROTOCOL_VERSIONS: [&str; 5] = [
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
    "2024-10-07",
];

#[derive(Clone, Debug)]
pub struct ProbeOptions {
    pub client_info: ModernClientInfo,
    pub client_capabilities: Value,
    pub supported_versions: Vec<String>,
}
/// A validated `server/discover` result.
pub type DiscoveryResult = Map<String, Value>;

#[derive(Clone, Debug, PartialEq)]
pub enum EraOutcome {
    /// `modern-OK` (`retry == false`) or `modern-version-retry`.
    Modern {
        retry: bool,
        version: String,
        discovery: Option<DiscoveryResult>,
        error: Option<ModernError>,
    },
    /// stdio only: `restart` means the probed child exited or its stream failed, so the legacy handshake needs a
    /// fresh process; `silent` means no answer in time, the same process gets the legacy handshake, and a late
    /// discovery reply may still upgrade it.
    LegacyFallback { restart: bool, silent: bool },
    Indeterminate {
        reason: String,
        action: String,
        modern_era: bool,
        status: Option<u16>,
        cause: Option<String>,
    },
}
impl EraOutcome {
    const LEGACY: Self = Self::LegacyFallback {
        restart: false,
        silent: false,
    };
}

/// The SDK numbers its own requests from 0; a string id can never answer one of them.
pub const STDIO_PROBE_ID: &str = "senpi-era-probe";

/// Rejection a stdio `ProbeSend` uses when the child gave no answer in time (as opposed to exiting).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProbeSendError {
    #[error("MCP stdio server did not answer discovery in time")]
    Silence,
    #[error("{0}")]
    Closed(String),
}

pub fn modern_error(body: &Value) -> Option<ModernError> {
    classify_modern_error(body)
}

fn valid_discovery(result: &Value) -> Option<DiscoveryResult> {
    let result = result.as_object()?;
    let strings = |value: &Value| {
        value
            .as_array()
            .is_some_and(|values| values.iter().all(Value::is_string))
    };
    let server_info = result
        .get("_meta")?
        .as_object()?
        .get("io.modelcontextprotocol/serverInfo")?
        .as_object()?;
    let valid = result.get("resultType").and_then(Value::as_str) == Some("complete")
        && result.get("supportedVersions").is_some_and(strings)
        && result.get("capabilities").is_some_and(Value::is_object)
        && server_info.get("name").is_some_and(Value::is_string)
        && server_info.get("version").is_some_and(Value::is_string)
        && result.get("instructions").is_none_or(Value::is_string)
        && result
            .get("ttlMs")
            .is_none_or(|ttl| ttl.as_f64().is_some_and(|ttl| ttl >= 0.0))
        && result.get("cacheScope").is_none_or(Value::is_string);
    valid.then(|| result.clone())
}

/// A complete discovery reply to `id`, or none.
pub fn discovery_reply(value: &Value, id: &Value) -> Option<DiscoveryResult> {
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || value.get("id") != Some(id) {
        return None;
    }
    valid_discovery(value.get("result")?)
}

/// A well-formed JSON-RPC error reply (any code), optionally addressed to `id`.
fn error_reply(value: &Value, id: Option<&Value>) -> bool {
    let id_ok = match (value.get("id"), id) {
        (Some(actual), Some(expected)) => actual == expected,
        (Some(actual), None) => actual.is_string() || actual.is_number() || actual.is_null(),
        (None, _) => false,
    };
    value.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && id_ok
        && value.get("error").is_some_and(|error| {
            error.get("code").is_some_and(Value::is_number)
                && error.get("message").is_some_and(Value::is_string)
        })
}

fn probe_request(options: &ProbeOptions, version: &str, id: Value) -> Value {
    let mut built = build_modern_request(
        id,
        "server/discover",
        &options.client_info,
        &options.client_capabilities,
        None,
    );
    built["params"]["_meta"][PROTOCOL_VERSION_KEY] = version.into();
    built
}

/// Preserve modern selection first; the SDK owns the implemented initialize-based versions.
fn mutual_version(error: &ModernError, options: &ProbeOptions) -> Option<String> {
    if error.kind != ModernErrorKind::UnsupportedProtocolVersionError {
        return None;
    }
    let supported = error.supported_versions.as_ref()?;
    let newest = |accept: &dyn Fn(&str) -> bool| {
        supported
            .iter()
            .filter(|candidate| accept(candidate.as_str()))
            .max()
            .cloned()
    };
    newest(&|candidate| {
        options
            .supported_versions
            .iter()
            .any(|version| version == candidate)
    })
    .or_else(|| newest(&|candidate| SDK_SUPPORTED_PROTOCOL_VERSIONS.contains(&candidate)))
}

fn indeterminate(reason: &str, action: impl Into<String>, modern_era: bool) -> EraOutcome {
    EraOutcome::Indeterminate {
        reason: reason.into(),
        action: action.into(),
        modern_era,
        status: None,
        cause: None,
    }
}

fn no_mutual_version(error: &ModernError) -> EraOutcome {
    let supported = error
        .supported_versions
        .as_ref()
        .map_or_else(|| "not provided".into(), |versions| versions.join(", "));
    indeterminate(
        "no-mutual-version",
        format!(
            "Update client or server to share a supported protocol version. Server supported versions: {supported}."
        ),
        true,
    )
}

fn parse_json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

#[derive(Clone, Debug, Default)]
pub struct ProbeHttpRequest {
    pub headers: BTreeMap<String, String>,
    pub body: String,
}
#[derive(Clone, Debug, Default)]
pub struct ProbeHttpResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: String,
}
/// `NeedsAuth` passes through untouched: the authorization flow already started and the connect path reports it.
#[derive(Clone, Debug)]
pub enum ProbeFetchError {
    NeedsAuth(Box<McpError>),
    Transport(String),
}

fn body(response: &ProbeHttpResponse) -> Value {
    if !response
        .content_type
        .as_deref()
        .is_some_and(|kind| kind.contains("text/event-stream"))
    {
        return parse_json(&response.body);
    }
    // This consumes only the POST's response stream, never a standalone GET.
    for event in response.body.replace("\r\n", "\n").split("\n\n") {
        let data = event
            .split('\n')
            .filter_map(|line| line.strip_prefix("data:"))
            .map(|line| line.strip_prefix(' ').unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n");
        let value = parse_json(&data);
        if discovery_reply(&value, &Value::from(1)).is_some() || modern_error(&value).is_some() {
            return value;
        }
    }
    Value::Null
}

pub async fn probe_http_era<F, Fut>(
    url: &url::Url,
    fetch: F,
    options: &ProbeOptions,
) -> Result<EraOutcome, McpError>
where
    F: Fn(url::Url, ProbeHttpRequest) -> Fut,
    Fut: Future<Output = Result<ProbeHttpResponse, ProbeFetchError>>,
{
    let retry_rejected = || {
        indeterminate(
            "version-retry-rejected",
            "Check server version negotiation; the modern retry failed.",
            true,
        )
    };
    let mut version = MODERN_PROTOCOL_VERSION.to_owned();
    for attempt in 0..2 {
        let discovery_request = probe_request(options, &version, Value::from(1));
        let mut headers = BTreeMap::from([
            ("Content-Type".to_owned(), "application/json".to_owned()),
            (
                "Accept".to_owned(),
                "application/json, text/event-stream".to_owned(),
            ),
        ]);
        headers.extend(derive_modern_headers(&discovery_request));
        let response = match fetch(
            url.clone(),
            ProbeHttpRequest {
                headers,
                body: discovery_request.to_string(),
            },
        )
        .await
        {
            Ok(response) => response,
            Err(ProbeFetchError::NeedsAuth(error)) => return Err(*error),
            Err(ProbeFetchError::Transport(cause)) => {
                return Ok(EraOutcome::Indeterminate {
                    reason: "transport".into(),
                    action: "Check the MCP endpoint connection and retry discovery.".into(),
                    modern_era: false,
                    status: None,
                    cause: Some(cause),
                });
            }
        };
        let value = body(&response);
        let error = modern_error(&value);
        let retry = attempt > 0;
        if response.status == 400 {
            let Some(error) = error else {
                return Ok(if attempt == 0 {
                    EraOutcome::LEGACY
                } else {
                    retry_rejected()
                });
            };
            if error.kind != ModernErrorKind::UnsupportedProtocolVersionError {
                return Ok(EraOutcome::Modern {
                    retry,
                    version,
                    discovery: None,
                    error: Some(error),
                });
            }
            if attempt == 1 {
                return Ok(indeterminate(
                    "version-retry-rejected",
                    "Update client or server; the single version retry was rejected.",
                    true,
                ));
            }
            let Some(mutual) = mutual_version(&error, options) else {
                return Ok(no_mutual_version(&error));
            };
            if !options.supported_versions.contains(&mutual) {
                return Ok(EraOutcome::LEGACY);
            }
            version = mutual;
            continue;
        }
        // A generic JSON-RPC error (no modern code) means the server does not know discovery, whatever the status.
        if error.is_none() && error_reply(&value, None) {
            return Ok(if attempt == 0 {
                EraOutcome::LEGACY
            } else {
                retry_rejected()
            });
        }
        if !(200..300).contains(&response.status) {
            return Ok(EraOutcome::Indeterminate {
                reason: "http-status".into(),
                action: "Check endpoint, authentication, or server health before retrying.".into(),
                modern_era: false,
                status: Some(response.status),
                cause: None,
            });
        }
        if let Some(discovery) = discovery_reply(&value, &Value::from(1)) {
            return Ok(EraOutcome::Modern {
                retry,
                version,
                discovery: Some(discovery),
                error: None,
            });
        }
        if let Some(error) = error {
            return Ok(EraOutcome::Modern {
                retry,
                version,
                discovery: None,
                error: Some(error),
            });
        }
        return Ok(indeterminate(
            "invalid-discovery",
            "Check that the server returns a complete server/discover envelope.",
            false,
        ));
    }
    Ok(indeterminate(
        "version-retry-rejected",
        "Update client or server protocol versions.",
        true,
    ))
}

/// `send` resolves with the reply to its request id, and fails with `ProbeSendError::Silence` when no reply came in
/// time or with `Closed` when the child exited or its stream failed. Once a server has shown it is modern (a -32022),
/// an unclean retry is never downgraded to legacy.
pub async fn probe_stdio_era<F, Fut>(send: F, options: &ProbeOptions) -> EraOutcome
where
    F: Fn(Value) -> Fut,
    Fut: Future<Output = Result<Value, ProbeSendError>>,
{
    let retry_rejected = || {
        indeterminate(
            "version-retry-rejected",
            "Update client or server; the single version retry was rejected.",
            true,
        )
    };
    let mut version = MODERN_PROTOCOL_VERSION.to_owned();
    for attempt in 0..2 {
        let id = Value::String(if attempt == 0 {
            STDIO_PROBE_ID.into()
        } else {
            format!("{STDIO_PROBE_ID}-{attempt}")
        });
        let value = match send(probe_request(options, &version, id.clone())).await {
            Ok(value) => value,
            Err(error) => {
                if attempt > 0 {
                    return retry_rejected();
                }
                return match error {
                    ProbeSendError::Silence => EraOutcome::LegacyFallback {
                        restart: false,
                        silent: true,
                    },
                    ProbeSendError::Closed(_) => EraOutcome::LegacyFallback {
                        restart: true,
                        silent: false,
                    },
                };
            }
        };
        let retry = attempt > 0;
        if let Some(discovery) = discovery_reply(&value, &id) {
            return EraOutcome::Modern {
                retry,
                version,
                discovery: Some(discovery),
                error: None,
            };
        }
        if !error_reply(&value, Some(&id)) {
            return if retry {
                retry_rejected()
            } else {
                EraOutcome::LegacyFallback {
                    restart: true,
                    silent: false,
                }
            };
        }
        let Some(error) = modern_error(&value) else {
            return if retry {
                retry_rejected()
            } else {
                EraOutcome::LEGACY
            };
        };
        let mutual = if retry {
            None
        } else {
            mutual_version(&error, options)
        };
        if !retry
            && error.kind == ModernErrorKind::UnsupportedProtocolVersionError
            && mutual.is_none()
        {
            return no_mutual_version(&error);
        }
        let Some(mutual) = mutual else {
            return EraOutcome::Modern {
                retry,
                version,
                discovery: None,
                error: Some(error),
            };
        };
        if !options.supported_versions.contains(&mutual) {
            return EraOutcome::LEGACY;
        }
        version = mutual;
    }
    retry_rejected()
}
