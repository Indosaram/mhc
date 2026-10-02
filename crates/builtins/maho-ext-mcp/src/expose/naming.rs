use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
pub const MCP_TOOL_NAME_MAX_LENGTH: usize = 64;
#[derive(Debug, Clone)]
pub struct McpToolNameEntry { pub server_name: String, pub tool_name: String }
pub fn build_mcp_tool_names(entries: &[McpToolNameEntry], mut warn: Option<&mut dyn FnMut(&str)>) -> Vec<String> {
    let mut names: Vec<String> = entries.iter().map(|e| ellipsize_middle(&format!("mcp_{}_{}", sanitize(&e.server_name), sanitize(&e.tool_name)), MCP_TOOL_NAME_MAX_LENGTH)).collect();
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, name) in names.iter().enumerate() { groups.entry(name.replace('-', "_")).or_default().push(i); }
    for group in groups.values().filter(|g| g.len() > 1) {
        if let Some(callback) = warn.as_mut() { callback(&format!("MCP tool name collision after normalization for '{}'; appended deterministic suffixes.", names[group[0]])); }
        for &i in group {
            let digest = format!("{:x}", Sha1::digest(format!("{}\0{}", entries[i].server_name, entries[i].tool_name)));
            let suffix = format!("_{}", &digest[..4]);
            names[i] = format!("{}{}", ellipsize_middle(&names[i], MCP_TOOL_NAME_MAX_LENGTH - suffix.len()), suffix);
        }
    }
    names
}
fn sanitize(name: &str) -> String {
    name.encode_utf16().map(|c| match char::from_u32(u32::from(c)) { Some(ch) if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' => ch, _ => '_' }).collect()
}
fn ellipsize_middle(value: &str, max: usize) -> String {
    if value.len() <= max { return value.into(); }
    if max <= 3 { return value[..max].into(); }
    let remaining = max - 3;
    format!("{}...{}", &value[..remaining.div_ceil(2)], &value[value.len() - remaining / 2..])
}
