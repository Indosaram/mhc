use crate::internal::validate::{
    Node, ObjectSpec, PreprocessFn, array, enumeration, optional, record, string, union,
};
use crate::schema::fallback_models::{
    normalize_legacy_model_fields, omo_fallback_model_object_schema,
};

/// A model profile is a named, ordered model chain a human picks by lane
/// (Daily / Geeky x Normal / Heavy) instead of by model id. It is NOT the `profiles`
/// key: that one is a VSCode-style config-layer overlay activated by `OMO_PROFILE`.
///
/// `models` is optional on purpose. A layer that fails validation is rejected wholesale,
/// so requiring at least one entry would make a label-only override of a builtin profile drop the
/// user's whole `categories`/`agents`/`teams` layer. "No models after merging the builtins" is a
/// runtime report, not a schema error. `family` / `tier` are optional metadata a user overlay may
/// set; they do not create an editor.
fn model_profile_input_spec() -> ObjectSpec {
    ObjectSpec {
        fields: vec![
            optional("display_name", string()),
            optional("family", enumeration(&["daily", "geeky"])),
            optional("tier", enumeration(&["normal", "heavy"])),
            optional(
                "models",
                array(union(vec![string(), omo_fallback_model_object_schema()])),
            ),
        ],
        strict: true,
        preprocess: Some(normalize_legacy_model_fields as PreprocessFn),
        refine: None,
    }
}

pub fn omo_model_profile_schema() -> Node {
    Node::Object(Box::new(model_profile_input_spec()))
}

pub fn omo_model_profiles_schema() -> Node {
    record(omo_model_profile_schema())
}

pub fn omo_model_profile_layer_schema() -> Node {
    Node::Object(Box::new(model_profile_input_spec()))
}

pub fn omo_model_profiles_layer_schema() -> Node {
    record(omo_model_profile_layer_schema())
}
