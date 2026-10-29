use serde::Serialize;
use serde_json::Value;

use crate::internal::jsonc::{
    JsonNode, JsoncEditError, NodeKind, apply_edits, find_node_at_location, modify,
    parse_jsonc_safe, parse_tree,
};
use crate::issue::PathSegment;
use super::types::OmoConfigEdit;
use super::writer::formatting_options;

pub fn apply_omo_config_edit(content: &str, edit: &OmoConfigEdit) -> Result<String, JsoncEditError> {
    let edits = modify(content, &edit.path, edit.value.as_ref(), &formatting_options())?;
    let fallback = apply_edits(content, &edits)?;
    let crlf = content.contains("\r\n");
    if crlf && content.replace("\r\n", "").contains('\n') {
        return Ok(fallback);
    }
    let normalized = if crlf { content.replace("\r\n", "\n") } else { content.to_string() };
    let Some(surgical) = surgical_edit(&normalized, edit) else {
        return Ok(fallback);
    };
    let surgical = if crlf { surgical.replace('\n', "\r\n") } else { surgical };
    let left = parse_jsonc_safe(&surgical);
    let right = parse_jsonc_safe(&fallback);
    if left.errors.is_empty()
        && (!right.errors.is_empty()
            || left.data.as_ref().map(Value::to_string) == right.data.as_ref().map(Value::to_string))
    {
        Ok(surgical)
    } else {
        Ok(fallback)
    }
}

fn line_start(text: &str, offset: usize) -> usize {
    text[..offset].rfind('\n').map_or(0, |index| index + 1)
}

fn line_end(text: &str, offset: usize) -> usize {
    text[offset..].find('\n').map_or(text.len(), |index| offset + index)
}

fn whitespace(text: &str) -> &str {
    text.trim_start_matches([' ', '\t'])
}

fn indent(text: &str, offset: usize) -> &str {
    let line = &text[line_start(text, offset)..];
    &line[..line.len() - whitespace(line).len()]
}

fn comma_after(text: &str, offset: usize) -> Option<usize> {
    let rest = &text[offset..];
    let trimmed = whitespace(rest);
    trimmed.starts_with(',').then_some(offset + rest.len() - trimmed.len())
}

fn line_rest_is_trivia(text: &str, offset: usize) -> bool {
    let rest = whitespace(&text[offset..line_end(text, offset)]);
    let rest = whitespace(rest.strip_prefix(',').unwrap_or(rest));
    rest.is_empty() || rest.starts_with("//") || rest.strip_prefix("/*").is_some_and(|comment| {
        comment.find("*/").is_some_and(|end| whitespace(&comment[end + 2..]).is_empty())
    })
}

fn file_indent_unit(text: &str) -> String {
    let indents: Vec<&str> = text.split('\n').skip(1).filter_map(|line| {
        let rest = whitespace(line);
        (!rest.is_empty() && rest.len() < line.len()).then_some(&line[..line.len() - rest.len()])
    }).collect();
    if indents.iter().any(|indent| indent.starts_with('\t')) {
        "\t".to_string()
    } else {
        " ".repeat(indents.iter().map(|indent| indent.len()).min().unwrap_or(2))
    }
}

fn render_member(key: &str, value: &Value, member_indent: &str, unit: &str) -> Option<String> {
    let mut bytes = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(unit.as_bytes());
    let mut serializer = serde_json::Serializer::with_formatter(&mut bytes, formatter);
    value.serialize(&mut serializer).ok()?;
    let rendered = String::from_utf8(bytes).ok()?.replace('\n', &format!("\n{member_indent}"));
    Some(format!("{}: {rendered}", serde_json::to_string(key).ok()?))
}

fn surgical_edit(text: &str, edit: &OmoConfigEdit) -> Option<String> {
    let root = parse_tree(text).ok()?;
    if find_node_at_location(&root, &edit.path).is_some() {
        if edit.value.is_some() {
            return None;
        }
        let (last, path) = edit.path.split_last()?;
        let PathSegment::Key(key) = last else { return None };
        let parent = find_node_at_location(&root, path)?;
        if parent.kind != NodeKind::Object { return None; }
        let index = parent.children.iter().position(|property| {
            property.children.first().and_then(|key| key.value.as_deref()) == Some(key.as_str())
        })?;
        return remove_member(text, parent, index);
    }
    let Some(value) = &edit.value else { return Some(text.to_string()) };
    for depth in (0..edit.path.len()).rev() {
        let Some(parent) = find_node_at_location(&root, &edit.path[..depth]) else { continue };
        if parent.kind != NodeKind::Object { return None; }
        let PathSegment::Key(key) = &edit.path[depth] else { return None };
        let mut value = value.clone();
        for segment in edit.path[depth + 1..].iter().rev() {
            let key = match segment {
                PathSegment::Key(key) => key.clone(),
                PathSegment::Index(index) => index.to_string(),
            };
            value = Value::Object(serde_json::Map::from_iter([(key, value)]));
        }
        return insert_member(text, parent, key, &value);
    }
    None
}

