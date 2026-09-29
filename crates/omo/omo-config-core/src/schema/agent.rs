use crate::internal::validate::{
    Node, ObjectSpec, PreprocessFn, array, boolean, enumeration, nonnegative_integer,
    number_between, optional, record, string, union,
};
use crate::schema::fallback_models::{
    normalize_legacy_model_fields, omo_fallback_model_object_schema, omo_reasoning_effort_schema,
};
use crate::schema::model_ref::omo_reasoning_schema;

pub fn omo_agent_model_entry_schema() -> Node {
    union(vec![string(), omo_fallback_model_object_schema()])
}

fn omo_agent_def_input_spec() -> ObjectSpec {
    ObjectSpec {
        fields: vec![
            optional("description", string()),
            optional("prompt", string()),
            optional("model", string()),
            optional("models", array(omo_agent_model_entry_schema())),
            optional("reasoning", omo_reasoning_schema()),
            optional("variant", string()),
            optional("reasoningEffort", omo_reasoning_effort_schema()),
            optional("tools", record(boolean())),
            optional("execution_mode", enumeration(&["in-process", "process"])),
            optional("background", boolean()),
            optional("max_depth", nonnegative_integer()),
            optional("allowed_subagents", array(string())),
            optional("disallowed_tools", array(string())),
            optional("max_turns", nonnegative_integer()),
            optional("temperature", number_between(0.0, 2.0)),
            optional("disable", boolean()),
        ],
        strict: true,
        preprocess: Some(normalize_legacy_model_fields as PreprocessFn),
        refine: None,
    }
}

pub fn omo_agent_def_schema() -> Node {
    Node::Object(Box::new(omo_agent_def_input_spec()))
}

pub fn omo_agents_config_schema() -> Node {
    record(omo_agent_def_schema())
}
