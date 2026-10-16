use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const MAX_REPLAYED_HISTORY_IMAGES: usize = 8;

#[derive(Clone)]
enum Origin {
    User,
    Tool { name: String, id: String },
}

impl Origin {
    fn source(&self) -> String {
        match self {
            Self::User => "attached by the user".into(),
            Self::Tool { name, id } => format!("returned by the {name} tool, id={id}"),
        }
    }
    fn subject(&self) -> String {
        match self { Self::User => "[image attached by the user".into(), Self::Tool { name, .. } => format!("[image returned by the {name} tool") }
    }
}

struct Image {
    message: usize,
    entry: usize,
    answered: bool,
    origin: Origin,
    block: Value,
    hash: Option<String>,
    data: Option<String>,
}

fn canonical_base64(data: &str) -> Option<(String, String)> {
    let compact: String = data.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if compact.is_empty() || compact.trim_end_matches('=').len() % 4 == 1
        || !compact.bytes().all(|b| b.is_ascii_alphanumeric() || b"+/_-=".contains(&b))
        || compact.bytes().filter(|b| *b == b'=').count() > 2
    { return None; }
    let normalized = compact.trim_end_matches('=').replace('-', "+").replace('_', "/");
    let mut bytes = Vec::new();
    let mut accumulator = 0u32;
    let mut bits = 0;
    for byte in normalized.bytes() {
        let value = match byte { b'A'..=b'Z' => byte-b'A', b'a'..=b'z' => byte-b'a'+26, b'0'..=b'9' => byte-b'0'+52, b'+' => 62, b'/' => 63, _ => return None };
        accumulator = (accumulator << 6) | u32::from(value); bits += 6;
        if bits >= 8 { bits -= 8; bytes.push((accumulator >> bits) as u8); }
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));
    const TABLE: &[u8;64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        encoded.push(TABLE[((n >> 18) & 63) as usize] as char); encoded.push(TABLE[((n >> 12) & 63) as usize] as char);
        encoded.push(if chunk.len()>1 { TABLE[((n >> 6)&63) as usize] as char } else {'='}); encoded.push(if chunk.len()>2 { TABLE[(n&63) as usize] as char } else {'='});
    }
    Some((digest, encoded))
}

fn sdk_name(name: &str, custom: &std::collections::BTreeMap<String,String>) -> String {
    custom.get(name).cloned().unwrap_or_else(|| crate::tools::map_pi_tool_name(name, custom))
}

