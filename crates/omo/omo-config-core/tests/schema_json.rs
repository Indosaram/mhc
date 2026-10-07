use std::collections::BTreeSet;

use omo_config_core::internal::validate::Node;
use omo_config_core::{object_property_keys, omo_config_json_schema};
use serde_json::{Map, Value};

fn document() -> Value {
    omo_config_json_schema()
}

fn property_schema<'a>(document: &'a Value, path: &[&str]) -> &'a Value {
    let mut current = document;
    for segment in path {
        current = if *segment == "additionalProperties" {
            current
                .get("additionalProperties")
                .unwrap_or_else(|| panic!("missing additionalProperties in {path:?}"))
        } else {
            current
                .get("properties")
                .and_then(|properties| properties.get(*segment))
                .unwrap_or_else(|| panic!("missing property {segment} in {path:?}"))
        };
    }
    current
}

fn keys(value: &Value) -> BTreeSet<String> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("expected an object, got {value}"))
        .keys()
        .cloned()
        .collect()
}

fn schema_keys(node: &Node) -> BTreeSet<String> {
    object_property_keys(node)
        .unwrap_or_else(|| panic!("expected an object node"))
        .into_iter()
        .collect()
}

fn assert_properties_match(path: &[&str], node: &Node) {
    let document = document();
    let properties = if path.is_empty() {
        document
            .get("properties")
            .expect("root properties")
            .clone()
    } else {
        property_schema(&document, path)
            .get("properties")
            .unwrap_or_else(|| panic!("{path:?} has no properties"))
            .clone()
    };
    assert_eq!(
        keys(&properties),
        schema_keys(node),
        "published schema properties must equal the schema tree fields at {path:?}"
    );
}

#[test]
fn published_schema_identifies_itself_as_the_omo_document() {
    let document = document();
    assert_eq!(document["$schema"], "http://json-schema.org/draft-07/schema#");
    assert_eq!(
        document["$id"],
        "https://raw.githubusercontent.com/code-yeongyu/oh-my-openagent/dev/assets/omo.schema.json"
    );
    assert_eq!(document["title"], "OmO Configuration");
    assert_eq!(document["type"], "object");
}

#[test]
fn published_root_properties_equal_the_root_schema_fields() {
    assert_properties_match(&[], &omo_config_core::omo_config_schema());
}

#[test]
fn published_layer_profile_and_harness_properties_equal_their_schema_fields() {
    assert_properties_match(
        &["profiles", "additionalProperties"],
        &omo_config_core::omo_config_profile_schema(),
    );
    assert_properties_match(
        &["[native]"],
        &omo_config_core::omo_typed_harness_config_schema(),
    );
    assert_properties_match(
        &["[senpi]"],
        &omo_config_core::omo_typed_harness_config_schema(),
    );
    assert_properties_match(
        &["[codex]"],
        &omo_config_core::omo_typed_harness_config_schema(),
    );
    let layer = omo_config_core::omo_config_layer_schema();
    assert_eq!(
        schema_keys(&layer),
        schema_keys(&omo_config_core::omo_config_schema()),
        "the layer shape mirrors the root shape"
    );
}

#[test]
fn published_section_properties_equal_their_schema_fields() {
    assert_properties_match(&["computer"], &omo_config_core::omo_computer_settings_schema());
    assert_properties_match(&["git_master"], &omo_config_core::omo_git_master_settings_schema());
    assert_properties_match(
        &["formatOnMutation"],
        &omo_config_core::omo_format_on_mutation_schema(),
    );
    assert_properties_match(&["memory"], &omo_config_core::omo_memory_settings_schema());
    assert_properties_match(
        &["memory", "recall"],
        &omo_config_core::omo_memory_recall_schema(),
    );
    assert_properties_match(
        &["memory", "recall", "event_caps"],
        &omo_config_core::omo_memory_recall_event_caps_schema(),
    );
    assert_properties_match(
        &["memory", "write_notice"],
        &omo_config_core::omo_memory_write_notice_schema(),
    );
    assert_properties_match(
        &["model_profiles", "additionalProperties"],
        &omo_config_core::omo_model_profile_schema(),
    );
}

#[test]
fn published_schema_marks_every_defaulted_property_optional() {
    fn walk(value: &Value, path: &str) {
        match value {
            Value::Object(map) => {
                if let Some(Value::Object(properties)) = map.get("properties") {
                    let required: BTreeSet<String> = match map.get("required") {
                        Some(Value::Array(entries)) => entries
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect(),
                        _ => BTreeSet::new(),
                    };
                    for (key, property) in properties {
                        if property.get("default").is_some_and(|value| !value.is_null()) {
                            assert!(
                                !required.contains(key),
                                "{path}.{key} carries a default and must not be required"
                            );
                        }
                        walk(property, &format!("{path}.{key}"));
                    }
                }
                for (key, entry) in map {
                    if key != "properties" {
                        walk(entry, &format!("{path}.{key}"));
                    }
                }
            }
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    walk(item, &format!("{path}[{index}]"));
                }
            }
            _ => {}
        }
    }
    walk(&document(), "$");
}

#[test]
fn published_schema_carries_the_documented_defaults() {
    let document = document();
    assert_eq!(
        property_schema(&document, &["formatOnMutation"])["properties"]["mode"]["default"],
        "best-effort"
    );
    assert_eq!(
        property_schema(&document, &["formatOnMutation"])["properties"]["maxFileBytes"]["default"],
        1_048_576
    );
    assert_eq!(
        property_schema(&document, &["git_master"])["properties"]["commit_footer"]["default"],
        false
    );
    assert_eq!(
        property_schema(&document, &["memory"])["properties"]["recall"]["default"]["max_items"],
        2
    );
    assert_eq!(
        property_schema(&document, &["memory"])["properties"]["write_notice"]["default"]["enabled"],
        true
    );
    assert_eq!(
        property_schema(&document, &["computer"])["additionalProperties"],
        false,
        "a strict object rejects unknown keys"
    );
    let root = document.get("properties").expect("root properties");
    let root: &Map<String, Value> = root.as_object().expect("object");
    assert!(
        !root.contains_key("codegraph"),
        "the retired codegraph key is absent from the published schema"
    );
}