fn insert_member(text: &str, parent: &JsonNode, key: &str, value: &Value) -> Option<String> {
    let object_indent = indent(text, parent.offset);
    let close = parent.end() - 1;
    let Some(last) = parent.children.last() else {
        if !text[parent.offset + 1..close].trim().is_empty() { return None; }
        let unit = file_indent_unit(text);
        let member_indent = format!("{object_indent}{unit}");
        let member = render_member(key, value, &member_indent, &unit)?;
        return Some(format!("{}{{\n{member_indent}{member}\n{object_indent}}}{}", &text[..parent.offset], &text[close + 1..]));
    };
    if !whitespace(&text[line_start(text, last.offset)..last.offset]).is_empty()
        || !line_rest_is_trivia(text, last.end())
    { return None; }
    let insert_at = line_end(text, last.end());
    if insert_at >= close { return None; }
    let member_indent = indent(text, last.offset);
    let unit = member_indent.strip_prefix(object_indent).filter(|unit| !unit.is_empty())
        .map_or_else(|| file_indent_unit(text), str::to_string);
    let member = render_member(key, value, member_indent, &unit)?;
    let trailing = comma_after(text, last.end()).is_some();
    let head = if trailing { text[..insert_at].to_string() } else {
        format!("{},{}", &text[..last.end()], &text[last.end()..insert_at])
    };
    Some(format!("{head}\n{member_indent}{member}{}{}", if trailing { "," } else { "" }, &text[insert_at..]))
}

