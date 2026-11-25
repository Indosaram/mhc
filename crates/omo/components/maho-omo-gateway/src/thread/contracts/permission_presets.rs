//! Permission preset lattice (`thread/contracts/permission-presets.ts`).

use serde::{Deserialize, Serialize};

pub const PERMISSION_PRESETS: &[&str] = &[
    "full-access",
    "workspace",
    "accept-edits",
    "auto",
    "read-only",
    "ask",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionPreset {
    FullAccess,
    Workspace,
    AcceptEdits,
    Auto,
    ReadOnly,
    Ask,
}

impl PermissionPreset {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "full-access" => Some(Self::FullAccess),
            "workspace" => Some(Self::Workspace),
            "accept-edits" => Some(Self::AcceptEdits),
            "auto" => Some(Self::Auto),
            "read-only" => Some(Self::ReadOnly),
            "ask" => Some(Self::Ask),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::FullAccess => "full-access",
            Self::Workspace => "workspace",
            Self::AcceptEdits => "accept-edits",
            Self::Auto => "auto",
            Self::ReadOnly => "read-only",
            Self::Ask => "ask",
        }
    }
}

pub fn is_permission_preset(value: &str) -> bool {
    PermissionPreset::parse(value).is_some()
}

/// Whether a session in `creator`'s mode may start a thread in `requested`.
/// A creator whose mode is unknown may only ask for `ask`.
pub fn preset_within(requested: PermissionPreset, creator: Option<&str>) -> bool {
    let Some(raw) = creator else {
        return requested == PermissionPreset::Ask;
    };
    let Some(creator_preset) = PermissionPreset::parse(raw) else {
        return requested == PermissionPreset::Ask;
    };
    match creator_preset {
        PermissionPreset::FullAccess => true,
        PermissionPreset::Workspace => matches!(
            requested,
            PermissionPreset::Workspace
                | PermissionPreset::AcceptEdits
                | PermissionPreset::ReadOnly
                | PermissionPreset::Ask
        ),
        PermissionPreset::AcceptEdits => matches!(
            requested,
            PermissionPreset::AcceptEdits | PermissionPreset::ReadOnly | PermissionPreset::Ask
        ),
        PermissionPreset::ReadOnly => {
            matches!(
                requested,
                PermissionPreset::ReadOnly | PermissionPreset::Ask
            )
        }
        PermissionPreset::Auto => {
            matches!(requested, PermissionPreset::Auto | PermissionPreset::Ask)
        }
        PermissionPreset::Ask => requested == PermissionPreset::Ask,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforces_permission_preset_lattice() {
        assert!(preset_within(PermissionPreset::ReadOnly, Some("workspace")));
        assert!(!preset_within(PermissionPreset::Auto, Some("workspace")));
        assert!(!preset_within(PermissionPreset::ReadOnly, Some("auto")));
        assert!(preset_within(PermissionPreset::Ask, None));
        assert!(!preset_within(PermissionPreset::ReadOnly, None));
    }
}
