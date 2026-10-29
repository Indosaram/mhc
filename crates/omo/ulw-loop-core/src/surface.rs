//! Surface resolution and reviewer roles for ulw-loop.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::Path;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UlwLoopToolkitSurface {
    LazyCodex,
    #[serde(rename = "omo-senpi")]
    OmoSenpi,
}

impl UlwLoopToolkitSurface {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LazyCodex => "lazycodex",
            Self::OmoSenpi => "omo-senpi",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "lazycodex" => Some(Self::LazyCodex),
            "omo-senpi" => Some(Self::OmoSenpi),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UlwLoopReviewerRoles {
    pub code_review: &'static str,
    pub manual_qa: &'static str,
    pub gate_review: &'static str,
}

pub const LAZYCODEX_ROLES: UlwLoopReviewerRoles = UlwLoopReviewerRoles {
    code_review: "lazycodex-code-reviewer",
    manual_qa: "lazycodex-qa-executor",
    gate_review: "lazycodex-gate-reviewer",
};

pub const OMO_SENPI_ROLES: UlwLoopReviewerRoles = UlwLoopReviewerRoles {
    code_review: "omo-native-code-reviewer",
    manual_qa: "omo-native-qa-executor",
    gate_review: "omo-native-gate-reviewer",
};

pub fn reviewer_roles_for(surface: UlwLoopToolkitSurface) -> UlwLoopReviewerRoles {
    match surface {
        UlwLoopToolkitSurface::LazyCodex => LAZYCODEX_ROLES,
        UlwLoopToolkitSurface::OmoSenpi => OMO_SENPI_ROLES,
    }
}

pub fn all_reviewer_roles() -> [UlwLoopReviewerRoles; 2] {
    [LAZYCODEX_ROLES, OMO_SENPI_ROLES]
}

pub const LEGACY_REVIEWER_AGENT_ALIASES: [(&str, &str); 3] = [
    ("omo-senpi-code-reviewer", "omo-native-code-reviewer"),
    ("omo-senpi-qa-executor", "omo-native-qa-executor"),
    ("omo-senpi-gate-reviewer", "omo-native-gate-reviewer"),
];

pub fn canonical_reviewer_agent_name(reviewer: &str) -> &str {
    for (legacy, canonical) in LEGACY_REVIEWER_AGENT_ALIASES {
        if legacy == reviewer {
            return canonical;
        }
    }
    reviewer
}

pub const GATE_REVIEWER_AGENT_NAMES: [&str; 4] = [
    "lazycodex-gate-reviewer",
    "omo-native-gate-reviewer",
    "omo-senpi-gate-reviewer",
    "main-session",
];

pub fn resolve_toolkit_surface() -> UlwLoopToolkitSurface {
    resolve_toolkit_surface_with_env(|key| env::var(key).ok())
}

pub fn resolve_toolkit_surface_with_env(get_env: impl FnOnce(&str) -> Option<String>) -> UlwLoopToolkitSurface {
    if let Some(v) = get_env("OMO_AGENT_TOOLKIT_SURFACE") {
        if let Some(s) = UlwLoopToolkitSurface::parse(&v) {
            return s;
        }
    }
    UlwLoopToolkitSurface::LazyCodex
}
