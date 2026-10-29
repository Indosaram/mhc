use super::{
    ItemError,
    document::is_live,
    frontmatter::{emit_frontmatter, value_node},
    history::analyze_history,
    state::{CombinedValue, combine_key, read_key_state},
    yaml::{Value, parse},
};
use loro::LoroDoc;
pub type FrontmatterEntry = (String, Value);
pub fn body_of(doc: &LoroDoc) -> String {
    doc.get_text("body").to_string()
}
pub fn frontmatter_entries(doc: &LoroDoc) -> Result<Vec<FrontmatterEntry>, ItemError> {
    let history = analyze_history(doc);
    let mut keys: Vec<_> = history.keys.iter().collect();
    keys.sort_by_key(|(_, kh)| kh.first);
    let mut entries = Vec::new();
    for (key, kh) in keys {
        let node = match combine_key(&read_key_state(doc, key), key, kh, &history) {
            Some(CombinedValue::Set { elements }) => Value::Seq(
                elements
                    .iter()
                    .map(|s| parse(s).map(|n| n.value))
                    .collect::<Result<_, _>>()?,
            ),
            Some(CombinedValue::Reg { text, verbatim }) => value_node(&text, verbatim)?,
            None => continue,
        };
        entries.push((key.clone(), node));
    }
    Ok(entries)
}
pub fn render_doc_content(doc: &LoroDoc) -> Result<String, ItemError> {
    let entries = frontmatter_entries(doc)?;
    let body = body_of(doc);
    Ok(if entries.is_empty() {
        body
    } else {
        format!("---\n{}\n---\n{body}", emit_frontmatter(&entries))
    })
}
pub fn render_item(doc: &LoroDoc) -> Result<Option<String>, ItemError> {
    if is_live(doc) {
        render_doc_content(doc).map(Some)
    } else {
        Ok(None)
    }
}