pub fn replay_history_images(messages: &[Value], custom: &std::collections::BTreeMap<String,String>, in_progress_turn: bool) -> std::collections::BTreeMap<usize, Value> {
    let answer_boundary = if in_progress_turn { messages.iter().rposition(|m|m["role"]=="user").unwrap_or(0) } else { messages.iter().rposition(|m|m["role"]=="assistant").unwrap_or(0) };
    let mut images = Vec::new();
    for (mi, message) in messages.iter().enumerate() {
        let origin = match message["role"].as_str() {
            Some("user") => Origin::User,
            Some("toolResult") => Origin::Tool { name: sdk_name(message["toolName"].as_str().unwrap_or("tool"),custom), id: message["toolCallId"].as_str().unwrap_or("").into() },
            _ => continue,
        };
        let Some(content) = message["content"].as_array() else {continue};
        for (ei, block) in content.iter().enumerate() {
            if block["type"]!="image" || !matches!(block["mimeType"].as_str(),Some("image/jpeg"|"image/png"|"image/gif"|"image/webp")) {continue;}
            let data=block["data"].as_str().map(str::to_owned);let decoded=data.as_deref().and_then(canonical_base64);
            images.push(Image {message:mi,entry:ei,answered:mi<answer_boundary,origin:origin.clone(),block:block.clone(),hash:decoded.as_ref().map(|v|v.0.clone()),data:decoded.map(|v|v.1)});
        }
    }
    if images.is_empty() {return Default::default()}
    let mut ranking=std::collections::BTreeMap::<String,(bool,usize)>::new();
    for (index,image) in images.iter().enumerate() {
        if image.answered && matches!(image.origin,Origin::Tool{..}) {continue}
        if let Some(hash)=&image.hash { let old=ranking.get(hash);ranking.insert(hash.clone(),(old.is_some_and(|v|v.0)||matches!(image.origin,Origin::User),index)); }
    }
    let mut ranked:Vec<_>=ranking.iter().map(|(hash,(user,index))|(hash.clone(),*user,*index)).collect();ranked.sort_by_key(|(_,user,index)|(std::cmp::Reverse(*user),std::cmp::Reverse(*index)));
    let allowed:std::collections::BTreeSet<_>=ranked.into_iter().take(MAX_REPLAYED_HISTORY_IMAGES).map(|x|x.0).collect();
    let mut shown=std::collections::BTreeMap::<String,Origin>::new();let mut replacements=std::collections::BTreeMap::<usize,std::collections::BTreeMap<usize,Vec<Value>>>::new();
    for image in images {
        let replacement=if let Some(hash)=image.hash.as_ref() {
            if matches!(image.origin,Origin::Tool{..})&&image.answered&&!shown.contains_key(hash) {
                let Origin::Tool{name,id}= &image.origin else {unreachable!()};vec![json!({"type":"text","text":format!("[image returned earlier by the {name} tool, id={id}: already seen earlier in this conversation; run the tool again if you need it]")})]
            } else if !allowed.contains(hash)&&!shown.contains_key(hash) {
                vec![json!({"type":"text","text":format!("{}: omitted from this replay, which re-sends at most {MAX_REPLAYED_HISTORY_IMAGES} earlier images; re-read the source if you need it]",image.origin.subject())})]
            } else if let Some(first)=shown.get(hash) {
                vec![json!({"type":"text","text":format!("{}: identical to an image already shown above ({}); not attached again]",image.origin.subject(),first.source())})]
            } else {
                shown.insert(hash.clone(),image.origin.clone());
                let block=if let Some(data)=image.data {let mut block=image.block;block["data"]=json!(data);block}else {image.block};
                let prefix=match &image.origin {Origin::Tool{name,..}=>Some(format!("[image returned by the {name} tool to your own call (tool output, not a user attachment)]")),Origin::User if image.answered=>Some("[image the user attached in this earlier turn (history, not part of the current message)]".into()),_=>None};
                let mut values=Vec::new();if let Some(text)=prefix {values.push(json!({"type":"text","text":text}));}values.push(block);values
            }
        } else { vec![json!({"type":"text","text":format!("{}: omitted because its image data is missing or unreadable]",image.origin.subject())})] };
        replacements.entry(image.message).or_default().insert(image.entry,replacement);
    }
    replacements.into_iter().map(|(mi,entries)| {let content=messages[mi]["content"].as_array().expect("image content");let rewritten:Vec<Value>=content.iter().enumerate().flat_map(|(ei,value)|entries.get(&ei).cloned().unwrap_or_else(||vec![value.clone()])).collect();(mi,json!(rewritten))}).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn image(data:&str)->Value {json!({"type":"image","mimeType":"image/png","data":data})}
    #[test]
    fn history_images_carry_origin_deduplicate_and_omit_answered_tool_images() {
        let messages=vec![json!({"role":"user","content":[image("aW1hZ2U=")]}),json!({"role":"assistant","content":[]}),json!({"role":"toolResult","toolName":"read","toolCallId":"c1","content":[image("aW1hZ2U=")]}),json!({"role":"user","content":[image("bmV3")]}),json!({"role":"assistant","content":[]})];
        let replay=replay_history_images(&messages,&Default::default(),false);
        assert!(replay[&0][0]["text"].as_str().unwrap().contains("earlier turn"));
        assert!(replay[&2][0]["text"].as_str().unwrap().contains("identical to an image already shown"));
        assert!(replay[&3].as_array().expect("replayed content").iter().any(|block|block["type"]=="image"));
    }
    #[test]
    fn replay_caps_distinct_images_and_labels_tool_outputs() {
        let data=["AQ==","Ag==","Aw==","BA==","BQ==","Bg==","Bw==","CA==","CQ==","Cg=="];
        let messages:Vec<_>=data.iter().map(|value|json!({"role":"user","content":[image(value)]})).collect();
        let replay=replay_history_images(&messages,&Default::default(),false);let attached=replay.values().flat_map(|content|content.as_array().expect("content")).filter(|block|block["type"]=="image").count();assert_eq!(attached,MAX_REPLAYED_HISTORY_IMAGES);
        let tool=vec![json!({"role":"toolResult","toolName":"read","toolCallId":"x","content":[image("aW1hZ2U=")]}),json!({"role":"user","content":"next"})];
        let replay=replay_history_images(&tool,&Default::default(),false);assert!(replay[&0][0]["text"].as_str().unwrap().contains("tool output, not a user attachment"));
    }
}
