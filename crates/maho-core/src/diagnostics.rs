//! Port of senpi packages/coding-agent/src/core/diagnostics.ts.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceCollision {
    pub resource_type: ResourceCollisionType,
    pub name: String,
    pub winner_path: String,
    pub loser_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loser_source: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceCollisionType {
    Extension,
    Skill,
    Prompt,
    Theme,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceDiagnostic {
    #[serde(rename = "type")]
    pub diagnostic_type: ResourceDiagnosticType,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collision: Option<ResourceCollision>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceDiagnosticType {
    Warning,
    Error,
    Collision,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_a_collision_with_camel_case_fields() {
        let collision = ResourceCollision {
            resource_type: ResourceCollisionType::Extension,
            name: "demo".to_owned(),
            winner_path: "/a".to_owned(),
            loser_path: "/b".to_owned(),
            winner_source: Some("npm:foo".to_owned()),
            loser_source: None,
        };
        let json = serde_json::to_value(&collision).expect("json");
        assert_eq!(json["resourceType"], "extension");
        assert_eq!(json["winnerPath"], "/a");
        assert!(json.get("loserSource").is_none());
        let diagnostic = ResourceDiagnostic {
            diagnostic_type: ResourceDiagnosticType::Collision,
            message: "collision".to_owned(),
            path: None,
            collision: Some(collision),
        };
        let json = serde_json::to_value(&diagnostic).expect("json");
        assert_eq!(json["type"], "collision");
        assert_eq!(json["collision"]["name"], "demo");
    }
}
