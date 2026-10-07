use crate::internal::validate::{Node, enumeration};

pub const HARNESS_IDS: [&str; 3] = ["codex", "opencode", "omo"];

pub const OMO_CONFIG_HARNESS_IDS: [&str; 3] = ["opencode", "native", "codex"];

/// Retired harness ids and the canonical id that replaced them.
///
/// The standalone edition is branded OmO Native, but its harness id was minted from the engine's
/// package name before the edition had a brand of its own. `senpi` therefore stays accepted as the
/// legacy spelling of `native`: it is canonicalized when the config is read, and the startup
/// migration rewrites the file itself.
pub const OMO_CONFIG_LEGACY_HARNESS_ALIASES: [(&str, &str); 1] = [("senpi", "native")];

pub const OMO_CONFIG_LEGACY_HARNESS_IDS: [&str; 1] = ["senpi"];

pub fn omo_harness_id_schema() -> Node {
    enumeration(&OMO_CONFIG_HARNESS_IDS)
}

pub fn is_harness_id(value: &str) -> bool {
    HARNESS_IDS.contains(&value)
}

pub fn is_omo_config_harness_id(value: &str) -> bool {
    OMO_CONFIG_HARNESS_IDS.contains(&value)
}

pub fn is_legacy_harness_id(value: &str) -> bool {
    OMO_CONFIG_LEGACY_HARNESS_IDS.contains(&value)
}

pub fn canonical_harness_name(name: &str) -> String {
    match OMO_CONFIG_LEGACY_HARNESS_ALIASES
        .iter()
        .find(|(legacy, _)| *legacy == name)
    {
        Some((_, canonical)) => (*canonical).to_string(),
        None => name.to_string(),
    }
}

pub fn harness_block_key(harness: &str) -> String {
    format!("[{harness}]")
}
