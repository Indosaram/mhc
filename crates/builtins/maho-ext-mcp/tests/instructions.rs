use maho_ext_mcp::instructions::*;
#[test]
fn empty_instructions_do_not_change_prompt() {assert!(inject_mcp_instructions("","system").is_none());assert!(build_mcp_instructions_block([]).is_empty());}
#[test]
fn instructions_are_escaped_and_capped_before_xml_expansion() {
    let text=format!("{}<", "a".repeat(3999));let block=build_mcp_instructions_block([("server\"",text.as_str())]);assert!(block.contains("server=\"server&quot;\""));assert!(block.contains(&format!("{}&lt;","a".repeat(3999))));
}
#[test]
fn duplicate_injection_is_suppressed() {let block=build_mcp_instructions_block([("fx","instruction")]);let injected=inject_mcp_instructions(&block,"system").unwrap();assert!(inject_mcp_instructions(&block,&injected).is_none());}
