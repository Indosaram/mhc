//! Adds members to an object inside a JSON-with-comments document by inserting text (upstream
//! `omo-native/bin/lib/jsonc-edit.js`), so every comment, blank line and key order the user wrote
//! survives. [`insert_jsonc_member`] only ever adds a key that is absent; the result is re-parsed and
//! compared with the intended document before it is returned, so a document this scanner misreads is
//! refused rather than written.

use super::jsonc::parse_jsonc;
use serde_json::{Map, Value};

struct Member {
    key: String,
    key_start: usize,
    value_end: usize,
    object: Option<ObjectNode>,
}

struct ObjectNode {
    open: usize,
    close: usize,
    members: Vec<Member>,
    trailing_comma: bool,
}

fn is_blank(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

fn starts_with(text: &[char], index: usize, pattern: &str) -> bool {
    pattern
        .chars()
        .enumerate()
        .all(|(offset, c)| text.get(index + offset) == Some(&c))
}

fn skip_blank(text: &[char], mut index: usize) -> usize {
    while index < text.len() {
        if is_blank(text[index]) {
            index += 1;
        } else if starts_with(text, index, "//") {
            while index < text.len() && text[index] != '\n' {
                index += 1;
            }
        } else if starts_with(text, index, "/*") {
            let mut cursor = index + 2;
            index = loop {
                if cursor + 1 >= text.len() {
                    break text.len();
                }
                if text[cursor] == '*' && text[cursor + 1] == '/' {
                    break cursor + 2;
                }
                cursor += 1;
            };
        } else {
            break;
        }
    }
    index
}

fn string_end(text: &[char], mut index: usize) -> Result<usize, String> {
    index += 1;
    while index < text.len() && text[index] != '"' {
        index += if text[index] == '\\' { 2 } else { 1 };
    }
    if index >= text.len() {
        return Err("unterminated string".into());
    }
    Ok(index + 1)
}

struct Scanned {
    end: usize,
    object: Option<ObjectNode>,
}

fn scan_value(text: &[char], index: usize) -> Result<Scanned, String> {
    match text.get(index) {
        Some('{') => scan_object(text, index),
        Some('[') => {
            let mut cursor = skip_blank(text, index + 1);
            while text.get(cursor) != Some(&']') {
                cursor = skip_blank(text, scan_value(text, cursor)?.end);
                if text.get(cursor) == Some(&',') {
                    cursor = skip_blank(text, cursor + 1);
                } else if text.get(cursor) != Some(&']') {
                    return Err(format!("expected , or ] at {cursor}"));
                }
            }
            Ok(Scanned {
                end: cursor + 1,
                object: None,
            })
        }
        Some('"') => Ok(Scanned {
            end: string_end(text, index)?,
            object: None,
        }),
        _ => {
            let mut cursor = index;
            while cursor < text.len()
                && !(text[cursor].is_whitespace() || matches!(text[cursor], ',' | '}' | ']' | '/'))
            {
                cursor += 1;
            }
            if cursor == index {
                return Err(format!("unexpected token at {index}"));
            }
            Ok(Scanned {
                end: cursor,
                object: None,
            })
        }
    }
}

fn scan_object(text: &[char], open: usize) -> Result<Scanned, String> {
    let mut members = Vec::new();
    let mut trailing_comma = false;
    let mut cursor = skip_blank(text, open + 1);
    while text.get(cursor) != Some(&'}') {
        if text.get(cursor) != Some(&'"') {
            return Err(format!("expected a key at {cursor}"));
        }
        let key_start = cursor;
        let key_end = string_end(text, cursor)?;
        cursor = skip_blank(text, key_end);
        if text.get(cursor) != Some(&':') {
            return Err(format!("expected : at {cursor}"));
        }
        let value = scan_value(text, skip_blank(text, cursor + 1))?;
        let raw_key: String = text[key_start..key_end].iter().collect();
        let key: String = serde_json::from_str(&raw_key).map_err(|e| e.to_string())?;
        let value_end = value.end;
        members.push(Member {
            key,
            key_start,
            value_end,
            object: value.object,
        });
        cursor = skip_blank(text, value_end);
        trailing_comma = text.get(cursor) == Some(&',');
        if trailing_comma {
            cursor = skip_blank(text, cursor + 1);
        } else if text.get(cursor) != Some(&'}') {
            return Err(format!("expected , or }} at {cursor}"));
        }
    }
    Ok(Scanned {
        end: cursor + 1,
        object: Some(ObjectNode {
            open,
            close: cursor,
            members,
            trailing_comma,
        }),
    })
}

fn line_start(text: &[char], index: usize) -> usize {
    text[..index]
        .iter()
        .rposition(|c| *c == '\n')
        .map_or(0, |p| p + 1)
}

fn line_indent(text: &[char], index: usize) -> Option<String> {
    let prefix: String = text[line_start(text, index)..index].iter().collect();
    prefix
        .chars()
        .all(|c| c == ' ' || c == '\t')
        .then_some(prefix)
}

struct Indent {
    closing: String,
    member: String,
}

fn indent_of(text: &[char], node: &ObjectNode) -> Indent {
    let closing = line_indent(text, node.close).unwrap_or_else(|| {
        text[line_start(text, node.open)..]
            .iter()
            .take_while(|c| **c == ' ' || **c == '\t')
            .collect()
    });
    let member = node
        .members
        .first()
        .and_then(|first| line_indent(text, first.key_start))
        .unwrap_or_else(|| format!("{closing}  "));
    Indent { closing, member }
}

fn json(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

fn member_text(key: &str, value: &Value, indent: &str) -> String {
    format!(
        "{indent}{}: {}",
        json(&key),
        pretty(value).replace('\n', &format!("\n{indent}"))
    )
}

fn insert_into(text: &[char], node: &ObjectNode, key: &str, value: &Value) -> String {
    let collect = |range: std::ops::Range<usize>| text[range].iter().collect::<String>();
    let indent = indent_of(text, node);
    let member = member_text(key, value, &indent.member);
    if node.members.is_empty() {
        let inner = collect(node.open + 1..node.close);
        // An empty `{}` (or one holding only whitespace) is rewritten whole; one holding a comment keeps it.
        if inner.trim().is_empty() {
            return format!(
                "{}\n{member}\n{}{}",
                collect(0..node.open + 1),
                indent.closing,
                collect(node.close..text.len())
            );
        }
        return format!(
            "{}\n{member}{}",
            collect(0..node.open + 1),
            collect(node.open + 1..text.len())
        );
    }
    let mut at = node.close;
    while at > 0 && text[at - 1].is_whitespace() {
        at -= 1;
    }
    // An object written on one line (`{ "a": 1 }`) gets the new member on that same line.
    let inline = line_indent(text, node.close).is_none();
    let added = if inline {
        format!(" {}: {}", json(&key), json(value))
    } else {
        format!("\n{member}")
    };
    let with_member: Vec<char> = format!("{}{added}{}", collect(0..at), collect(at..text.len()))
        .chars()
        .collect();
    if node.trailing_comma {
        return with_member.into_iter().collect();
    }
    let last_end = node.members.last().map_or(at, |m| m.value_end);
    let mut out: String = with_member[..last_end].iter().collect();
    out.push(',');
    out.extend(with_member[last_end..].iter());
    out
}

fn nest(path: &[String], key: &str, value: &Value) -> Value {
    let mut inner = Map::new();
    inner.insert(key.to_owned(), value.clone());
    path.iter()
        .rev()
        .fold(Value::Object(inner), |inner, segment| {
            let mut outer = Map::new();
            outer.insert(segment.clone(), inner);
            Value::Object(outer)
        })
}

fn set_in(document: &Value, path: &[String], key: &str, value: &Value) -> Value {
    let mut next = document.clone();
    let mut cursor = &mut next;
    for segment in path {
        let Some(object) = cursor.as_object_mut() else {
            return next;
        };
        cursor = object
            .entry(segment.clone())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    if let Some(object) = cursor.as_object_mut() {
        object.insert(key.to_owned(), value.clone());
    }
    next
}

/// `text` with `key: value` added to the object at `path` (created, nested, when missing).
///
/// # Errors
/// The text is not an object document, a `path` segment is not an object, `key` exists, or the
/// edited text does not parse back to the intended value.
pub fn insert_jsonc_member(
    text: &str,
    path: &[String],
    key: &str,
    value: &Value,
) -> Result<String, String> {
    let chars: Vec<char> = text.chars().collect();
    let start = skip_blank(&chars, 0);
    if chars.get(start) != Some(&'{') {
        return Err("the document is not an object".into());
    }
    let mut node = scan_object(&chars, start)?
        .object
        .ok_or("the document is not an object")?;
    let mut depth = 0;
    while depth < path.len() {
        let Some(index) = node.members.iter().position(|m| m.key == path[depth]) else {
            break;
        };
        let member = node.members.swap_remove(index);
        let Some(object) = member.object else {
            return Err(format!("{} is not an object", path[..=depth].join(".")));
        };
        node = object;
        depth += 1;
    }
    if depth == path.len() && node.members.iter().any(|m| m.key == key) {
        let mut full = path.to_vec();
        full.push(key.to_owned());
        return Err(format!("{} already exists", full.join(".")));
    }
    let next = if depth == path.len() {
        insert_into(&chars, &node, key, value)
    } else {
        insert_into(
            &chars,
            &node,
            &path[depth],
            &nest(&path[depth + 1..], key, value),
        )
    };
    let intended = set_in(&parse_jsonc(text)?, path, key, value);
    if parse_jsonc(&next).ok() != Some(intended) {
        return Err("the edited document does not parse back to the intended value".into());
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const COMMENTED: &str = "{\n  // the user's own note\n  \"$schema\": \"https://example.test/omo.schema.json\",\n  \"[opencode]\": { \"tmux\": { \"enabled\": true } }, /* trailing block comment */\n}\n";

    fn path(segments: &[&str]) -> Vec<String> {
        segments.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn a_nested_member_under_a_missing_block_keeps_comments() {
        let next = insert_jsonc_member(
            COMMENTED,
            &path(&["[native]", "categories"]),
            "quick",
            &json!({"model": "opencode-go/glm-5.2"}),
        )
        .expect("insert");
        assert!(next.contains("// the user's own note"));
        assert!(next.contains("/* trailing block comment */"));
        assert_eq!(
            parse_jsonc(&next).expect("parse"),
            json!({
                "$schema": "https://example.test/omo.schema.json",
                "[opencode]": {"tmux": {"enabled": true}},
                "[native]": {"categories": {"quick": {"model": "opencode-go/glm-5.2"}}},
            })
        );
    }

    #[test]
    fn a_separating_comma_follows_the_last_value() {
        let text = "{\n  \"[native]\": {\n    \"categories\": { \"quick\": { \"model\": \"a/b\" } } // mine\n  }\n}\n";
        let next = insert_jsonc_member(
            text,
            &path(&["[native]", "categories"]),
            "deep-low",
            &json!({"model": "c/d"}),
        )
        .expect("insert");
        assert!(next.contains("// mine"));
        assert_eq!(
            parse_jsonc(&next).expect("parse"),
            json!({"[native]": {"categories": {"quick": {"model": "a/b"}, "deep-low": {"model": "c/d"}}}})
        );
    }

    #[test]
    fn members_added_to_an_empty_document_are_pretty_json() {
        let first =
            insert_jsonc_member("{}\n", &[], "defaultProvider", &json!("kimi-coding")).expect("1");
        let next = insert_jsonc_member(&first, &[], "defaultModel", &json!("k3")).expect("2");
        assert_eq!(
            next,
            "{\n  \"defaultProvider\": \"kimi-coding\",\n  \"defaultModel\": \"k3\"\n}\n"
        );
    }

    #[test]
    fn existing_keys_non_object_paths_and_documents_are_refused() {
        let error = insert_jsonc_member(r#"{ "a": { "b": 1 } }"#, &path(&["a"]), "b", &json!(2))
            .expect_err("exists");
        assert!(error.contains("already exists"), "{error}");
        assert_eq!(
            insert_jsonc_member(r#"{ "a": [1, 2] }"#, &path(&["a"]), "b", &json!(2)),
            Err("a is not an object".into())
        );
        let error = insert_jsonc_member("[1]", &[], "b", &json!(2)).expect_err("array");
        assert!(error.contains("not an object"), "{error}");
    }

    #[test]
    fn string_data_holding_comment_and_brace_characters_is_intact() {
        let text = r#"{ "url": "https://x.test/a//b}", "note": "/* not a comment */" }"#;
        let next = insert_jsonc_member(text, &[], "k", &json!("v")).expect("insert");
        assert_eq!(
            parse_jsonc(&next).expect("parse"),
            json!({"url": "https://x.test/a//b}", "note": "/* not a comment */", "k": "v"})
        );
    }
}
