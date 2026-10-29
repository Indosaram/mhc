use crate::internal::validate::{
    Node, ObjectSpec, PreprocessFn, any, array, boolean, enumeration, number, optional,
    positive_integer, record, string, union,
};
use crate::schema::fallback_models::{
    normalize_legacy_model_fields, omo_fallback_model_object_schema, omo_fallback_models_schema,
    omo_reasoning_effort_schema, omo_thinking_config_schema,
};
use crate::schema::model_ref::omo_reasoning_schema;

pub fn omo_category_config_object_spec() -> ObjectSpec {
    ObjectSpec {
        fields: vec![
            optional("description", string()),
            optional("model", string()),
            optional(
                "models",
                array(union(vec![string(), omo_fallback_model_object_schema()])),
            ),
            optional("reasoning", omo_reasoning_schema()),
            optional(
                "temperature",
                crate::internal::validate::number_between(0.0, 2.0),
            ),
            optional("top_p", crate::internal::validate::number_between(0.0, 1.0)),
            optional("max_tokens", positive_integer()),
            optional("provider_options", record(any())),
            optional("fallback_models", omo_fallback_models_schema()),
            optional("variant", string()),
            optional("maxTokens", number()),
            optional("thinking", omo_thinking_config_schema()),
            optional("reasoningEffort", omo_reasoning_effort_schema()),
            optional("textVerbosity", enumeration(&["low", "medium", "high"])),
            optional("tools", record(boolean())),
            optional("prompt_append", string()),
            optional("max_prompt_tokens", positive_integer()),
            optional("is_unstable_agent", boolean()),
            optional("disable", boolean()),
            optional("warn_unavailable", boolean()),
        ],
        strict: true,
        preprocess: Some(normalize_legacy_model_fields as PreprocessFn),
        refine: None,
    }
}

pub fn omo_category_config_object_schema() -> Node {
    Node::Object(Box::new(omo_category_config_object_spec()))
}

pub fn omo_category_config_schema() -> Node {
    omo_category_config_object_schema()
}

pub fn omo_category_config_layer_schema() -> Node {
    Node::Object(Box::new(omo_category_config_object_spec().partial()))
}

pub fn omo_categories_config_schema() -> Node {
    record(omo_category_config_schema())
}

pub fn omo_categories_config_layer_schema() -> Node {
    record(omo_category_config_layer_schema())
}
