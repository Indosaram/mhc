use super::protocol::{error, integer, nonempty, optional, string, strings};
use serde_json::Value;

fn tool_error(value: &Value) -> bool {
    error(value) && optional(value, "details", |details| {
        details.is_object() && nonempty(details, "tool") && nonempty(details, "call_id")
            && matches!(details.get("reason").and_then(Value::as_str), Some("allow" | "deny"))
    })
}

pub fn valid_invoke_scope(value: &Value) -> bool {
    value.is_object() && optional(value, "tools", |tools| tools.is_object()
        && optional(tools, "allow", strings) && optional(tools, "deny", strings))
}

pub fn valid_descriptor(value: &Value) -> bool {
    value.is_object() && nonempty(value, "name") && string(value, "description") && value.get("input_schema").is_some()
        && matches!(value.get("language").and_then(Value::as_str), Some("js" | "py" | "rb" | "jl"))
        && value.get("kernel_generation").is_some_and(|v| integer(v, 0))
        && value.get("definition_revision").is_some_and(|v| integer(v, 1))
}

pub fn valid_host_message(value: &Value) -> bool {
    match value.get("type").and_then(Value::as_str) {
        Some("kernel-tool-describe") => nonempty(value, "requestId") && value.get("names").is_some_and(strings),
        Some("kernel-tool-invoke") => nonempty(value, "requestId") && nonempty(value, "name")
            && value.get("kernel_generation").is_some_and(|v| integer(v, 0))
            && value.get("definition_revision").is_some_and(|v| integer(v, 1)) && value.get("args").is_some()
            && nonempty(value, "call_id") && optional(value, "scope", valid_invoke_scope),
        Some("kernel-tool-cancel") => nonempty(value, "requestId"),
        Some("kernel-tools-names") => value.get("hostToolNames").is_some_and(strings) && value.get("foreignLanguageNames").is_some_and(strings),
        _ => false,
    }
}

pub fn valid_kernel_message(value: &Value) -> bool {
    if !nonempty(value, "requestId") { return false; }
    match (value.get("type").and_then(Value::as_str), value.get("ok").and_then(Value::as_bool)) {
        (Some("kernel-tool-describe-reply"), Some(false)) => value.get("error").is_some_and(tool_error),
        (Some("kernel-tool-invoke-reply"), Some(true)) => value.get("value").is_some() && optional(value, "output", Value::is_string),
        (Some("kernel-tool-invoke-reply"), Some(false)) => value.get("error").is_some_and(tool_error) && optional(value, "output", Value::is_string),
        (Some("kernel-tool-describe-reply"), Some(true)) => value.get("results").and_then(Value::as_array).is_some_and(|results| results.iter().all(|result| {
            nonempty(result, "name") && match result.get("ok").and_then(Value::as_bool) {
                Some(true) => result.get("descriptor").is_some_and(valid_descriptor),
                Some(false) => result.get("error").is_some_and(tool_error), None => false,
            }
        })),
        (Some("kernel-tools-defined"), None) => value.get("names").is_some_and(strings),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_non_javascript_descriptors_and_python_call_output() {
        let descriptor = json!({"name":"plot","description":"plot","input_schema":{},"language":"py","kernel_generation":0,"definition_revision":1});
        assert!(valid_descriptor(&descriptor));
        assert!(valid_kernel_message(&json!({"type":"kernel-tool-invoke-reply","requestId":"r","ok":true,"value":null,"output":"printed"})));
        assert!(valid_kernel_message(&json!({"type":"kernel-tools-defined","names":["plot"]})));
    }
}
