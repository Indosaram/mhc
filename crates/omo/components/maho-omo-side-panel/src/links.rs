use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode, percent_decode_str};
const SCHEME: &str = "omo-panel:";
const COMPONENT: &AsciiSet = &CONTROLS.add(b' ').add(b'"').add(b'#').add(b'$').add(b'%').add(b'&').add(b'+').add(b',').add(b'/').add(b':').add(b';').add(b'<').add(b'=').add(b'>').add(b'?').add(b'@').add(b'[').add(b'\\').add(b']').add(b'^').add(b'`').add(b'{').add(b'|').add(b'}');
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelAction { File { path: String }, Agent { id: String }, Goal, Memory }
pub fn panel_action_url(action: &PanelAction) -> String {
    let (kind, value) = match action { PanelAction::File { path } => ("file", path.as_str()), PanelAction::Agent { id } => ("agent", id.as_str()), PanelAction::Goal => ("goal", "current"), PanelAction::Memory => ("memory", "current") };
    format!("{SCHEME}{kind}/{}", utf8_percent_encode(value, COMPONENT))
}
pub fn parse_panel_action_url(url: &str) -> Option<PanelAction> {
    let (kind, encoded) = url.strip_prefix(SCHEME)?.split_once('/')?;
    if encoded.is_empty() { return None; }
    let bytes = encoded.as_bytes();
    for (i, byte) in bytes.iter().enumerate() { if *byte == b'%' && (i + 2 >= bytes.len() || !bytes[i + 1].is_ascii_hexdigit() || !bytes[i + 2].is_ascii_hexdigit()) { return None; } }
    let value = percent_decode_str(encoded).decode_utf8().ok()?.into_owned();
    if value.is_empty() { return None; }
    match kind { "file" => Some(PanelAction::File { path: value }), "agent" => Some(PanelAction::Agent { id: value }), "goal" if value == "current" => Some(PanelAction::Goal), "memory" if value == "current" => Some(PanelAction::Memory), _ => None }
}
pub fn with_action_link(text: &str, action: &PanelAction) -> String { format!("\x1b]8;;{}\x07{text}\x1b]8;;\x07", panel_action_url(action)) }
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn action_roundtrips() {
        for action in [PanelAction::File { path: "packages/a b#c.ts".into() }, PanelAction::Agent { id: "st_01ABC/def".into() }, PanelAction::Goal, PanelAction::Memory, PanelAction::File { path: "a\x07b\x1bc".into() }] {
            let url = panel_action_url(&action); assert!(!url.contains(['\x07', '\x1b'])); assert_eq!(parse_panel_action_url(&url), Some(action));
        }
    }
    #[test] fn foreign_and_malformed_urls_are_not_claimed() { for url in ["https://example.com/x", "omo-panel:ssh/x", "omo-panel:file/", "omo-panel:file", "omo-panel:/x", "omo-panel:file/%zz", "omo-panel:file/%", "omo-panel:file/%FF", "omo-panel:goal/other"] { assert_eq!(parse_panel_action_url(url), None); } }
    #[test] fn links_preserve_visible_text() { assert_eq!(with_action_link("M file.ts", &PanelAction::File { path: "file.ts".into() }), "\x1b]8;;omo-panel:file/file.ts\x07M file.ts\x1b]8;;\x07"); }
}
