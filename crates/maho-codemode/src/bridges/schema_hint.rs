use serde_json::Value;

const HEADER: &str = "Expected parameters:";

pub fn append_schema_hint(message: &str, tool_name: &str, schema: &Value) -> String {
    match render_schema_hint(tool_name, schema) {
        Some(hint) => format!("{message}\n\n{HEADER}\n{hint}"),
        None => message.into(),
    }
}

pub fn render_schema_hint(tool_name: &str, schema: &Value) -> Option<String> {
    if !schema.is_object() { return None; }
    let lines = object_lines(schema, 0, 1);
    if lines.is_empty() { return None; }
    let marker = format!("[truncated; call tool_schema({}) for the full schema]", Value::String(tool_name.into()));
    let budget = 1200 - HEADER.len() - 1;
    let mut kept = Vec::new();
    let mut used = 0;
    for line in lines {
        let next = used + line.encode_utf16().count() + usize::from(!kept.is_empty());
        if next > budget - marker.encode_utf16().count() - 1 {
            kept.push(marker);
            return Some(kept.join("\n"));
        }
        used = next;
        kept.push(line);
    }
    Some(kept.join("\n"))
}

fn object_lines(schema: &Value, indent: usize, depth: usize) -> Vec<String> {
    let required = schema["required"].as_array().map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>()).unwrap_or_default();
    let entries = schema["properties"].as_object().map(|values| values.iter().collect::<Vec<_>>()).unwrap_or_default();
    if entries.is_empty() && required.is_empty() { return Vec::new(); }
    let ordered = entries.iter().filter(|(name, _)| required.contains(&name.as_str()))
        .chain(entries.iter().filter(|(name, _)| !required.contains(&name.as_str()))).collect::<Vec<_>>();
    let pad = "  ".repeat(indent + 1);
    let mut lines = Vec::new();
    if indent == 0 && !required.is_empty() { lines.push(format!("required: {}", required.join(", "))); }
    for (name, value) in ordered.iter().take(30).copied() {
        let optional = if required.contains(&name.as_str()) { "" } else { "?" };
        let mut description = type_label(value);
        if let Some(text) = value["description"].as_str().and_then(|text| text.split('\n').next()).map(str::trim).filter(|text| !text.is_empty()) {
            let truncated = if text.encode_utf16().count() > 80 {
                let units = text.encode_utf16().take(80).collect::<Vec<_>>();
                format!("{}…", String::from_utf16_lossy(&units))
            } else { text.into() };
            description.push_str(&format!(" — {truncated}"));
        }
        lines.push(format!("{pad}{name}{optional}: {description}"));
        if depth < 2 && value.is_object() {
            let nested = if value["items"].is_object() { &value["items"] } else { value };
            if nested["properties"].as_object().is_some_and(|properties| !properties.is_empty()) {
                lines.extend(object_lines(nested, indent + 1, depth + 1));
            }
        }
    }
    let hidden = ordered.len().saturating_sub(30);
    if hidden > 0 { lines.push(format!("{pad}… {hidden} more propert{}", if hidden == 1 { "y" } else { "ies" })); }
    lines
}

fn type_label(schema: &Value) -> String {
    if !schema.is_object() { return "unknown".into(); }
    if let Some(value) = schema.get("const") { return value.to_string(); }
    if let Some(values) = schema["enum"].as_array().filter(|values| !values.is_empty()) { return union_label(values.iter().map(Value::to_string).collect()); }
    let branches = schema.get("oneOf").filter(|value| !value.is_null()).unwrap_or(&schema["anyOf"]);
    if let Some(values) = branches.as_array().filter(|values| !values.is_empty()) { return union_label(values.iter().map(type_label).collect()); }
    if let Some(values) = schema["allOf"].as_array().filter(|values| !values.is_empty()) { return type_label(&values[0]); }
    let base = match &schema["type"] {
        Value::String(value) => value.clone(),
        Value::Array(values) => values.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("|"),
        _ => "unknown".into(),
    };
    if base == "array" && schema["items"].is_object() { format!("array<{}>", type_label(&schema["items"])) } else { base }
}

fn union_label(values: Vec<String>) -> String {
    let shown = values.iter().take(8).cloned().collect::<Vec<_>>().join(" | ");
    if values.len() > 8 { format!("{shown} | … ({} more)", values.len() - 8) } else { shown }
}
