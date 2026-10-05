use maho_ai::types::{TextContent,TextAudience};
use serde_json::{Map,Value};
use crate::manager::TerminalManager;
pub struct TerminalToolResult {pub content:Vec<TextContent>,pub details:Option<Map<String,Value>>,pub is_error:Option<bool>}
pub fn text_result(text:impl Into<String>)->TerminalToolResult {TerminalToolResult {content:vec![TextContent {text:text.into(),audience:None,text_signature:None}],details:None,is_error:None}}
pub fn error_result(text:impl Into<String>)->TerminalToolResult {let mut result=text_result(text);result.is_error=Some(true);result}
pub fn noticed_result(text:&str,notices:&[Option<&str>])->TerminalToolResult {TerminalToolResult {content:crate::output_format::split_model_only_notices(text,notices),details:None,is_error:None}}
pub fn model_only_part(text:impl Into<String>)->TextContent {TextContent {text:text.into(),audience:Some(TextAudience::Model),text_signature:None}}
pub fn resolve_terminal_id(manager:&TerminalManager,id:&str)->String {manager.resolve_id(id).unwrap_or_else(||id.to_owned())}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolve_keeps_runtime_ids_and_falls_back_for_unseen_monitor_ids() {
        let mut manager=TerminalManager::new(1);manager.bind_monitor_id("mon_1","bash_9");
        assert_eq!(resolve_terminal_id(&manager,"mon_1"),"bash_9");
        assert_eq!(resolve_terminal_id(&manager,"bash_9"),"bash_9");
        assert_eq!(resolve_terminal_id(&manager,"mon_missing"),"mon_missing");
    }
}