fn remove_member(text: &str, parent: &JsonNode, index: usize) -> Option<String> {
    let property = &parent.children[index];
    if !whitespace(&text[line_start(text, property.offset)..property.offset]).is_empty()
        || !line_rest_is_trivia(text, property.end())
    { return None; }
    let from = line_start(text, property.offset);
    let after = line_end(text, property.end());
    let to = if after < text.len() { after + 1 } else { after };
    let mut removed = format!("{}{}", &text[..from], &text[to..]);
    if index + 1 == parent.children.len()
        && index > 0
        && comma_after(text, property.end()).is_none()
        && let Some(comma) = comma_after(text, parent.children[index - 1].end())
    {
        removed.remove(comma);
    }
    Some(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn edit(keys: &[&str], value: Option<Value>) -> OmoConfigEdit {
        OmoConfigEdit {
            path: keys.iter().map(|key| PathSegment::Key((*key).to_string())).collect(),
            value,
        }
    }

    #[test]
    fn insertion_preserves_tabs_comments_and_trailing_comma_style() {
        let cases = [
            ("{\n\t\"a\": 1,\n\t\"b\": 2 // keep\n}\n", "{\n\t\"a\": 1,\n\t\"b\": 2, // keep\n\t\"c\": [\n\t\t3\n\t]\n}\n"),
            ("{\n    \"a\": 1,\n}\n", "{\n    \"a\": 1,\n    \"c\": [\n        3\n    ],\n}\n"),
        ];
        for (content, expected) in cases {
            let next = apply_omo_config_edit(content, &edit(&["c"], Some(json!([3])))).expect("insert");
            assert_eq!(next, expected);
            assert!(parse_jsonc_safe(&next).errors.is_empty());
        }
    }

    #[test]
    fn insertion_preserves_crlf_and_multiline_neighbor() {
        let content = "{\r\n    \"a\": 1, // keep\r\n    \"b\": {\r\n        \"c\": true\r\n    }\r\n}\r\n";
        let next = apply_omo_config_edit(content, &edit(&["d"], Some(json!({"e": 1})))).expect("insert");
        assert_eq!(next, "{\r\n    \"a\": 1, // keep\r\n    \"b\": {\r\n        \"c\": true\r\n    },\r\n    \"d\": {\r\n        \"e\": 1\r\n    }\r\n}\r\n");
    }

    #[test]
    fn removal_preserves_neighbors_and_repairs_sole_trailing_comma() {
        let cases = [
            ("{\n  \"a\": 1, // first\n  \"b\": 2\n}\n", "b", "{\n  \"a\": 1 // first\n}\n"),
            ("{\n  \"a\": 1,\n  \"b\": 2, // remove\n  \"c\": 3,\n}\n", "b", "{\n  \"a\": 1,\n  \"c\": 3,\n}\n"),
            ("{\n  \"a\": 1,\n}\n", "a", "{\n}\n"),
            ("{\r\n  \"a\": 1,\r\n}\r\n", "a", "{\r\n}\r\n"),
        ];
        for (content, key, expected) in cases {
            let next = apply_omo_config_edit(content, &edit(&[key], None)).expect("remove");
            assert_eq!(next, expected);
            assert!(parse_jsonc_safe(&next).errors.is_empty());
        }
    }

    #[test]
    fn nested_insertion_uses_nearest_parent_and_file_indent() {
        let content = "{\n\t\"agents\": {},\n\t\"x\": 1\n}\n";
        let next = apply_omo_config_edit(content, &edit(&["agents", "oracle", "model"], Some(json!("m")))).expect("insert");
        assert_eq!(next, "{\n\t\"agents\": {\n\t\t\"oracle\": {\n\t\t\t\"model\": \"m\"\n\t\t}\n\t},\n\t\"x\": 1\n}\n");
    }

    #[test]
    fn shared_lines_and_mixed_eol_use_formatted_fallback() {
        for content in ["{ \"a\": 1, \"b\": 2 }\n", "{\r\n  \"a\": 1,\n  \"b\": 2\r\n}\n"] {
            let edit = edit(&["c"], Some(json!(3)));
            let expected = apply_edits(content, &modify(content, &edit.path, edit.value.as_ref(), &formatting_options()).expect("modify")).expect("apply");
            let next = apply_omo_config_edit(content, &edit).expect("insert");
            assert_eq!(next, expected);
            assert_eq!(parse_jsonc_safe(&next).data, Some(json!({"a": 1, "b": 2, "c": 3})));
        }
        let next = apply_omo_config_edit("{\n  \"a\": 1, /* x */ \"b\": 2 /* y */\n}\n", &edit(&["a"], None)).expect("remove");
        assert_eq!(parse_jsonc_safe(&next).data, Some(json!({"b": 2})));
    }

    #[test]
    fn mismatched_nested_numeric_object_falls_back_to_array_data() {
        let content = "{\n  \"a\": 1\n}\n";
        let edit = OmoConfigEdit {
            path: vec![PathSegment::Key("items".to_string()), PathSegment::Index(0)],
            value: Some(json!(7)),
        };
        let next = apply_omo_config_edit(content, &edit).expect("insert array");
        assert_eq!(parse_jsonc_safe(&next).data, Some(json!({"a": 1, "items": [7]})));
    }

    #[test]
    fn empty_root_object_opens_onto_its_own_lines_after_a_leading_comment() {
        let next = apply_omo_config_edit("// top\n{}\n", &edit(&["a"], Some(json!(1)))).expect("insert");
        assert_eq!(next, "// top\n{\n  \"a\": 1\n}\n");
    }

    #[test]
    fn empty_nested_object_in_a_tab_file_is_indented_with_tabs() {
        let content = "{\n\t\"agents\": {},\n\t\"x\": 1\n}\n";
        let next = apply_omo_config_edit(content, &edit(&["agents", "oracle"], Some(json!("m")))).expect("insert");
        assert_eq!(next, "{\n\t\"agents\": {\n\t\t\"oracle\": \"m\"\n\t},\n\t\"x\": 1\n}\n");
    }

    #[test]
    fn missing_nested_parent_keeps_comments_and_adds_one_nested_member() {
        let content = "{\n  // agents\n  \"agents\": {\n    \"oracle\": {}\n  }\n}\n";
        let next = apply_omo_config_edit(content, &edit(&["agents", "explore", "model"], Some(json!("zai/glm-5.3")))).expect("insert");
        assert_eq!(next, "{\n  // agents\n  \"agents\": {\n    \"oracle\": {},\n    \"explore\": {\n      \"model\": \"zai/glm-5.3\"\n    }\n  }\n}\n");
        assert_eq!(
            parse_jsonc_safe(&next).data,
            Some(json!({"agents": {"oracle": {}, "explore": {"model": "zai/glm-5.3"}}}))
        );
    }

    #[test]
    fn removing_a_missing_member_leaves_the_document_untouched() {
        let content = "{\n  \"a\": 1, // keep\n}\n";
        let next = apply_omo_config_edit(content, &edit(&["zzz"], None)).expect("remove");
        assert_eq!(next, content);
    }

    #[test]
    fn replacement_preserves_sibling_lines_and_comments() {
        let content = "{\n  /* keep */\n  \"a\": [1, 2,], // keep too\n  \"b\": 2,\n}\n";
        let next = apply_omo_config_edit(content, &edit(&["b"], Some(json!(5)))).expect("replace");
        assert_eq!(next, "{\n  /* keep */\n  \"a\": [1, 2,], // keep too\n  \"b\": 5,\n}\n");
    }
}
