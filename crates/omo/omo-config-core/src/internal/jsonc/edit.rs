use serde_json::Value;

use super::format::{FormattingOptions, JsoncEdit, JsoncEditError, apply_edit, format_range};
use super::parse::{JsonNode, NodeKind, find_node_at_location, parse_tree, to_value};
use crate::issue::PathSegment;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModificationOptions {
    pub formatting: Option<FormattingOptions>,
}

impl ModificationOptions {
    pub fn formatted(formatting: FormattingOptions) -> Self {
        Self {
            formatting: Some(formatting),
        }
    }
}

pub fn to_json_text(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

pub fn modify(
    text: &str,
    path: &[PathSegment],
    value: Option<&Value>,
    options: &ModificationOptions,
) -> Result<Vec<JsoncEdit>, JsoncEditError> {
    set_property(text, path, value, options)
}

fn set_property(
    text: &str,
    original_path: &[PathSegment],
    value: Option<&Value>,
    options: &ModificationOptions,
) -> Result<Vec<JsoncEdit>, JsoncEditError> {
    let root = parse_tree(text).ok();
    let mut path: Vec<PathSegment> = original_path.to_vec();
    let mut value: Option<Value> = value.cloned();
    let mut parent: Option<&JsonNode> = None;
    let mut last_segment: Option<PathSegment> = None;

    while !path.is_empty() {
        last_segment = path.pop();
        parent = match root.as_ref() {
            Some(root) => find_node_at_location(root, &path),
            None => None,
        };
        if parent.is_none() && value.is_some() {
            let segment = last_segment.clone().unwrap_or(PathSegment::Index(0));
            let inner = value.take().unwrap();
            value = Some(match segment {
                PathSegment::Key(key) => {
                    let mut map = serde_json::Map::new();
                    map.insert(key, inner);
                    Value::Object(map)
                }
                PathSegment::Index(_) => Value::Array(vec![inner]),
            });
        } else {
            break;
        }
    }

    let Some(parent) = parent else {
        let Some(value) = value.as_ref() else {
            return Err(JsoncEditError::DeleteInEmptyDocument);
        };
        let (offset, length) = root
            .as_ref()
            .map(|node| (node.offset, node.length))
            .unwrap_or((0, 0));
        return Ok(with_formatting(
            text,
            JsoncEdit {
                offset,
                length,
                content: to_json_text(value),
            },
            options,
        ));
    };

    if parent.kind == NodeKind::Object {
        let Some(PathSegment::Key(key)) = last_segment.clone() else {
            return Err(JsoncEditError::CannotModifyParent);
        };
        let existing = parent.children.iter().position(|child| {
            child.kind == NodeKind::Property
                && child
                    .children
                    .first()
                    .and_then(|node| node.value.as_deref())
                    == Some(key.as_str())
        });

        match existing {
            Some(index) => {
                if value.is_none() {
                    let property = &parent.children[index];
                    let remove_end = property.end();
                    let remove_begin = if index > 0 {
                        parent.children[index - 1].end()
                    } else {
                        parent.offset + 1
                    };
                    let remove_end = if index == 0 && parent.children.len() > 1 {
                        parent.children[1].offset
                    } else {
                        remove_end
                    };
                    return Ok(with_formatting(
                        text,
                        JsoncEdit {
                            offset: remove_begin,
                            length: remove_end - remove_begin,
                            content: String::new(),
                        },
                        options,
                    ));
                }
                let existing_node = &parent.children[index].children[1];
                return Ok(with_formatting(
                    text,
                    JsoncEdit {
                        offset: existing_node.offset,
                        length: existing_node.length,
                        content: to_json_text(value.as_ref().unwrap()),
                    },
                    options,
                ));
            }
            None => {
                if value.is_none() {
                    return Ok(Vec::new());
                }
                let new_property = format!(
                    "{}: {}",
                    to_json_text(&Value::String(key)),
                    to_json_text(value.as_ref().unwrap())
                );
                let index = parent.children.len();
                let edit = if index > 0 {
                    JsoncEdit {
                        offset: parent.children[index - 1].end(),
                        length: 0,
                        content: format!(",{new_property}"),
                    }
                } else if parent.children.is_empty() {
                    JsoncEdit {
                        offset: parent.offset + 1,
                        length: 0,
                        content: new_property,
                    }
                } else {
                    JsoncEdit {
                        offset: parent.offset + 1,
                        length: 0,
                        content: format!("{new_property},"),
                    }
                };
                return Ok(with_formatting(text, edit, options));
            }
        }
    }

    if parent.kind == NodeKind::Array {
        let Some(PathSegment::Index(insert_index)) = last_segment.clone() else {
            return Err(JsoncEditError::CannotModifyParent);
        };
        if insert_index == -1 {
            let new_property = to_json_text(value.as_ref().unwrap());
            let edit = if parent.children.is_empty() {
                JsoncEdit {
                    offset: parent.offset + 1,
                    length: 0,
                    content: new_property,
                }
            } else {
                JsoncEdit {
                    offset: parent.children[parent.children.len() - 1].end(),
                    length: 0,
                    content: format!(",{new_property}"),
                }
            };
            return Ok(with_formatting(text, edit, options));
        }
        if value.is_none() {
            let Some(to_remove) = parent.children.get(insert_index as usize) else {
                return Ok(Vec::new());
            };
            let edit = if parent.children.len() == 1 {
                JsoncEdit {
                    offset: parent.offset + 1,
                    length: parent.length - 2,
                    content: String::new(),
                }
            } else if parent.children.len() - 1 == insert_index as usize {
                let previous = &parent.children[insert_index as usize - 1];
                let offset = previous.end();
                let parent_end = parent.offset + parent.length;
                JsoncEdit {
                    offset,
                    length: parent_end - 2 - offset,
                    content: String::new(),
                }
            } else {
                JsoncEdit {
                    offset: to_remove.offset,
                    length: parent.children[insert_index as usize + 1].offset - to_remove.offset,
                    content: String::new(),
                }
            };
            return Ok(with_formatting(text, edit, options));
        }
        let value = value.unwrap();
        let new_property = to_json_text(&value);
        if parent.children.len() > insert_index as usize {
            let to_modify = &parent.children[insert_index as usize];
            return Ok(with_formatting(
                text,
                JsoncEdit {
                    offset: to_modify.offset,
                    length: to_modify.length,
                    content: new_property,
                },
                options,
            ));
        }
        let edit = if parent.children.is_empty() {
            JsoncEdit {
                offset: parent.offset + 1,
                length: 0,
                content: new_property,
            }
        } else {
            let index = if insert_index > parent.children.len() as i64 {
                parent.children.len()
            } else {
                insert_index as usize
            };
            JsoncEdit {
                offset: parent.children[index - 1].end(),
                length: 0,
                content: format!(",{new_property}"),
            }
        };
        return Ok(with_formatting(text, edit, options));
    }

    Err(JsoncEditError::CannotModifyParent)
}

fn with_formatting(
    document: &str,
    edit: JsoncEdit,
    options: &ModificationOptions,
) -> Vec<JsoncEdit> {
    let Some(formatting) = options.formatting.as_ref() else {
        return vec![edit];
    };

    let mut new_text = apply_edit(document, &edit);
    let mut begin = edit.offset as i64;
    let mut end = (edit.offset + edit.content.len()) as i64;

    if edit.length == 0 || edit.content.is_empty() {
        while begin > 0 && !super::scan::is_eol(&new_text, begin as usize - 1) {
            begin -= 1;
        }
        while (end as usize) < new_text.len() && !super::scan::is_eol(&new_text, end as usize) {
            end += 1;
        }
    }

    let formatting_edits = format_range(
        &new_text,
        Some((begin as usize, (end - begin) as usize)),
        formatting,
    );
    for formatting_edit in formatting_edits.iter().rev() {
        new_text = apply_edit(&new_text, formatting_edit);
        begin = begin.min(formatting_edit.offset as i64);
        end = end.max((formatting_edit.offset + formatting_edit.length) as i64);
        end += formatting_edit.content.len() as i64 - formatting_edit.length as i64;
    }

    let edit_length = document.len() as i64 - (new_text.len() as i64 - end) - begin;
    vec![JsoncEdit {
        offset: begin.max(0) as usize,
        length: edit_length.max(0) as usize,
        content: new_text[begin.max(0) as usize..end.max(0) as usize].to_string(),
    }]
}

pub fn document_value(text: &str) -> Option<Value> {
    parse_tree(text).ok().map(|node| to_value(&node))
}
