use crate::internal::validate::{Node, ObjectSpec, array, enumeration, optional};

const EXPANSIONS: &[&str] = &["ultrawork", "team", "hyperplan", "hyperplan-ultrawork"];

/// Keyword expansion allowlist and denylist shared by the OmO harnesses.
pub fn omo_keyword_detector_settings_schema() -> Node {
    Node::Object(Box::new(ObjectSpec {
        fields: vec![
            optional("enabled_expansions", array(enumeration(EXPANSIONS))),
            optional("disabled_keywords", array(enumeration(EXPANSIONS))),
        ],
        ..ObjectSpec::default()
    }))
}
