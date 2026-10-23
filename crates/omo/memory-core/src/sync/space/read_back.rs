use super::{
    super::item::frontmatter::{FmValue, split_header, split_item, without_bom},
    hub_trace::is_t2_header,
};
use std::collections::BTreeMap;

pub fn to_lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}
pub fn normalize_input(text: &str) -> String {
    to_lf(text.strip_prefix('\u{feff}').unwrap_or(text))
}
pub fn lines_of(text: &str) -> Vec<&str> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.strip_suffix('\n')
            .unwrap_or(text)
            .split('\n')
            .collect()
    }
}
fn counts<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<(&'a str, usize)> {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for line in lines {
        if let Some((_, count)) = counts.iter_mut().find(|(s, _)| *s == line) {
            *count += 1;
        } else {
            counts.push((line, 1));
        }
    }
    counts
}
enum ResultFact {
    Reg(Vec<String>),
    Set(Vec<String>),
}
fn survives(wanted: &FmValue, actual: &ResultFact) -> bool {
    let held = match actual {
        ResultFact::Reg(v) | ResultFact::Set(v) => v,
    };
    match wanted {
        FmValue::Set { elements, flow } => match actual {
            ResultFact::Set(_) => elements.iter().all(|e| held.contains(e)),
            ResultFact::Reg(_) => held.contains(flow),
        },
        FmValue::Reg { value } => held.contains(value),
        FmValue::Remove => false,
    }
}
fn represented(line: &str, facts: &BTreeMap<String, ResultFact>) -> bool {
    [line.to_owned(), line.replace("-- >", "-->")]
        .iter()
        .any(|candidate| {
            let parsed = split_item(&format!("---\n{candidate}\n---\n"));
            match parsed.fm.as_deref() {
                Some([entry]) => facts
                    .get(&entry.key)
                    .is_some_and(|fact| survives(&entry.value, fact)),
                _ => false,
            }
        })
}
fn joined(line: &str, registers: &BTreeMap<String, Vec<String>>) -> bool {
    match split_item(&format!("---\n{line}\n---\n")).fm.as_deref() {
        Some([entry]) => registers.get(&entry.key).is_some_and(|v| v.len() > 1),
        _ => false,
    }
}
/// The result's lines that count as held text: a joined register line counts for nothing, but only
/// inside the result's own header; a body line is text, whatever key it looks like.
fn held_lines<'a>(rendered: &'a str, registers: &BTreeMap<String, Vec<String>>) -> Vec<&'a str> {
    let lines = lines_of(rendered);
    if split_item(rendered).fm.is_none() {
        return lines;
    }
    let Some((header, _)) = split_header(without_bom(rendered)) else {
        return lines;
    };
    let header_end = 1 + lines_of(&format!("{header}\n")).len();
    lines
        .into_iter()
        .enumerate()
        .filter(|(index, line)| *index < 1 || *index >= header_end || !joined(line, registers))
        .map(|(_, line)| line)
        .collect()
}
/// `registers` is required: the space's active components per register key; a caller with no doc
/// passes an empty map, which reads every rendered register as one whole value.
pub fn first_missing_fact(
    rendered: &str,
    source: &str,
    registers: &BTreeMap<String, Vec<String>>,
) -> Option<String> {
    let source_lines = lines_of(source);
    let have = counts(held_lines(rendered, registers));
    let headers = source_lines
        .iter()
        .filter(|line| is_t2_header(line))
        .count();
    let fences = usize::from(split_item(source).fm.is_some()) * 2 + headers * 2;
    let facts: BTreeMap<_, _> = split_item(rendered)
        .fm
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| {
            let fact = match entry.value {
                FmValue::Set { elements, .. } => ResultFact::Set(elements),
                FmValue::Reg { value } => ResultFact::Reg(
                    registers
                        .get(&entry.key)
                        .cloned()
                        .unwrap_or_else(|| vec![value]),
                ),
                FmValue::Remove => return None,
            };
            Some((entry.key, fact))
        })
        .collect();
    for (line, count) in counts(source_lines) {
        let held = have.iter().find(|(s, _)| *s == line).map_or(0, |(_, n)| *n);
        if held >= count || is_t2_header(line) {
            continue;
        }
        let missing = count - held;
        let allowed = match line {
            "-->" | "----->" | "" => Some(headers),
            "---" => Some(fences),
            _ => None,
        };
        if let Some(allowed) = allowed {
            if missing <= allowed {
                continue;
            }
            return Some(format!(
                "missing line {} ({missing} dropped, {allowed} allowed as metadata)",
                serde_json::Value::String(line.into())
            ));
        }
        if represented(line, &facts) {
            continue;
        }
        let quoted = serde_json::Value::String(line.into());
        return Some(if missing == count {
            format!("missing line {quoted}")
        } else {
            format!("missing line {quoted} ({count} in the source, {held} merged)")
        });
    }
    None
}
