use super::{
    ItemError,
    frontmatter::{FmValue, emit_frontmatter, split_header, split_item, without_bom},
    render::{body_of, frontmatter_entries, render_item},
    yaml::{Node, Value, parse},
};
use loro::LoroDoc;
use std::collections::{BTreeMap, BTreeSet};

#[derive(PartialEq, Eq)]
struct Model {
    fields: BTreeMap<String, String>,
    body: String,
}
fn model(text: &str) -> Model {
    let parsed = split_item(text);
    let fields = parsed
        .fm
        .unwrap_or_default()
        .into_iter()
        .map(|e| {
            let value = match e.value {
                FmValue::Set { mut elements, .. } => {
                    elements.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
                    serde_json::json!(["set", elements]).to_string()
                }
                FmValue::Reg { value } => serde_json::json!(["reg", value]).to_string(),
                FmValue::Remove => "remove".into(),
            };
            (e.key, value)
        })
        .collect();
    Model {
        fields,
        body: parsed.body,
    }
}
pub fn render_for_disk(doc: &LoroDoc, disk_text: &str) -> Result<Option<String>, ItemError> {
    let Some(canonical) = render_item(doc)? else {
        return Ok(None);
    };
    let want = model(&canonical);
    if want == model(disk_text) {
        return Ok(Some(disk_text.into()));
    }
    let disk = without_bom(disk_text);
    let bom = &disk_text[..disk_text.len() - disk.len()];
    let eol = eol_of(disk);
    if let Ok(Some(patched)) = patch_disk(doc, disk, &want)
        && model(&patched) == want
    {
        return Ok(Some(format!("{bom}{patched}")));
    }
    let styled = with_eol(&canonical, eol);
    Ok(Some(format!(
        "{bom}{}",
        if model(&styled) == want {
            styled
        } else {
            canonical
        }
    )))
}
fn eol_of(text: &str) -> &'static str {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count() - crlf;
    if crlf > lf
        || (crlf == lf
            && text
                .find('\n')
                .is_some_and(|n| n > 0 && text.as_bytes()[n - 1] == b'\r'))
    {
        "\r\n"
    } else {
        "\n"
    }
}
fn with_eol(text: &str, eol: &str) -> String {
    match split_header(text) {
        Some((header, body)) if eol != "\n" => {
            format!("---{eol}{}{eol}---{eol}{body}", header.replace('\n', eol))
        }
        _ => text.into(),
    }
}
struct PairSpan {
    key: String,
    start: usize,
    end: usize,
}
fn pair_spans(node: &Node, source: &str) -> Option<Vec<PairSpan>> {
    let mut spans = Vec::new();
    for (key, value) in &node.pairs {
        let Value::String(key_text) = &key.value else {
            return None;
        };
        let start = key.start;
        let mut end = value.end;
        if !source.is_char_boundary(start) || !source.is_char_boundary(end) {
            return None;
        }
        if end > 0 && source.as_bytes().get(end - 1) != Some(&b'\n') {
            let tail = source.get(end..)?;
            let comment_end = tail.find('\n').map(|n| end + n + 1).unwrap_or(source.len());
            if tail[..comment_end - end].trim().is_empty() || tail.trim_start().starts_with('#') {
                end = comment_end;
            }
        }
        if start > end
            || end > source.len()
            || spans.last().is_some_and(|s: &PairSpan| s.end > start)
        {
            return None;
        }
        spans.push(PairSpan {
            key: key_text.clone(),
            start,
            end,
        });
    }
    Some(spans)
}
fn patch_disk(doc: &LoroDoc, disk: &str, want: &Model) -> Result<Option<String>, ItemError> {
    let Some((_, body)) = split_header(disk) else {
        return Ok(None);
    };
    let entries = frontmatter_entries(doc)?;
    if entries.is_empty() {
        return Ok(None);
    }
    let have = model(disk);
    if have.fields == want.fields {
        return Ok(Some(format!(
            "{}{}",
            &disk[..disk.len() - body.len()],
            body_of(doc)
        )));
    }
    let open = if disk.starts_with("---\r\n") { 5 } else { 4 };
    let Some(end) = disk[open..].find("\n---").map(|n| n + open) else {
        return Ok(None);
    };
    let source = &disk[open..end + 1];
    let parsed = parse(source)?;
    if !matches!(parsed.value, Value::Map(_)) {
        return Ok(None);
    }
    let mut changed: BTreeSet<_> = have
        .fields
        .keys()
        .chain(want.fields.keys())
        .filter(|k| have.fields.get(*k) != want.fields.get(*k))
        .cloned()
        .collect();
    for (key, _) in &parsed.pairs {
        if key.value == Value::String("<<".into()) {
            return Ok(None);
        }
    }
    loop {
        let mut lost = BTreeSet::new();
        for (key, value) in &parsed.pairs {
            if let Value::String(key_text) = &key.value
                && changed.contains(key_text)
            {
                lost.extend(&key.anchors);
                lost.extend(&value.anchors);
            }
        }
        let count = changed.len();
        for (key, value) in &parsed.pairs {
            if let Value::String(key) = &key.value
                && !value.aliases.is_disjoint(&lost)
            {
                changed.insert(key.clone());
            }
        }
        if changed.len() == count {
            break;
        }
    }
    let Some(spans) = pair_spans(&parsed, source) else {
        return Ok(None);
    };
    let eol = eol_of(disk);
    let nodes: BTreeMap<_, _> = entries.iter().map(|(k, v)| (k.clone(), v)).collect();
    let emit = |key: &str, value: &Value| {
        format!(
            "{}{eol}",
            emit_frontmatter(&[(key.into(), value.clone())]).replace('\n', eol)
        )
    };
    let mut out = String::new();
    let mut cursor = 0;
    for span in &spans {
        if !changed.contains(&span.key) {
            continue;
        }
        if let Some(node) = nodes.get(&span.key) {
            out.push_str(&source[cursor..span.start]);
            out.push_str(&emit(&span.key, node));
        } else {
            let mut from = span.start;
            while from > cursor {
                let start = source[..from.saturating_sub(1)]
                    .rfind('\n')
                    .map_or(0, |n| n + 1);
                if !source[start..from].starts_with('#') {
                    break;
                }
                from = start;
            }
            out.push_str(&source[cursor..from]);
        }
        cursor = span.end;
    }
    let tail = spans.last().map_or(0, |s| s.end);
    if cursor <= tail {
        out.push_str(&source[cursor..tail]);
        cursor = tail;
    }
    for (key, value) in &entries {
        if !spans.iter().any(|s| s.key == *key) {
            out.push_str(&emit(key, value));
        }
    }
    out.push_str(&source[cursor..]);
    let body_start = disk.len() - body.len();
    Ok(Some(format!(
        "{}{}{}{}",
        &disk[..open],
        out,
        &disk[end + 1..body_start],
        body_of(doc)
    )))
}
