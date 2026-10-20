use super::{
    super::item::{
        ItemError, RegisterValue,
        frontmatter::{FmEntry, FmValue, render_entry, value_node},
        yaml::{Value, parse},
    },
    SpaceError,
};
use std::collections::BTreeMap;

#[derive(Debug)]
pub enum AdoptedField {
    Reg { values: Vec<String>, in_text: usize },
    Set { elements: Vec<String> },
}
pub type AdoptedFields = Vec<(String, AdoptedField)>;
pub fn seed_fields(
    fields: &mut AdoptedFields,
    entries: &[FmEntry],
    registers: &BTreeMap<String, Vec<String>>,
) -> Result<(), SpaceError> {
    for entry in entries {
        let field = match &entry.value {
            FmValue::Remove => continue,
            FmValue::Set { elements, .. } => AdoptedField::Set {
                elements: elements.clone(),
            },
            FmValue::Reg { .. } => {
                let components = registers
                    .get(&entry.key)
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| SpaceError::MissingComponents(entry.key.clone()))?;
                AdoptedField::Reg {
                    values: components.clone(),
                    in_text: components.len(),
                }
            }
        };
        put(fields, &entry.key, field);
    }
    Ok(())
}
fn put(fields: &mut AdoptedFields, key: &str, field: AdoptedField) {
    if let Some((_, old)) = fields.iter_mut().find(|(k, _)| k == key) {
        *old = field;
    } else {
        fields.push((key.into(), field));
    }
}
fn add_missing(into: &mut Vec<String>, values: &[String]) {
    for value in values {
        if !into.contains(value) {
            into.push(value.clone());
        }
    }
}
pub fn adopt_fields(fields: &mut AdoptedFields, entries: &[FmEntry]) {
    for entry in entries {
        if entry.value == FmValue::Remove {
            continue;
        }
        if let Some((_, current)) = fields.iter_mut().find(|(key, _)| key == &entry.key) {
            match current {
                AdoptedField::Reg { values, .. } => match &entry.value {
                    FmValue::Reg { value } => add_missing(values, std::slice::from_ref(value)),
                    FmValue::Set { flow, .. } => add_missing(values, std::slice::from_ref(flow)),
                    FmValue::Remove => {}
                },
                AdoptedField::Set { elements } => match &entry.value {
                    FmValue::Reg { value } => add_missing(elements, std::slice::from_ref(value)),
                    FmValue::Set {
                        elements: incoming, ..
                    } => add_missing(elements, incoming),
                    FmValue::Remove => {}
                },
            }
        } else {
            let field = match &entry.value {
                FmValue::Reg { value } => AdoptedField::Reg {
                    values: vec![value.clone()],
                    in_text: 1,
                },
                FmValue::Set { elements, .. } => AdoptedField::Set {
                    elements: elements.clone(),
                },
                FmValue::Remove => continue,
            };
            fields.push((entry.key.clone(), field));
        }
    }
}
pub fn extra_values(fields: &AdoptedFields) -> Vec<RegisterValue> {
    fields
        .iter()
        .flat_map(|(key, field)| match field {
            AdoptedField::Reg { values, in_text } => values[*in_text..]
                .iter()
                .map(|value| RegisterValue {
                    key: key.clone(),
                    value: value.clone(),
                })
                .collect::<Vec<_>>(),
            AdoptedField::Set { .. } => Vec::new(),
        })
        .collect()
}
pub fn registers_of(fields: &AdoptedFields) -> BTreeMap<String, Vec<String>> {
    fields
        .iter()
        .filter_map(|(key, field)| match field {
            AdoptedField::Reg { values, .. } => Some((key.clone(), values.clone())),
            AdoptedField::Set { .. } => None,
        })
        .collect()
}
#[derive(Clone, Copy)]
pub enum RenderMode {
    Working,
    Final,
}
pub fn render_fields(
    fields: &AdoptedFields,
    body: &str,
    mode: RenderMode,
) -> Result<String, ItemError> {
    let mut lines = Vec::new();
    for (key, field) in fields {
        let node = match field {
            AdoptedField::Set { elements } => {
                if elements.is_empty() {
                    continue;
                }
                Value::Seq(
                    elements
                        .iter()
                        .map(|s| parse(s).map(|n| n.value))
                        .collect::<Result<_, _>>()?,
                )
            }
            AdoptedField::Reg { values, in_text } => {
                let values = match mode {
                    RenderMode::Working => &values[..*in_text],
                    RenderMode::Final => values.as_slice(),
                };
                if let [sole] = values {
                    value_node(sole, true)?
                } else {
                    let mut distinct = values.to_vec();
                    distinct.sort();
                    distinct.dedup();
                    value_node(&distinct.join(" / "), false)?
                }
            }
        };
        lines.push(render_entry(key, &node));
    }
    Ok(if lines.is_empty() {
        body.into()
    } else {
        format!("---\n{}\n---\n{body}", lines.join("\n"))
    })
}
