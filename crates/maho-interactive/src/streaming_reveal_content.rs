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
        if let Some((old, count, tail)) = self.entries.get(&index) {
            if old == text {
                return *count;
            }
            if *count > 0 && text.len() > old.len() && text.starts_with(old) {
                let mut n = count - 1;
                let mut last = *tail;
                for (i, _) in text[*tail..].grapheme_indices(true) {
                    n += 1;
                    last = tail + i;
                }
                self.entries.insert(index, (text.into(), n, last));
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
        if let Some((old, n, end, _)) = self.slices.get(&index)
            && old == text
            && *n == units
        {
            return text[..*end].into();
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
    let mut display = target.clone();
    let mut remaining = revealed;
    if let Some(blocks) = display.get_mut("content").and_then(Value::as_array_mut) {
        for (i, block) in blocks.iter_mut().enumerate() {
            if let Some(key) = field(block, hide)
                && let Some(text) = block.get(key).and_then(Value::as_str)
            {
                let units = counter.count(i, text);
                if remaining < units {
                    let sliced = counter.slice(i, text, remaining);
                    block[key] = Value::String(sliced);
                }
                remaining = remaining.saturating_sub(units);
            }
        }
    }
    display
}
