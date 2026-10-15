//! Port of streaming-reveal-content.ts; grapheme-aware content preserves opaque metadata.
use serde_json::Value;
use std::collections::BTreeMap;
use unicode_segmentation::UnicodeSegmentation;
#[derive(Default)]
pub struct BlockUnitCounter {
    entries: BTreeMap<usize, (String, usize, usize)>,
    slices: BTreeMap<usize, (String, usize, usize, usize)>,
}
impl BlockUnitCounter {
    pub fn count(&mut self, index: usize, text: &str) -> usize {
        if let Some((old, count, tail)) = self.entries.get_mut(&index) {
            if old == text {
                return *count;
            }
            if *count > 0 && text.len() > old.len() && text.starts_with(old.as_str()) {
                let mut n = *count - 1;
                let mut last = *tail;
                for (i, _) in text[*tail..].grapheme_indices(true) {
                    n += 1;
                    last = *tail + i;
                }
                // Extend the cached copy in place instead of re-copying the whole block.
                old.push_str(&text[old.len()..]);
                *count = n;
                *tail = last;
                return n;
            }
        }
        let mut n = 0;
        let mut last = 0;
        for (i, _) in text.grapheme_indices(true) {
            n += 1;
            last = i;
        }
        self.entries.insert(index, (text.into(), n, last));
        n
    }
    pub fn slice(&mut self, index: usize, text: &str, units: usize) -> String {
        if units == 0 || text.is_empty() {
            return String::new();
        }
        if let Some((old, n, end, last)) = self.slices.get_mut(&index) {
            if old == text && *n == units {
                return text[..*end].into();
            }
            // The text only grew and more units are wanted: resume from the last counted
            // grapheme (it may have extended) instead of rescanning from the start.
            if *n > 0 && units >= *n && text.len() >= old.len() && text.starts_with(old.as_str()) {
                let mut count = *n - 1;
                let mut new_end = *last;
                let mut new_last = *last;
                for (i, g) in text[*last..].grapheme_indices(true).take(units - count) {
                    count += 1;
                    new_last = *last + i;
                    new_end = new_last + g.len();
                }
                old.push_str(&text[old.len()..]);
                *n = count;
                *end = new_end;
                *last = new_last;
                return text[..new_end].into();
            }
        }
        let mut count = 0;
        let mut end = 0;
        let mut last = 0;
        for (i, g) in text.grapheme_indices(true).take(units) {
            count += 1;
            last = i;
            end = i + g.len();
        }
        self.slices.insert(index, (text.into(), count, end, last));
        text[..end].into()
    }
    pub fn reset(&mut self) {
        self.entries.clear();
        self.slices.clear();
    }
}
fn field(block: &Value, hide: bool) -> Option<&'static str> {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => Some("text"),
        Some("thinking") if !hide => Some("thinking"),
        _ => None,
    }
}
pub fn count_visible_units(message: &Value, hide: bool, counter: &mut BlockUnitCounter) -> usize {
    message
        .get("content")
        .and_then(Value::as_array)
        .map_or(0, |blocks| {
            blocks
                .iter()
                .enumerate()
                .map(|(i, b)| {
                    field(b, hide)
                        .and_then(|k| b.get(k))
                        .and_then(Value::as_str)
                        .map_or(0, |s| counter.count(i, s))
                })
                .sum()
        })
}
pub fn visible_units(message: &Value, hide: bool) -> usize {
    count_visible_units(message, hide, &mut BlockUnitCounter::default())
}
pub fn build_display_message(
    target: &Value,
    revealed: usize,
    hide: bool,
    counter: &mut BlockUnitCounter,
) -> Value {
    let Some(fields) = target.as_object() else {
        return target.clone();
    };
    // Copy the target field by field so a partially revealed block's full text is never cloned
    // only to be overwritten by its slice.
    let mut display = serde_json::Map::with_capacity(fields.len());
    let mut remaining = revealed;
    for (name, value) in fields {
        let Some(blocks) = value.as_array().filter(|_| name == "content") else {
            display.insert(name.clone(), value.clone());
            continue;
        };
        let mut shown = Vec::with_capacity(blocks.len());
        for (i, block) in blocks.iter().enumerate() {
            let sliced = match field(block, hide)
                .and_then(|key| block.get(key).and_then(Value::as_str).map(|text| (key, text)))
            {
                Some((key, text)) => {
                    let units = counter.count(i, text);
                    let sliced = (remaining < units).then(|| (key, counter.slice(i, text, remaining)));
                    remaining = remaining.saturating_sub(units);
                    sliced
                }
                None => None,
            };
            match (sliced, block.as_object()) {
                (Some((key, sliced)), Some(block_fields)) => {
                    let mut copy = serde_json::Map::with_capacity(block_fields.len());
                    let mut sliced = Some(sliced);
                    for (block_key, block_value) in block_fields {
                        let value = match sliced.take_if(|_| block_key == key) {
                            Some(text) => Value::String(text),
                            None => block_value.clone(),
                        };
                        copy.insert(block_key.clone(), value);
                    }
                    shown.push(Value::Object(copy));
                }
                _ => shown.push(block.clone()),
            }
        }
        display.insert(name.clone(), Value::Array(shown));
    }
    Value::Object(display)
}
