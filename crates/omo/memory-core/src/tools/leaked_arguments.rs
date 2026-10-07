//! Repair of memory-tool arguments that leaked into a sibling text argument.

use std::collections::BTreeMap;

use super::memory::MemoryToolParams;

/// The memory tool's free-text arguments (pin `leaked-arguments.ts` `TEXT_ARGUMENTS`).
pub const TEXT_ARGUMENTS: [&str; 9] = [
    "file_path",
    "old_path",
    "new_path",
    "old_string",
    "new_string",
    "insert_text",
    "description",
    "file_text",
    "reason",
];

/// One repaired leak: `to` arrived inside `from`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeakedArgumentRepair {
    pub from: String,
    pub to: String,
}

/// Repaired parameters plus the repairs that produced them.
#[derive(Debug, Clone)]
pub struct RepairedMemoryToolParams {
    pub params: MemoryToolParams,
    pub repairs: Vec<LeakedArgumentRepair>,
}

struct Leak {
    index: usize,
    length: usize,
    name: String,
}

/// Splits arguments that leaked into a sibling back apart, repeating until no repair applies.
pub fn repair_leaked_arguments(params: MemoryToolParams) -> RepairedMemoryToolParams {
    let mut text: BTreeMap<String, String> = BTreeMap::new();
    for name in TEXT_ARGUMENTS {
        if let Some(value) = text_value(&params, name) {
            text.insert(name.to_string(), value);
        }
    }

    let mut repairs = Vec::new();
    let mut changed = true;
    while changed {
        changed = false;
        for from in TEXT_ARGUMENTS {
            let Some(value) = text.get(from).cloned() else {
                continue;
            };
            let Some(leak) = find_leak(&value, from) else {
                continue;
            };
            let to = leak.name;
            if !is_text_argument(&to) || to == from || text.contains_key(&to) {
                continue;
            }
            text.insert(from.to_string(), value[..leak.index].to_string());
            text.insert(to.clone(), value[leak.index + leak.length..].to_string());
            repairs.push(LeakedArgumentRepair {
                from: from.to_string(),
                to,
            });
            changed = true;
        }
    }

    if repairs.is_empty() {
        return RepairedMemoryToolParams { params, repairs };
    }
    let mut params = params;
    for (name, value) in &text {
        set_text_value(&mut params, name, value.clone());
    }
    RepairedMemoryToolParams { params, repairs }
}

/// Tells the model its call was repaired, so the next call closes every argument with `</parameter>`.
pub fn describe_repairs(repairs: &[LeakedArgumentRepair]) -> String {
    repairs
        .iter()
        .map(|repair| {
            format!(
                "\nNote: '{}' arrived inside '{}' because '{}' was closed with </{}> instead of </parameter>; it was split back out before writing.",
                repair.to, repair.from, repair.from, repair.from
            )
        })
        .collect()
}

fn is_text_argument(name: &str) -> bool {
    TEXT_ARGUMENTS.contains(&name)
}

fn find_leak(value: &str, from: &str) -> Option<Leak> {
    let closing = format!("</{from}>");
    let marker = "<parameter name=\"";
    let mut search = 0usize;
    while let Some(relative) = value[search..].find(&closing) {
        let index = search + relative;
        let after_closing = &value[index + closing.len()..];
        let trimmed = after_closing.trim_start();
        let whitespace = after_closing.len() - trimmed.len();
        if let Some(after_marker) = trimmed.strip_prefix(marker)
            && let Some(quote) = after_marker.find('"')
        {
            let name = &after_marker[..quote];
            let tail = &after_marker[quote..];
            if tail.starts_with("\">")
                && !name.is_empty()
                && name
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch == '_')
            {
                return Some(Leak {
                    index,
                    length: closing.len() + whitespace + marker.len() + name.len() + 2,
                    name: name.to_string(),
                });
            }
        }
        search = index + closing.len();
    }
    None
}

fn text_value(params: &MemoryToolParams, name: &str) -> Option<String> {
    match name {
        "file_path" => params.file_path.clone(),
        "old_path" => params.old_path.clone(),
        "new_path" => params.new_path.clone(),
        "old_string" => params.old_string.clone(),
        "new_string" => params.new_string.clone(),
        "insert_text" => params.insert_text.clone(),
        "description" => params.description.clone(),
        "file_text" => params.file_text.clone(),
        "reason" => Some(params.reason.clone()),
        _ => None,
    }
}

fn set_text_value(params: &mut MemoryToolParams, name: &str, value: String) {
    match name {
        "file_path" => params.file_path = Some(value),
        "old_path" => params.old_path = Some(value),
        "new_path" => params.new_path = Some(value),
        "old_string" => params.old_string = Some(value),
        "new_string" => params.new_string = Some(value),
        "insert_text" => params.insert_text = Some(value),
        "description" => params.description = Some(value),
        "file_text" => params.file_text = Some(value),
        "reason" => params.reason = value,
        _ => {}
    }
}
