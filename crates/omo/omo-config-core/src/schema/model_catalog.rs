use crate::internal::validate::{
    Node, ObjectSpec, PreprocessFn, optional, record, required, string,
};
use crate::schema::fallback_models::{normalize_legacy_model_fields, omo_reasoning_effort_schema};
use crate::schema::model_ref::omo_reasoning_schema;

fn omo_model_catalog_entry_input_spec() -> ObjectSpec {
    ObjectSpec {
        fields: vec![
            required("model", string()),
            optional("reasoning", omo_reasoning_schema()),
            optional("variant", string()),
            optional("reasoningEffort", omo_reasoning_effort_schema()),
        ],
        strict: true,
        preprocess: Some(normalize_legacy_model_fields as PreprocessFn),
        refine: None,
    }
}

pub fn omo_model_catalog_entry_schema() -> Node {
    Node::Object(Box::new(omo_model_catalog_entry_input_spec()))
}

pub fn omo_model_catalog_schema() -> Node {
    record(omo_model_catalog_entry_schema())
}

pub fn omo_model_catalog_entry_layer_schema() -> Node {
    Node::Object(Box::new(omo_model_catalog_entry_input_spec().partial()))
}

pub fn omo_model_catalog_layer_schema() -> Node {
    record(omo_model_catalog_entry_layer_schema())
}
