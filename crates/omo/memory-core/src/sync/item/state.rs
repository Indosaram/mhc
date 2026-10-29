use super::{
    frontmatter::sequence_elements,
    history::{FmPart, ItemHistory, KeyHistory, Stamp},
    yaml::{Value, parse},
};
use loro::{LoroDoc, ToJson};
use std::collections::BTreeSet;
pub const BODY_NAME: &str = "body";
pub const TOMBSTONES_NAME: &str = "tombstones";
pub fn fm_name(key: &str, part: FmPart) -> String {
    let mut encoded = String::new();
    for byte in key.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("fm\u{1f}{encoded}\u{1f}{}", part.name())
}
#[derive(Clone, Debug)]
pub struct RegEntry {
    pub tag: String,
    pub value: Option<String>,
    pub supersedes: Vec<String>,
}
#[derive(Clone, Debug, Default)]
pub struct KeyState {
    pub set: Vec<(String, String)>,
    pub reg: Vec<RegEntry>,
}
pub fn read_key_state(doc: &LoroDoc, key: &str) -> KeyState {
    let mut state = KeyState::default();
    let set = doc
        .get_map(fm_name(key, FmPart::Set))
        .get_deep_value()
        .to_json_value();
    if let Some(map) = set.as_object() {
        for (tag, v) in map {
            if let Some(element) = v.as_str() {
                state.set.push((tag.clone(), element.into()));
            }
        }
    }
    let reg = doc
        .get_map(fm_name(key, FmPart::Reg))
        .get_deep_value()
        .to_json_value();
    if let Some(map) = reg.as_object() {
        for (tag, v) in map {
            let Some(value) = v.get("v") else { continue };
            if !value.is_null() && !value.is_string() {
                continue;
            }
            let Some(s) = v.get("s").and_then(|v| v.as_array()) else {
                continue;
            };
            let Some(supersedes) = s
                .iter()
                .map(|v| v.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            state.reg.push(RegEntry {
                tag: tag.clone(),
                value: value.as_str().map(str::to_owned),
                supersedes,
            });
        }
    }
    // Map entry traversal is not an operation ordering contract. Writers consume
    // these vectors too, so normalize before deletes and supersede-list writes.
    state.set.sort_by(|a, b| a.0.cmp(&b.0));
    state.reg.sort_by(|a, b| a.tag.cmp(&b.tag));
    state
}
pub fn active_reg(entries: &[RegEntry]) -> Vec<&RegEntry> {
    let superseded: BTreeSet<_> = entries.iter().flat_map(|e| &e.supersedes).collect();
    entries
        .iter()
        .filter(|e| e.value.is_some() && !superseded.contains(&e.tag))
        .collect()
}
pub fn register_values(entries: &[RegEntry]) -> Vec<String> {
    active_reg(entries)
        .iter()
        .filter_map(|e| e.value.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
pub fn reg_text(text: &str) -> String {
    match parse(text).map(|n| n.value) {
        Ok(Value::String(s)) => s,
        _ => text.into(),
    }
}
fn add_stamp(history: &ItemHistory, key: &str, part: FmPart, tag: &str) -> Stamp {
    let book = match part {
        FmPart::Set => &history.set_adds,
        FmPart::Reg => &history.reg_adds,
    };
    book.get(key)
        .and_then(|b| b.get(tag))
        .copied()
        .or_else(|| {
            history
                .keys
                .get(key)
                .and_then(|k| k.by_kind.get(&part))
                .copied()
        })
        .unwrap_or(Stamp {
            lamport: u64::MAX,
            peer: 0,
        })
}
pub fn ordered_set_elements(state: &KeyState, key: &str, history: &ItemHistory) -> Vec<String> {
    ordered(
        state
            .set
            .iter()
            .map(|(tag, e)| (add_stamp(history, key, FmPart::Set, tag), e.clone()))
            .collect(),
    )
}
fn ordered(mut values: Vec<(Stamp, String)>) -> Vec<String> {
    values.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.encode_utf16().cmp(b.1.encode_utf16()))
    });
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .filter_map(|(_, v)| {
            if seen.insert(v.clone()) {
                Some(v)
            } else {
                None
            }
        })
        .collect()
}
#[derive(Clone, Debug)]
pub enum CombinedValue {
    Reg { text: String, verbatim: bool },
    Set { elements: Vec<String> },
}
pub fn combine_key(
    state: &KeyState,
    key: &str,
    kh: &KeyHistory,
    history: &ItemHistory,
) -> Option<CombinedValue> {
    match kh.kind {
        FmPart::Reg => {
            let values = register_values(&state.reg);
            let sole = values.len() == 1 && state.set.is_empty();
            let parts: BTreeSet<_> = values
                .into_iter()
                .chain(state.set.iter().map(|(_, e)| e.clone()))
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(CombinedValue::Reg {
                    text: parts.into_iter().collect::<Vec<_>>().join(" / "),
                    verbatim: sole,
                })
            }
        }
        FmPart::Set => {
            let mut tagged: Vec<_> = state
                .set
                .iter()
                .map(|(tag, e)| (add_stamp(history, key, FmPart::Set, tag), e.clone()))
                .collect();
            for entry in active_reg(&state.reg) {
                if let Some(value) = &entry.value {
                    let stamp = add_stamp(history, key, FmPart::Reg, &entry.tag);
                    for source in sequence_elements(value).unwrap_or_else(|| vec![value.clone()]) {
                        tagged.push((stamp, source));
                    }
                }
            }
            let elements = ordered(tagged);
            if elements.is_empty() {
                None
            } else {
                Some(CombinedValue::Set { elements })
            }
        }
    }
}
