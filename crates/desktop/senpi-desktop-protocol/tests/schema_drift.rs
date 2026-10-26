use serde_json::Value;

use senpi_desktop_protocol::{
    ENGINE_ABI, ENGINE_METHODS, ENGINE_NOTIFICATIONS, ERROR_CODES, PROTOCOL_VERSION,
};

fn load_engine_schema() -> Value {
    let schema_str = include_str!("../../senpi-desktop-core/schema/engine.schema.json");
    serde_json::from_str(schema_str).expect("Valid engine.schema.json")
}

#[test]
fn keeps_protocol_version_equal_to_the_committed_schema() {
    let schema = load_engine_schema();
    assert_eq!(PROTOCOL_VERSION, schema["protocolVersion"].as_str().unwrap());
}

#[test]
fn keeps_engine_abi_equal_to_the_committed_schema() {
    let schema = load_engine_schema();
    assert_eq!(ENGINE_ABI, schema["abi"].as_str().unwrap());
}

#[test]
fn keeps_error_codes_equal_to_schema_errors_and_definitions_error_code() {
    let schema = load_engine_schema();
    let errors_obj = schema["errors"].as_object().unwrap();
    let mut from_errors: Vec<String> = errors_obj.keys().cloned().collect();
    from_errors.sort();

    let def_one_of = schema["definitions"]["ErrorCode"]["oneOf"].as_array().unwrap();
    let mut from_definition = Vec::new();
    for branch in def_one_of {
        if let Some(c) = branch.get("const").and_then(|v| v.as_str()) {
            from_definition.push(c.to_string());
        } else if let Some(arr) = branch.get("enum").and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(s) = item.as_str() {
                    from_definition.push(s.to_string());
                }
            }
        }
    }
    from_definition.sort();

    let mut exported: Vec<String> = ERROR_CODES.iter().map(|s| (*s).to_string()).collect();
    exported.sort();

    assert_eq!(exported, from_errors);
    assert_eq!(exported, from_definition);
}

#[test]
fn keeps_engine_methods_equal_to_the_schema_method_list() {
    let schema = load_engine_schema();
    let methods_obj = schema["methods"].as_object().unwrap();
    let mut from_methods: Vec<String> = methods_obj.keys().cloned().collect();
    from_methods.sort();

    let def_enum = schema["definitions"]["Method"]["enum"].as_array().unwrap();
    let mut from_enum: Vec<String> = def_enum.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
    from_enum.sort();

    let mut exported: Vec<String> = ENGINE_METHODS.iter().map(|s| (*s).to_string()).collect();
    exported.sort();

    assert_eq!(exported, from_methods);
    assert_eq!(exported, from_enum);
}

#[test]
fn keeps_engine_notifications_equal_to_the_schema_notification_list() {
    let schema = load_engine_schema();
    let notifs_obj = schema["notifications"].as_object().unwrap();
    let mut from_notifs: Vec<String> = notifs_obj.keys().cloned().collect();
    from_notifs.sort();

    let mut exported: Vec<String> = ENGINE_NOTIFICATIONS.iter().map(|s| (*s).to_string()).collect();
    exported.sort();

    assert_eq!(exported, from_notifs);
}
