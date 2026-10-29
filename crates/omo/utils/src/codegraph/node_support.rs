use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const CODEGRAPH_MIN_NODE_MAJOR: u32 = 20;
pub const CODEGRAPH_BLOCKED_NODE_MAJOR: u32 = 25;
pub const CODEGRAPH_UNSAFE_NODE_ENV: &str = "CODEGRAPH_ALLOW_UNSAFE_NODE";
pub const CODEGRAPH_NODE_BIN_ENV: &str = "CODEGRAPH_NODE_BIN";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodegraphNodeUnsupportedReason {
    #[serde(rename = "too-new")]
    TooNew,
    #[serde(rename = "too-old")]
    TooOld,
}

impl CodegraphNodeUnsupportedReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TooNew => "too-new",
            Self::TooOld => "too-old",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodegraphNodeSupport {
    pub major: u32,
    #[serde(rename = "override")]
    pub r#override: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<CodegraphNodeUnsupportedReason>,
    pub supported: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvaluateCodegraphNodeSupportOptions {
    pub env: Option<BTreeMap<String, String>>,
    pub node_version: Option<String>,
}

fn parse_node_major(version: &str) -> u32 {
    let normalized = version.strip_prefix('v').unwrap_or(version);
    let major_str = normalized.split('.').next().unwrap_or("");
    major_str.parse::<u32>().unwrap_or(0)
}

pub fn evaluate_codegraph_node_support(
    options: &EvaluateCodegraphNodeSupportOptions,
) -> CodegraphNodeSupport {
    let major = options
        .node_version
        .as_deref()
        .map(parse_node_major)
        .unwrap_or(0);

    let override_val = options
        .env
        .as_ref()
        .and_then(|env| env.get(CODEGRAPH_UNSAFE_NODE_ENV))
        .map(|val| !val.trim().is_empty())
        .unwrap_or_else(|| {
            std::env::var(CODEGRAPH_UNSAFE_NODE_ENV)
                .map(|val| !val.trim().is_empty())
                .unwrap_or(false)
        });

    if major >= CODEGRAPH_BLOCKED_NODE_MAJOR {
        return CodegraphNodeSupport {
            major,
            r#override: override_val,
            reason: Some(CodegraphNodeUnsupportedReason::TooNew),
            supported: override_val,
        };
    }

    if major < CODEGRAPH_MIN_NODE_MAJOR {
        return CodegraphNodeSupport {
            major,
            r#override: override_val,
            reason: Some(CodegraphNodeUnsupportedReason::TooOld),
            supported: override_val,
        };
    }

    CodegraphNodeSupport {
        major,
        r#override: override_val,
        reason: None,
        supported: true,
    }
}

pub fn build_codegraph_node_skip_hint(support: &CodegraphNodeSupport) -> String {
    let detail = match support.reason {
        Some(CodegraphNodeUnsupportedReason::TooNew) => format!(
            "Node {} is unsupported (>= {CODEGRAPH_BLOCKED_NODE_MAJOR} crashes CodeGraph mid-indexing)",
            support.major
        ),
        _ => format!(
            "Node {} is too old (CodeGraph requires >= {CODEGRAPH_MIN_NODE_MAJOR})",
            support.major
        ),
    };
    format!(
        "CodeGraph MCP skipped: {detail}. Use Node {CODEGRAPH_MIN_NODE_MAJOR}-{} (e.g. Node 22 LTS) or set {CODEGRAPH_UNSAFE_NODE_ENV}=1 to override.\n",
        CODEGRAPH_BLOCKED_NODE_MAJOR - 1
    )
}
