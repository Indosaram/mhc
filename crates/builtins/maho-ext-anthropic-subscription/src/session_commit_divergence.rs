//! senpi `session-commit-divergence.ts` (senpi#1975): the first diverged field path between the provider-final and
//! the committed assistant projection. Only paths leave this module; no value from either side is embedded.
use serde_json::{Map, Value};

const MAX_DIVERGENCE_DEPTH: usize = 16;

/// Argument keys are model-produced data: only a short plain identifier is named in a path. Anything else (long, or
/// containing `.`/`[` that would make the path ambiguous) is named by its sorted position, never by its text.
fn plain_key(key: &str) -> bool {
    let mut characters = key.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_' || first == '$')
        && key.chars().count() <= 64
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '$' | '-')
        })
}

fn key_segment(key: &str, position: usize) -> String {
    if plain_key(key) {
        format!(".{key}")
    } else {
        format!(".<key#{position}>")
    }
}

/// First diverged sub-path between two JSON-shaped values: objects contribute `.key` (sorted, so the path is
/// deterministic), arrays contribute `[n]`, and a length difference names the first index present on only one side.
/// `Some("")` when the values at this position differ, `None` when they are equal; past the depth cap the parent
/// position is named instead of recursing.
fn first_diverged_segment(
    left: Option<&Value>,
    right: Option<&Value>,
    depth: usize,
) -> Option<String> {
    if left == right {
        return None;
    }
    if depth > MAX_DIVERGENCE_DEPTH {
        return Some(String::new());
    }
    match (left, right) {
        (Some(Value::Array(left)), Some(Value::Array(right))) => {
            let shared = left.len().min(right.len());
            for index in 0..shared {
                if let Some(inner) =
                    first_diverged_segment(left.get(index), right.get(index), depth + 1)
                {
                    return Some(format!("[{index}]{inner}"));
                }
            }
            (left.len() != right.len()).then(|| format!("[{shared}]"))
        }
        (Some(Value::Object(left)), Some(Value::Object(right))) => {
            let mut keys: Vec<&String> = left.keys().chain(right.keys()).collect();
            keys.sort();
            keys.dedup();
            for (position, key) in keys.into_iter().enumerate() {
                if let Some(inner) =
                    first_diverged_segment(left.get(key), right.get(key), depth + 1)
                {
                    return Some(format!("{}{inner}", key_segment(key, position)));
                }
            }
            None
        }
        _ => Some(String::new()),
    }
}

fn field<'a>(block: &'a Value, name: &str) -> Option<&'a Value> {
    block
        .as_object()
        .and_then(|block: &Map<String, Value>| block.get(name))
}

fn first_diverged_block_field(index: usize, left: &Value, right: &Value) -> Option<String> {
    let kind = field(left, "type");
    if kind != field(right, "type") {
        return Some(format!("content[{index}].type"));
    }
    let scalar_fields: Option<&[&str]> = match kind.and_then(Value::as_str) {
        Some("text") => Some(&["text"]),
        Some("thinking") => Some(&["thinking", "thinkingSignature"]),
        Some("toolCall") => Some(&["id", "name"]),
        _ => None,
    };
    let Some(scalar_fields) = scalar_fields else {
        return first_diverged_segment(Some(left), Some(right), 0)
            .map(|segment| format!("content[{index}]{segment}"));
    };
    if let Some(name) = scalar_fields
        .iter()
        .find(|name| field(left, name) != field(right, name))
    {
        return Some(format!("content[{index}].{name}"));
    }
    if kind.and_then(Value::as_str) != Some("toolCall") {
        return None;
    }
    first_diverged_segment(field(left, "arguments"), field(right, "arguments"), 0)
        .map(|segment| format!("content[{index}].arguments{segment}"))
}

/// First diverged path between the provider-final and committed projections: `content.length`, then per block
/// `type`, its semantic fields, and for toolCall blocks `arguments.<path>`, then `api` / `provider` / `model`.
pub fn first_diverged_path(provider_final: &Value, committed: &Value) -> String {
    let empty = Vec::new();
    let left = provider_final["content"].as_array().unwrap_or(&empty);
    let right = committed["content"].as_array().unwrap_or(&empty);
    if left.len() != right.len() {
        return "content.length".into();
    }
    for (index, (left, right)) in left.iter().zip(right).enumerate() {
        if let Some(path) = first_diverged_block_field(index, left, right) {
            return path;
        }
    }
    ["api", "provider", "model"]
        .into_iter()
        .find(|name| provider_final.get(*name) != committed.get(*name))
        .unwrap_or("content")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn projection(content: Value) -> Value {
        json!({"role":"assistant","api":"claude-sdk-oauth","provider":"anthropic-subscription","model":"m","content":content})
    }

    #[test]
    fn names_the_first_diverged_block_field() {
        let base = projection(
            json!([{"type":"thinking","thinking":"a","thinkingSignature":"s"},{"type":"text","text":"t"},{"type":"toolCall","id":"c","name":"n","arguments":{"a":{"b":[1,2]}}}]),
        );
        let mut other = base.clone();
        other["content"].as_array_mut().unwrap().pop();
        assert_eq!(first_diverged_path(&base, &other), "content.length");
        let mut other = base.clone();
        other["content"][0]["thinkingSignature"] = json!("x");
        assert_eq!(
            first_diverged_path(&base, &other),
            "content[0].thinkingSignature"
        );
        let mut other = base.clone();
        other["content"][1]["type"] = json!("image");
        assert_eq!(first_diverged_path(&base, &other), "content[1].type");
        let mut other = base.clone();
        other["content"][2]["arguments"]["a"]["b"][1] = json!(3);
        assert_eq!(
            first_diverged_path(&base, &other),
            "content[2].arguments.a.b[1]"
        );
        let mut other = base.clone();
        other["content"][2]["arguments"]["a"]["b"] = json!([1, 2, 3]);
        assert_eq!(
            first_diverged_path(&base, &other),
            "content[2].arguments.a.b[2]"
        );
        let mut other = base.clone();
        other["model"] = json!("n");
        assert_eq!(first_diverged_path(&base, &other), "model");
        assert_eq!(first_diverged_path(&base, &base), "content");
    }

    #[test]
    fn non_plain_keys_are_named_by_sorted_position_never_by_text() {
        let base = projection(
            json!([{"type":"toolCall","id":"c","name":"n","arguments":{"a":1,"secret.key":"x"}}]),
        );
        let mut other = base.clone();
        other["content"][0]["arguments"]["secret.key"] = json!("y");
        let path = first_diverged_path(&base, &other);
        assert_eq!(path, "content[0].arguments.<key#1>");
        assert!(!path.contains("secret"));
    }

    #[test]
    fn deep_arguments_stop_at_the_depth_cap() {
        let mut deep = json!(1);
        for _ in 0..40 {
            deep = json!({"k": deep});
        }
        let base = projection(json!([{"type":"toolCall","id":"c","name":"n","arguments":deep}]));
        let mut other = base.clone();
        let mut cursor = &mut other["content"][0]["arguments"];
        for _ in 0..40 {
            cursor = &mut cursor["k"];
        }
        *cursor = json!(2);
        let path = first_diverged_path(&base, &other);
        assert_eq!(path.matches(".k").count(), MAX_DIVERGENCE_DEPTH + 1);
    }
}
