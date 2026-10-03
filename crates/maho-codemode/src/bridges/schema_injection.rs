pub fn inject_schema_instruction(prompt: &str, schema: &serde_json::Value) -> String {
    format!("{prompt}\n\nRespond ONLY with JSON matching this JSON-Schema:\n{schema}")
}
