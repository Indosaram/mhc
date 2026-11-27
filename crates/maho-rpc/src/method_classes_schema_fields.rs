//! The bounded field schemas every inbound RPC record schema is built from (senpi#4324): ids, paths,
//! text and secrets by length, counts as non-negative integers, lists and free-form records by size.
//! The host's own limits are reused where it has them; the rest are named in [`RPC_INBOUND_LIMITS`].

use crate::rpc_input_validation::{MAX_RPC_MESSAGE_CHARACTERS, SESSION_CONTEXT_LIMITS};
use serde_json::Value;

/// Bounds on a question answer map (`extension_ui_response`, `extension_ui_progress`) from a validated caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuestionAnswerLimits {
    pub answers: usize,
    pub selected_per_answer: usize,
    pub label_characters: usize,
}

pub const RPC_QUESTION_ANSWER_LIMITS: QuestionAnswerLimits = QuestionAnswerLimits {
    answers: 64,
    selected_per_answer: 64,
    label_characters: MAX_RPC_MESSAGE_CHARACTERS,
};

/// Bounds every field of a validated caller's record stays inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RpcInboundLimits {
    pub identifier_characters: usize,
    pub path_characters: usize,
    pub text_characters: usize,
    pub secret_characters: usize,
    pub list_items: usize,
    pub images: usize,
    pub record_keys: usize,
    pub count: i64,
    pub env_entries: usize,
    pub env_value_characters: usize,
}

