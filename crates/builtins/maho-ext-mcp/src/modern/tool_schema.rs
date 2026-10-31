//! Port of senpi mcp/modern/tool-schema.ts: modern tool schemas are raw JSON Schema 2020-12.
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaErrorCode {
    UnsupportedDialect,
    Invalid,
    Limit,
    OutputSchemaMismatch,
}
impl SchemaErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedDialect => "MCP_SCHEMA_UNSUPPORTED_DIALECT",
            Self::Invalid => "MCP_SCHEMA_INVALID",
            Self::Limit => "MCP_SCHEMA_LIMIT",
            Self::OutputSchemaMismatch => "MCP_OUTPUT_SCHEMA_MISMATCH",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct McpToolSchemaError {
    pub code: SchemaErrorCode,
    pub message: String,
}
impl McpToolSchemaError {
    pub fn new(code: SchemaErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

const DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";
const TYPE_NAMES: [&str; 7] = [
    "array", "boolean", "integer", "null", "number", "object", "string",
];

/// A compiled modern schema. Validation runs on the shared JSON-Schema engine; compile performs the
/// checks ajv's compile step makes fatal: dialect, bounds, `$async`, keyword shapes and local `$ref`s.
#[derive(Clone, Debug)]
pub struct ModernSchemaValidator {
    schema: Value,
}
impl ModernSchemaValidator {
    pub fn validate(&self, value: &Value) -> bool {
        maho_ai::utils::validation::strict_errors(&self.schema, value).is_empty()
    }
    /// Ajv-shaped errors for pi-ai's protocol-owned tool-argument validation (no coercion, no null removal).
    pub fn errors(
        &self,
        value: &Value,
    ) -> Result<(), Vec<maho_ai::utils::tool_argument_validator::ToolArgumentValidationError>> {
        let issues = maho_ai::utils::validation::strict_issues(&self.schema, value);
        if issues.is_empty() {
            Ok(())
        } else {
            Err(issues)
        }
    }
}

/// No coercion/default insertion/removal, no network resolver, no shared $id namespace.
pub fn compile_modern_schema(schema: &Value) -> Result<ModernSchemaValidator, McpToolSchemaError> {
    check_bounds(schema)?;
    if let Some(declared) = schema.get("$schema")
        && !declared
            .as_str()
            .is_some_and(|declared| declared == DIALECT || declared == format!("{DIALECT}#"))
    {
        return Err(McpToolSchemaError::new(
            SchemaErrorCode::UnsupportedDialect,
            "Modern MCP supports JSON Schema 2020-12.",
        ));
    }
    if schema.get("$async").is_some() {
        return Err(McpToolSchemaError::new(
            SchemaErrorCode::Invalid,
            "Async schemas are unsupported.",
        ));
    }
    if !schema.is_boolean() && !schema.is_object() || !well_formed(schema, schema) {
        return Err(McpToolSchemaError::new(
            SchemaErrorCode::Invalid,
            "Invalid MCP schema or unresolved schema reference.",
        ));
    }
    Ok(ModernSchemaValidator {
        schema: schema.clone(),
    })
}

/// Reject one unusable definition, never its valid siblings; diagnostics contain no schema or arguments.
pub fn has_supported_modern_tool_schemas(tool: &Value) -> bool {
    let checked =
        compile_modern_schema(tool.get("inputSchema").unwrap_or(&Value::Null)).and_then(|_| {
            tool.get("outputSchema")
                .map_or(Ok(()), |output| compile_modern_schema(output).map(|_| ()))
        });
    match checked {
        Ok(()) => true,
        Err(error) => {
            let name = tool.get("name").cloned().unwrap_or(Value::Null);
            maho_ext_api::diagnostic(&format!(
                "MCP tool {name} skipped: {}: {}",
                error.code.as_str(),
                error.message
            ));
            false
        }
    }
}

fn check_bounds(schema: &Value) -> Result<(), McpToolSchemaError> {
    let mut pending = vec![(schema, 0usize)];
    let mut nodes = 0usize;
    while let Some((value, depth)) = pending.pop() {
        nodes += 1;
        if nodes > 16_384 || depth > 64 {
            return Err(McpToolSchemaError::new(
                SchemaErrorCode::Limit,
                "MCP schema exceeds the depth or node limit.",
            ));
        }
        match value {
            Value::Object(entries) => {
                pending.extend(entries.values().map(|child| (child, depth + 1)))
            }
            Value::Array(entries) => pending.extend(entries.iter().map(|child| (child, depth + 1))),
            _ => {}
        }
    }
    Ok(())
}

fn resolve_pointer<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    let pointer = reference.strip_prefix('#')?;
    if pointer.is_empty() {
        return Some(root);
    }
    let pointer = percent_decode(pointer)?;
    root.pointer(&pointer)
}
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

const SCHEMA_MAPS: [&str; 5] = [
    "properties",
    "patternProperties",
    "$defs",
    "definitions",
    "dependentSchemas",
];
const SCHEMA_LISTS: [&str; 4] = ["allOf", "anyOf", "oneOf", "prefixItems"];
const SCHEMA_VALUES: [&str; 11] = [
    "not",
    "if",
    "then",
    "else",
    "items",
    "contains",
    "additionalProperties",
    "propertyNames",
    "unevaluatedItems",
    "unevaluatedProperties",
    "additionalItems",
];

/// Keyword shapes ajv's meta-schema validation rejects at compile time, plus unresolved local references.
fn well_formed(root: &Value, schema: &Value) -> bool {
    let Some(entries) = schema.as_object() else {
        return schema.is_boolean();
    };
    if let Some(kind) = entries.get("type") {
        let valid = |name: &Value| name.as_str().is_some_and(|name| TYPE_NAMES.contains(&name));
        if !(valid(kind)
            || kind
                .as_array()
                .is_some_and(|names| !names.is_empty() && names.iter().all(valid)))
        {
            return false;
        }
    }
    if entries.get("required").is_some_and(|required| {
        !required
            .as_array()
            .is_some_and(|keys| keys.iter().all(Value::is_string))
    }) {
        return false;
    }
    if entries.get("enum").is_some_and(|values| !values.is_array()) {
        return false;
    }
    if let Some(reference) = entries.get("$ref") {
        // No network resolver and no shared $id namespace: only document-local references compile.
        let Some(target) = reference
            .as_str()
            .and_then(|reference| resolve_pointer(root, reference))
        else {
            return false;
        };
        if !target.is_object() && !target.is_boolean() {
            return false;
        }
    }
    for key in SCHEMA_MAPS {
        if let Some(map) = entries.get(key) {
            let Some(map) = map.as_object() else {
                return false;
            };
            if !map.values().all(|child| well_formed(root, child)) {
                return false;
            }
        }
    }
    for key in SCHEMA_LISTS {
        if let Some(list) = entries.get(key) {
            let Some(list) = list.as_array().filter(|list| !list.is_empty()) else {
                return false;
            };
            if !list.iter().all(|child| well_formed(root, child)) {
                return false;
            }
        }
    }
    SCHEMA_VALUES
        .iter()
        .filter_map(|key| entries.get(*key))
        .all(|child| well_formed(root, child))
}
