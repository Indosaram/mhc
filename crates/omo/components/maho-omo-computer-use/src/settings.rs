use senpi_desktop_tool::settings::{
    resolve_computer_settings, ComputerSettings, ComputerSettingsError,
};
use serde_json::{Map, Value};

pub use senpi_desktop_tool::settings::ComputerSettingsInput;

fn snake_to_camel(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize = false;
    for c in s.chars() {
        if c == '_' {
            capitalize = true;
        } else if capitalize {
            result.extend(c.to_uppercase());
            capitalize = false;
        } else {
            result.push(c);
        }
    }
    result
}

fn convert_keys_to_camel(val: &Value) -> Value {
    match val {
        Value::Object(map) => {
            let mut new_map = Map::new();
            for (k, v) in map {
                let camel_k = snake_to_camel(k);
                new_map.insert(camel_k, convert_keys_to_camel(v));
            }
            Value::Object(new_map)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(convert_keys_to_camel).collect()),
        other => other.clone(),
    }
}

pub fn resolve_omo_computer_settings(
    raw: Option<&Value>,
    platform: &str,
) -> Result<ComputerSettings, ComputerSettingsError> {
    let normalized = raw.map(convert_keys_to_camel);
    resolve_computer_settings(normalized.as_ref(), platform)
}
