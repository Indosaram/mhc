use std::collections::{BTreeMap, BTreeSet};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use super::document::ToolSearchSource;

pub const TOOL_SEARCH_ACTIVATION_MARKER_V2: &str = "[tool_search:activated:v2]";
pub const LEGACY_TOOL_SEARCH_ACTIVATION_MARKER: &str = "[tool_search:activated]";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolActivationIdentity { pub name: String, pub registration_id: String }
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParsedActivationMarker { V2(Vec<ToolActivationIdentity>), V1(Vec<String>) }
pub struct RehydratableToolSearchDocument { pub registration_id: String, pub source: ToolSearchSource, pub allow_lazy_activation: Option<bool> }
fn js_whitespace(character:char)->bool { matches!(character,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}') }

pub fn derive_mcp_registration_id(server: &str, name: &str) -> String { format!("mcp\0{server}\0{name}") }
pub fn derive_extension_registration_id(path: &str, resolved_path: Option<&str>, name: &str) -> String { format!("{}\0{name}", resolved_path.unwrap_or(path)) }
pub fn emit_activation_marker(activations: &[ToolActivationIdentity]) -> Result<String, serde_json::Error> { Ok(format!("{TOOL_SEARCH_ACTIVATION_MARKER_V2} {}", serde_json::to_string(activations)?)) }

fn array_prefix(text: &str) -> Option<&str> {
    if !text.starts_with('[') { return None; }
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i,c) in text.char_indices() {
        if in_string { if escaped { escaped=false; } else if c=='\\' { escaped=true; } else if c=='"' { in_string=false; } continue; }
        match c { '"'=>in_string=true, '['=>depth+=1, ']'=> { depth=depth.checked_sub(1)?; if depth==0 { return Some(&text[..i+1]); } }, _=>{} }
    }
    None
}
pub fn parse_activation_markers(messages: &[Value]) -> Vec<ParsedActivationMarker> {
    let mut markers=Vec::new();
    for message in messages {
        let blob=message.to_string();
        for (offset,_) in blob.match_indices(TOOL_SEARCH_ACTIVATION_MARKER_V2) {
            let rest=&blob[offset+TOOL_SEARCH_ACTIVATION_MARKER_V2.len()..];
            let mut escaped=false;
            let mut end=None;
            for (i,c) in rest.char_indices() { if escaped { escaped=false; } else if c=='\\' { escaped=true; } else if c=='"' { end=Some(i); break; } else if c=='\n' { break; } }
            let Some(end)=end else { continue; };
            let Ok(decoded)=serde_json::from_str::<String>(&format!("\"{}\"", &rest[..end])) else { continue; };
            let Some(payload)=array_prefix(decoded.trim_start_matches(js_whitespace)) else { continue; };
            let Ok(values)=serde_json::from_str::<Vec<Value>>(payload) else { continue; };
            let activations:Vec<_>=values.into_iter().filter_map(|v| serde_json::from_value::<ToolActivationIdentity>(v).ok()).filter(|v| !v.name.is_empty() && !v.registration_id.is_empty()).collect();
            if !activations.is_empty() { markers.push(ParsedActivationMarker::V2(activations)); }
        }
        for (offset,_) in blob.match_indices(LEGACY_TOOL_SEARCH_ACTIVATION_MARKER) {
            let rest=&blob[offset+LEGACY_TOOL_SEARCH_ACTIVATION_MARKER.len()..];
            let end=rest.find(['"','\\','\n']).unwrap_or(rest.len());
            let names:Vec<_>=rest[..end].split(js_whitespace).filter(|name|!name.is_empty()).map(str::to_owned).collect();
            if !names.is_empty() { markers.push(ParsedActivationMarker::V1(names)); }
        }
    }
    markers
}
pub fn rehydrate(messages: &[Value], docs: &BTreeMap<String, RehydratableToolSearchDocument>) -> Vec<String> {
    let mut restored=BTreeSet::new();
    for marker in parse_activation_markers(messages) {
        match marker {
            ParsedActivationMarker::V2(activations)=>for a in activations { if docs.get(&a.name).is_some_and(|d| d.allow_lazy_activation!=Some(false) && d.registration_id==a.registration_id) { restored.insert(a.name); } },
            ParsedActivationMarker::V1(names)=>for name in names { if docs.get(&name).is_some_and(|d| d.source==ToolSearchSource::Mcp && d.allow_lazy_activation!=Some(false)) { restored.insert(name); } },
        }
    }
    let mut restored:Vec<_>=restored.into_iter().collect();
    restored.sort_by(|left,right|left.encode_utf16().cmp(right.encode_utf16()));
    restored
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test] fn identity_uses_owner() { assert_eq!(derive_mcp_registration_id("docs","read"),"mcp\0docs\0read"); assert_eq!(derive_extension_registration_id("alias",Some("canonical"),"read"),"canonical\0read"); }
    #[test] fn history_replay_sorts_names_by_utf16_units() {
        let names=["\u{e000}","\u{10000}"];
        let docs=names.iter().map(|name|((*name).into(),RehydratableToolSearchDocument{registration_id:(*name).into(),source:ToolSearchSource::Mcp,allow_lazy_activation:None})).collect();
        assert_eq!(rehydrate(&[json!(format!("[tool_search:activated] {} {}",names[0],names[1]))],&docs),["\u{10000}","\u{e000}"]);
    }
    #[test] fn v2_round_trip() { let a=ToolActivationIdentity { name:"read".into(),registration_id:"owner\0read".into() }; let message=json!({"content":[{"text":emit_activation_marker(std::slice::from_ref(&a)).unwrap()}]}); assert_eq!(parse_activation_markers(&[message]),vec![ParsedActivationMarker::V2(vec![a])]); }
    #[test] fn malformed_history_is_ignored() { assert!(parse_activation_markers(&[json!("[tool_search:activated:v2] [invalid]")]).is_empty()); }
    #[test] fn history_marker_uses_ecmascript_whitespace() { assert_eq!(parse_activation_markers(&[json!("[tool_search:activated]\u{feff}a\u{0085}b")]),vec![ParsedActivationMarker::V1(vec!["a\u{0085}b".into()])]); }
    #[test] fn legacy_stops_at_json_boundary() { assert_eq!(parse_activation_markers(&[json!({"text":"[tool_search:activated] a b","other":"c"})]),vec![ParsedActivationMarker::V1(vec!["a".into(),"b".into()])]); }
    #[test] fn ownership_and_lazy_gate() { let a=ToolActivationIdentity{name:"read".into(),registration_id:"old".into()}; let msg=json!(emit_activation_marker(&[a]).unwrap()); let mut docs=BTreeMap::new(); docs.insert("read".into(),RehydratableToolSearchDocument{registration_id:"new".into(),source:ToolSearchSource::Extension,allow_lazy_activation:None}); assert!(rehydrate(&[msg],&docs).is_empty()); assert!(rehydrate(&[json!("[tool_search:activated] read")],&docs).is_empty()); }
}
