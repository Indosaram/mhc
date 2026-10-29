use crate::internal::validate::{
    Node, any, enumeration, number_between, optional, positive_integer, record, required,
    strict_object, string, union,
};
pub const REASONING_LEVELS_OR_AUTO: [&str; 8] = [
    "off", "minimal", "low", "medium", "high", "xhigh", "max", "auto",
];

pub fn omo_reasoning_schema() -> Node {
    union(vec![enumeration(&REASONING_LEVELS_OR_AUTO), string()])
}

pub fn omo_model_ref_object_schema() -> Node {
    strict_object(vec![
        required("model", string()),
        optional("reasoning", omo_reasoning_schema()),
        optional("temperature", number_between(0.0, 2.0)),
        optional("top_p", number_between(0.0, 1.0)),
        optional("max_tokens", positive_integer()),
        optional("provider_options", record(any())),
    ])
}

pub fn omo_model_ref_schema() -> Node {
    union(vec![string(), omo_model_ref_object_schema()])
}
