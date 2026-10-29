use serde_json::json;

use crate::internal::validate::{
    Node, boolean, defaulted, enumeration, optional, positive_integer, record, strict_object,
};

fn mode_schema() -> Node {
    enumeration(&["off", "best-effort", "required"])
}

fn languages_schema() -> Node {
    record(boolean())
}

pub fn omo_format_on_mutation_layer_schema() -> Node {
    strict_object(vec![
        optional("mode", mode_schema()),
        optional("languages", languages_schema()),
        optional("maxFileBytes", positive_integer()),
        optional("timeoutMs", positive_integer()),
    ])
}

pub fn omo_format_on_mutation_schema() -> Node {
    strict_object(vec![
        defaulted("mode", mode_schema(), || json!("best-effort")),
        optional("languages", languages_schema()),
        defaulted("maxFileBytes", positive_integer(), || json!(1_048_576)),
        defaulted("timeoutMs", positive_integer(), || json!(3_000)),
    ])
}
