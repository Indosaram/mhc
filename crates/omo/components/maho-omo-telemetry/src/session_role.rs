//! Port of `packages/omo-senpi/src/components/telemetry/session-role.ts`.
//!
//! Kept free of imports: the schema generator and the event schemas load this vocabulary, and must not pull
//! the task engine in with it. The reader that maps a live session onto it is `session_role_resolver`.

pub const TELEMETRY_SESSION_ROLES: [&str; 3] = ["root", "child", "unknown"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TelemetrySessionRole {
    Root,
    #[default]
    Child,
    Unknown,
}

impl TelemetrySessionRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Child => "child",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for TelemetrySessionRole {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A closed role parser: anything outside the vocabulary reads as `unknown`.
pub fn parse_telemetry_session_role(value: Option<&str>) -> TelemetrySessionRole {
    match value {
        Some("root") => TelemetrySessionRole::Root,
        Some("child") => TelemetrySessionRole::Child,
        _ => TelemetrySessionRole::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_vocabulary_defaults_to_unknown() {
        assert_eq!(TELEMETRY_SESSION_ROLES, ["root", "child", "unknown"]);
        assert_eq!(
            parse_telemetry_session_role(Some("root")),
            TelemetrySessionRole::Root
        );
        assert_eq!(
            parse_telemetry_session_role(Some("child")),
            TelemetrySessionRole::Child
        );
        for value in [None, Some(""), Some("main"), Some("dag_child"), Some("member")] {
            assert_eq!(
                parse_telemetry_session_role(value),
                TelemetrySessionRole::Unknown
            );
        }
    }

    #[test]
    fn wire_names_match_the_vocabulary() {
        for role in [
            TelemetrySessionRole::Root,
            TelemetrySessionRole::Child,
            TelemetrySessionRole::Unknown,
        ] {
            assert!(TELEMETRY_SESSION_ROLES.contains(&role.as_str()));
        }
    }
}
