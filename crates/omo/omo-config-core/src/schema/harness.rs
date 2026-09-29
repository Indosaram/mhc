use crate::internal::validate::{Node, enumeration};

pub const HARNESS_IDS: [&str; 3] = ["codex", "opencode", "omo"];

pub const OMO_CONFIG_HARNESS_IDS: [&str; 3] = ["opencode", "senpi", "codex"];

pub fn omo_harness_id_schema() -> Node {
    enumeration(&OMO_CONFIG_HARNESS_IDS)
}

pub fn is_harness_id(value: &str) -> bool {
    HARNESS_IDS.contains(&value)
}

pub fn is_omo_config_harness_id(value: &str) -> bool {
    OMO_CONFIG_HARNESS_IDS.contains(&value)
}
