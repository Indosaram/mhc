use serde_json::{Value,json};

pub fn append_sdk_content_blocks(blocks:&mut Vec<Value>,content:&Value) -> bool {
    if let Some(text)=content.as_str() {
        if !text.is_empty() { blocks.push(json!({"type":"text","text":text})); }
        return !text.trim().is_empty();
    }
    let mut has_text=false;
    if let Some(entries)=content.as_array() {
        for entry in entries { has_text=append_entry(blocks,entry) || has_text; }
    }
    has_text
}
fn placeholder(entry:&Value) -> String {
    match entry["type"].as_str() {
        Some("image")=>match (entry["mimeType"].as_str(),entry["data"].as_str()) {
            (Some(media),Some(_))=>format!("[image block omitted: unsupported media type {media}]"),
            _=>"[image block omitted: missing data]".into(),
        },
        Some(kind)=>format!("[unsupported content block omitted: {kind}]"),
        None=>"[unsupported content block omitted]".into(),
    }
}
fn append_entry(blocks:&mut Vec<Value>,entry:&Value) -> bool {
    if let Some(text)=entry.as_str().or_else(|| (entry["type"]=="text").then(|| entry["text"].as_str()).flatten()) {
        blocks.push(json!({"type":"text","text":text}));return !text.trim().is_empty();
    }
    if entry["type"]=="image" && let (Some(media),Some(data))=(entry["mimeType"].as_str(),entry["data"].as_str())
        && ["image/jpeg","image/png","image/gif","image/webp"].contains(&media) {
        blocks.push(json!({"type":"image","source":{"type":"base64","media_type":media,"data":data}}));return false;
    }
    blocks.push(json!({"type":"text","text":placeholder(entry)}));true
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_strings_are_text() {
        let mut blocks=Vec::new();assert!(append_sdk_content_blocks(&mut blocks,&json!([{"type":"text","text":"ok"},"\n(OmO) auto-formatted file"])));
        assert_eq!(blocks[1]["type"],"text");
    }
    #[test]
    fn well_formed_image() {
        let mut blocks=Vec::new();assert!(!append_sdk_content_blocks(&mut blocks,&json!([{"type":"image","mimeType":"image/png","data":"aW1hZ2U="}])));
        assert_eq!(blocks[0],json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"aW1hZ2U="}}));
    }
    #[test]
    fn malformed_and_unknown_blocks_are_text() {
        for entry in [json!({"type":"image","mimeType":"image/png"}),json!({"type":"tool_use","id":"x"}),json!({"type":"text","text":42}),json!({"type":"image","mimeType":"image/svg+xml","data":"PHN2Zz4="})] {
            let mut blocks=Vec::new();assert!(append_sdk_content_blocks(&mut blocks,&json!([entry])));assert_eq!(blocks[0]["type"],"text");
        }
    }
    #[test]
    fn empty_whole_text_skipped() { let mut blocks=Vec::new();assert!(!append_sdk_content_blocks(&mut blocks,&json!("")));assert!(blocks.is_empty()); }
    #[test]
    fn whitespace_does_not_count_as_text() { let mut blocks=Vec::new();assert!(!append_sdk_content_blocks(&mut blocks,&json!([{"type":"text","text":"   "}])));assert_eq!(blocks.len(),1); }
}