pub const RPC_INBOUND_LIMITS: RpcInboundLimits = RpcInboundLimits {
    identifier_characters: 512,
    path_characters: 4096,
    text_characters: MAX_RPC_MESSAGE_CHARACTERS,
    secret_characters: 16_384,
    list_items: 1024,
    images: 64,
    record_keys: 256,
    count: 9_007_199_254_740_991,
    env_entries: 16,
    env_value_characters: 4096,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaFailure {
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct FieldSchema {
    pub name: &'static str,
    pub schema: Schema,
    pub optional: bool,
}

#[derive(Debug, Clone)]
pub enum Schema {
    String {
        max_len: Option<usize>,
        env_name: bool,
    },
    Integer {
        min: Option<i64>,
        max: Option<i64>,
    },
    Number,
    Boolean,
    Null,
    LiteralStr(&'static str),
    LiteralBool(bool),
    Never,
    Unknown,
    Array {
        item: Box<Schema>,
        max_items: Option<usize>,
    },
    Record {
        key_env_name: bool,
        key_max_len: Option<usize>,
        value: Box<Schema>,
        max_properties: Option<usize>,
        additional_properties: bool,
    },
    Object {
        fields: Vec<FieldSchema>,
        additional_properties: bool,
    },
    Union(Vec<Schema>),
}

pub fn is_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_uppercase() || b == b'_')
        && bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

impl Schema {
    pub fn validate(&self, value: &Value, path: &str) -> Result<(), SchemaFailure> {
        match self {
            Self::String { max_len, env_name } => {
                let Some(text) = value.as_str() else {
                    return Err(SchemaFailure {
                        path: path.to_owned(),
                        message: "Expected string".to_owned(),
                    });
                };
                if *env_name && !is_env_name(text) {
                    return Err(SchemaFailure {
                        path: path.to_owned(),
                        message: "Expected string to match ^[A-Z_][A-Z0-9_]*$".to_owned(),
                    });
                }
                if let Some(max) = max_len
                    && text.encode_utf16().count() > *max
                {
                    return Err(SchemaFailure {
                        path: path.to_owned(),
                        message: format!("Expected string length less or equal to {max}"),
                    });
                }
                Ok(())
            }
            Self::Integer { min, max } => {
                let Some(num) = value.as_number() else {
                    return Err(SchemaFailure {
                        path: path.to_owned(),
                        message: "Expected integer".to_owned(),
                    });
                };
                let int_val: i128 = if let Some(i) = num.as_i64() {
                    i128::from(i)
                } else if let Some(u) = num.as_u64() {
                    i128::from(u)
                } else {
                    return Err(SchemaFailure {
                        path: path.to_owned(),
                        message: "Expected integer".to_owned(),
                    });
                };
                if let Some(m) = min
                    && int_val < i128::from(*m)
                {
                    return Err(SchemaFailure {
                        path: path.to_owned(),
                        message: format!("Expected integer to be greater or equal to {m}"),
                    });
                }
                if let Some(m) = max
                    && int_val > i128::from(*m)
                {
                    return Err(SchemaFailure {
                        path: path.to_owned(),
                        message: format!("Expected integer to be less or equal to {m}"),
                    });
                }
                Ok(())
            }
            Self::Number => {
                if value.is_number() {
                    Ok(())
                } else {
                    Err(SchemaFailure {
                        path: path.to_owned(),
                        message: "Expected number".to_owned(),
                    })
                }
            }
            Self::Boolean => {
                if value.is_boolean() {
                    Ok(())
                } else {
                    Err(SchemaFailure {
                        path: path.to_owned(),
                        message: "Expected boolean".to_owned(),
                    })
                }
            }
            Self::Null => {
                if value.is_null() {
                    Ok(())
                } else {
                    Err(SchemaFailure {
                        path: path.to_owned(),
                        message: "Expected null".to_owned(),
                    })
                }
            }
            Self::LiteralStr(expected) => {
                if value.as_str() == Some(*expected) {
                    Ok(())
                } else {
                    Err(SchemaFailure {
                        path: path.to_owned(),
                        message: format!("Expected '{expected}'"),
                    })
                }
            }
            Self::LiteralBool(expected) => {
                if value.as_bool() == Some(*expected) {
                    Ok(())
                } else {
                    Err(SchemaFailure {
                        path: path.to_owned(),
                        message: format!("Expected {expected}"),
                    })
                }
            }
            Self::Never => Err(SchemaFailure {
                path: path.to_owned(),
                message: "Expected never".to_owned(),
            }),
            Self::Unknown => Ok(()),
            Self::Array { item, max_items } => {
                let Some(items) = value.as_array() else {
                    return Err(SchemaFailure {
                        path: path.to_owned(),
                        message: "Expected array".to_owned(),
                    });
                };
                if let Some(max) = max_items
                    && items.len() > *max
                {
                    return Err(SchemaFailure {
                        path: path.to_owned(),
                        message: format!("Expected array length less or equal to {max}"),
                    });
                }
                for (index, element) in items.iter().enumerate() {
                    let elem_path = format!("{path}/{index}");
                    item.validate(element, &elem_path)?;
                }
                Ok(())
            }
            Self::Record {
                key_env_name,
                key_max_len,
                value: val_schema,
                max_properties,
                additional_properties,
            } => {
                let Some(object) = value.as_object() else {
                    return Err(SchemaFailure {
                        path: path.to_owned(),
                        message: "Expected object".to_owned(),
                    });
                };
                if let Some(max) = max_properties
                    && object.len() > *max
                {
                    return Err(SchemaFailure {
                        path: path.to_owned(),
                        message: format!("Expected object to have at most {max} properties"),
                    });
                }
                for (key, entry_val) in object {
                    let key_path = format!("{path}/{key}");
                    if *key_env_name && !is_env_name(key) {
                        if !*additional_properties {
                            return Err(SchemaFailure {
                                path: key_path,
                                message: "Unexpected property".to_owned(),
                            });
                        }
                        continue;
                    }
                    if let Some(max) = key_max_len
                        && key.encode_utf16().count() > *max
                    {
                        if !*additional_properties {
                            return Err(SchemaFailure {
                                path: key_path,
                                message: format!(
                                    "Expected property name length less or equal to {max}"
                                ),
                            });
                        }
                        continue;
                    }
                    val_schema.validate(entry_val, &key_path)?;
                }
                Ok(())
            }
            Self::Object {
                fields,
                additional_properties,
            } => {
                let Some(object) = value.as_object() else {
                    return Err(SchemaFailure {
                        path: path.to_owned(),
                        message: "Expected object".to_owned(),
                    });
                };
                for field in fields {
                    let field_path = format!("{path}/{}", field.name);
                    match object.get(field.name) {
                        Some(field_value) => field.schema.validate(field_value, &field_path)?,
                        None if !field.optional => {
                            return Err(SchemaFailure {
                                path: field_path,
                                message: "Expected required property".to_owned(),
                            });
                        }
                        None => {}
                    }
                }
                if !*additional_properties {
                    for key in object.keys() {
                        if !fields.iter().any(|field| field.name == key) {
                            return Err(SchemaFailure {
                                path: format!("{path}/{key}"),
                                message: "Unexpected property".to_owned(),
                            });
                        }
                    }
                }
                Ok(())
            }
            Self::Union(variants) => {
                let mut best: Option<SchemaFailure> = None;
                for variant in variants {
                    match variant.validate(value, path) {
                        Ok(()) => return Ok(()),
                        Err(err) => {
                            let replace = best
                                .as_ref()
                                .is_none_or(|prev| err.path.len() > prev.path.len());
                            if replace {
                                best = Some(err);
                            }
                        }
                    }
                }
                Err(best.unwrap_or_else(|| SchemaFailure {
                    path: path.to_owned(),
                    message: "Expected union value".to_owned(),
                }))
            }
        }
    }
}

pub fn req(name: &'static str, schema: Schema) -> FieldSchema {
    FieldSchema {
        name,
        schema,
        optional: false,
    }
}

pub fn opt(name: &'static str, schema: Schema) -> FieldSchema {
    FieldSchema {
        name,
        schema,
        optional: true,
    }
}

pub fn str_schema() -> Schema {
    Schema::String {
        max_len: Some(RPC_INBOUND_LIMITS.text_characters),
        env_name: false,
    }
}

pub fn id_schema() -> Schema {
    Schema::String {
        max_len: Some(RPC_INBOUND_LIMITS.identifier_characters),
        env_name: false,
    }
}

pub fn path_str_schema() -> Schema {
    Schema::String {
        max_len: Some(RPC_INBOUND_LIMITS.path_characters),
        env_name: false,
    }
}

pub fn secret_schema() -> Schema {
    Schema::String {
        max_len: Some(RPC_INBOUND_LIMITS.secret_characters),
        env_name: false,
    }
}

pub fn num_schema() -> Schema {
    Schema::Integer {
        min: Some(0),
        max: Some(RPC_INBOUND_LIMITS.count),
    }
}

pub fn pos_int_schema() -> Schema {
    Schema::Integer {
        min: Some(1),
        max: Some(RPC_INBOUND_LIMITS.count),
    }
}

pub fn bool_schema() -> Schema {
    Schema::Boolean
}

pub fn strings_schema() -> Schema {
    Schema::Array {
        item: Box::new(id_schema()),
        max_items: Some(RPC_INBOUND_LIMITS.list_items),
    }
}

pub fn literals_schema(values: &[&'static str]) -> Schema {
    Schema::Union(values.iter().map(|v| Schema::LiteralStr(v)).collect())
}

pub fn object_schema(fields: Vec<FieldSchema>) -> Schema {
    Schema::Object {
        fields,
        additional_properties: true,
    }
}

pub fn strict_object_schema(fields: Vec<FieldSchema>) -> Schema {
    Schema::Object {
        fields,
        additional_properties: false,
    }
}

pub fn thread_settled_by_schema() -> Schema {
    Schema::Union(vec![
        object_schema(vec![req("kind", Schema::LiteralStr("user"))]),
        object_schema(vec![
            req("kind", Schema::LiteralStr("agent")),
            opt("threadId", id_schema()),
            opt("threadTitle", str_schema()),
        ]),
        object_schema(vec![
            req("kind", literals_schema(&["pr-merged", "pr-closed"])),
            opt("pullRequestNumber", pos_int_schema()),
        ]),
        object_schema(vec![
            req("kind", Schema::LiteralStr("inactivity")),
            req("days", num_schema()),
        ]),
    ])
}

pub fn thinking_level_schema() -> Schema {
    literals_schema(&["off", "minimal", "low", "medium", "high", "xhigh", "max"])
}

pub fn session_kind_schema() -> Schema {
    literals_schema(&["interactive", "worker"])
}

pub fn session_context_schema() -> Schema {
    Schema::Record {
        key_env_name: false,
        key_max_len: Some(RPC_INBOUND_LIMITS.identifier_characters),
        value: Box::new(str_schema()),
        max_properties: Some(SESSION_CONTEXT_LIMITS.keys),
        additional_properties: true,
    }
}

pub fn session_env_schema() -> Schema {
    Schema::Record {
        key_env_name: true,
        key_max_len: None,
        value: Box::new(Schema::String {
            max_len: Some(RPC_INBOUND_LIMITS.env_value_characters),
            env_name: false,
        }),
        max_properties: Some(RPC_INBOUND_LIMITS.env_entries),
        additional_properties: false,
    }
}

pub fn images_schema() -> Schema {
    let image_content = object_schema(vec![
        req("type", Schema::LiteralStr("image")),
        req(
            "data",
            Schema::String {
                max_len: None,
                env_name: false,
            },
        ),
        req("mimeType", id_schema()),
    ]);
    Schema::Array {
        item: Box::new(image_content),
        max_items: Some(RPC_INBOUND_LIMITS.images),
    }
}

pub fn model_entries_schema() -> Schema {
    let model_entry = object_schema(vec![
        req("model", Schema::Unknown),
        opt("thinkingLevel", thinking_level_schema()),
        opt("thinkingSelection", Schema::Unknown),
        opt("serviceTier", Schema::Unknown),
    ]);
    Schema::Array {
        item: Box::new(model_entry),
        max_items: Some(RPC_INBOUND_LIMITS.list_items),
    }
}

pub fn retry_fallback_schema() -> Schema {
    object_schema(vec![
        req("modelFallback", bool_schema()),
        req(
            "fallbackChains",
            Schema::Record {
                key_env_name: false,
                key_max_len: Some(RPC_INBOUND_LIMITS.identifier_characters),
                value: Box::new(strings_schema()),
                max_properties: Some(RPC_INBOUND_LIMITS.record_keys),
                additional_properties: true,
            },
        ),
    ])
}

pub fn question_answers_schema() -> Schema {
    Schema::Record {
        key_env_name: false,
        key_max_len: Some(RPC_INBOUND_LIMITS.identifier_characters),
        value: Box::new(object_schema(vec![
            req(
                "selected",
                Schema::Array {
                    item: Box::new(Schema::String {
                        max_len: Some(RPC_QUESTION_ANSWER_LIMITS.label_characters),
                        env_name: false,
                    }),
                    max_items: Some(RPC_QUESTION_ANSWER_LIMITS.selected_per_answer),
                },
            ),
            opt("text", str_schema()),
        ])),
        max_properties: Some(RPC_QUESTION_ANSWER_LIMITS.answers),
        additional_properties: true,
    }
}

pub fn command_schema(record_type: &'static str, mut properties: Vec<FieldSchema>) -> Schema {
    let mut fields = vec![
        opt("id", id_schema()),
        opt("admissionId", id_schema()),
        req("type", Schema::LiteralStr(record_type)),
    ];
    fields.append(&mut properties);
    object_schema(fields)
}

pub fn session_schema(record_type: &'static str, mut properties: Vec<FieldSchema>) -> Schema {
    let mut fields = vec![opt("sessionId", id_schema())];
    fields.append(&mut properties);
    command_schema(record_type, fields)
}

pub fn admission_session_schema(
    record_type: &'static str,
    mut properties: Vec<FieldSchema>,
) -> Schema {
    let mut fields = vec![
        opt("id", id_schema()),
        req("type", Schema::LiteralStr(record_type)),
        opt("sessionId", id_schema()),
    ];
    fields.append(&mut properties);
    object_schema(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn env_names_and_string_bounds_validate_utf16_and_pattern() {
        assert!(is_env_name("OMOCAT_LANE"));
        assert!(is_env_name("_A1"));
        assert!(!is_env_name("1A"));
        assert!(!is_env_name("lower"));

        let s = id_schema();
        assert!(s.validate(&json!("abc"), "").is_ok());
        let too_long = "x".repeat(RPC_INBOUND_LIMITS.identifier_characters + 1);
        assert!(s.validate(&json!(too_long), "/id").is_err());

        // Astral scalar takes 2 UTF-16 code units.
        let one = Schema::String {
            max_len: Some(1),
            env_name: false,
        };
        assert!(one.validate(&json!("😀"), "/s").is_err());
    }

    #[test]
    fn integer_and_record_bounds_fail_with_field_path() {
        assert!(num_schema().validate(&json!(0), "/n").is_ok());
        assert!(num_schema().validate(&json!(-1), "/n").is_err());
        assert!(num_schema().validate(&json!(1.5), "/n").is_err());

        let env = session_env_schema();
        assert!(env.validate(&json!({"OMOCAT_LANE": "1"}), "/env").is_ok());
        let bad_key = env.validate(&json!({"bad": "1"}), "/env").unwrap_err();
        assert_eq!(bad_key.path, "/env/bad");
    }
}
