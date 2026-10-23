use serde_json::{Value, json};

use crate::internal::validate::{
    Node, boolean, defaulted, integer_between, optional, strict_object,
};

pub const OMO_MEMORY_AGREEMENTS_DEFAULT_ENABLED: bool = true;
pub const OMO_MEMORY_AGREEMENTS_DEFAULT_NEEDS_HUMAN_THRESHOLD: i64 = 3;
pub const OMO_MEMORY_AGREEMENTS_DEFAULT_MAX_ITEMS: i64 = 64;

pub fn omo_memory_agreements_defaults() -> Value {
    json!({
        "enabled": OMO_MEMORY_AGREEMENTS_DEFAULT_ENABLED,
        "needs_human_threshold": OMO_MEMORY_AGREEMENTS_DEFAULT_NEEDS_HUMAN_THRESHOLD,
        "max_items": OMO_MEMORY_AGREEMENTS_DEFAULT_MAX_ITEMS,
    })
}

pub fn omo_memory_agreements_schema() -> Node {
    strict_object(vec![
        defaulted("enabled", boolean(), || {
            json!(OMO_MEMORY_AGREEMENTS_DEFAULT_ENABLED)
        }),
        defaulted(
            "needs_human_threshold",
            integer_between(1, 9_007_199_254_740_991),
            || json!(OMO_MEMORY_AGREEMENTS_DEFAULT_NEEDS_HUMAN_THRESHOLD),
        ),
        defaulted("max_items", integer_between(8, 256), || {
            json!(OMO_MEMORY_AGREEMENTS_DEFAULT_MAX_ITEMS)
        }),
    ])
}

pub fn omo_memory_agreements_layer_schema() -> Node {
    strict_object(vec![
        optional("enabled", boolean()),
        optional(
            "needs_human_threshold",
            integer_between(1, 9_007_199_254_740_991),
        ),
        optional("max_items", integer_between(8, 256)),
    ])
}
