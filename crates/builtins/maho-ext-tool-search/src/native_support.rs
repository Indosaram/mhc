pub enum AnthropicToolSearchTarget<'a> {
    Api(&'a str),
    Model { api: &'a str, id: &'a str, provider: &'a str, supports_tool_references: Option<bool> },
}
pub fn supports_anthropic_native_tool_search(target: Option<&AnthropicToolSearchTarget<'_>>) -> bool {
    let Some(target)=target else { return false; };
    match target {
        AnthropicToolSearchTarget::Api(api)=>*api=="anthropic-messages",
        AnthropicToolSearchTarget::Model { api,id,provider,supports_tool_references }=> {
            if *api!="anthropic-messages" { return false; }
            if let Some(supported)=supports_tool_references { return *supported; }
            if *provider!="anthropic" || id.contains("haiku") { return false; }
            let Some(tail)=id.strip_prefix("claude-") else { return false; };
            let Some(tail)=["opus-","sonnet-","fable-"].iter().find_map(|family|tail.strip_prefix(family)) else { return false; };
            let major_length=tail.bytes().take_while(u8::is_ascii_digit).count(); if major_length==0 { return false; }
            let major=tail[..major_length].parse::<f64>().unwrap_or(f64::INFINITY);
            let suffix=&tail[major_length..]; if !suffix.is_empty() && !suffix.starts_with('-') { return false; }
            let minor= suffix.strip_prefix('-').map(|rest| { let length=rest.bytes().take_while(u8::is_ascii_digit).count(); let suffix=&rest[length..]; if length>0 && length<8 && (suffix.is_empty() || suffix.starts_with('-')) { rest[..length].parse::<f64>().unwrap_or(0.) } else { 0. } }).unwrap_or(0.);
            major>4. || (major==4. && minor>=5.)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn upstream_support(id:&str)->bool {supports_anthropic_native_tool_search(Some(&AnthropicToolSearchTarget::Model{api:"anthropic-messages",id,provider:"anthropic",supports_tool_references:None}))}
    #[test] fn upstream_fable_5_1() {assert!(upstream_support("claude-fable-5-1"));}
    #[test] fn upstream_opus_5() {assert!(upstream_support("claude-opus-5"));}
    #[test] fn upstream_opus_4_8() {assert!(upstream_support("claude-opus-4-8"));}
    #[test] fn upstream_sonnet_4_5_dated() {assert!(upstream_support("claude-sonnet-4-5-20250929"));}
    #[test] fn upstream_opus_4_1() {assert!(!upstream_support("claude-opus-4-1"));}
    #[test] fn upstream_haiku_4_5_dated() {assert!(!upstream_support("claude-haiku-4-5-20251001"));}
    #[test] fn upstream_old_sonnet() {assert!(!upstream_support("claude-3-5-sonnet-20241022"));}
    #[test] fn upstream_explicit_proxy_override() {
        assert!(supports_anthropic_native_tool_search(Some(&AnthropicToolSearchTarget::Model{api:"anthropic-messages",id:"claude-opus-4-1",provider:"openmodel",supports_tool_references:Some(true)})));
    }
    #[test] fn upstream_api_only_contract() {
        assert!(supports_anthropic_native_tool_search(Some(&AnthropicToolSearchTarget::Api("anthropic-messages"))));
        assert!(!supports_anthropic_native_tool_search(Some(&AnthropicToolSearchTarget::Api("openai-responses"))));
        assert!(!supports_anthropic_native_tool_search(None));
    }
    #[test] fn api_gate() { assert!(supports_anthropic_native_tool_search(Some(&AnthropicToolSearchTarget::Api("anthropic-messages")))); assert!(!supports_anthropic_native_tool_search(None)); }
    #[test] fn model_gate() { for (id,expected) in [("claude-opus-4-5-20251101",true),("claude-opus-4-20251101",false),("claude-opus-4-1",false),("claude-sonnet-5",true),("claude-haiku-5",false),("claude-fable-5",true)] { assert_eq!(supports_anthropic_native_tool_search(Some(&AnthropicToolSearchTarget::Model { api:"anthropic-messages", id, provider:"anthropic", supports_tool_references:None })),expected); } }
    #[test] fn compatibility_override() { assert!(supports_anthropic_native_tool_search(Some(&AnthropicToolSearchTarget::Model { api:"anthropic-messages",id:"proxy",provider:"gateway",supports_tool_references:Some(true) }))); }
    #[test] fn malformed_minor_does_not_enable_four_series() { assert!(!supports_anthropic_native_tool_search(Some(&AnthropicToolSearchTarget::Model{api:"anthropic-messages",id:"claude-opus-4-5x",provider:"anthropic",supports_tool_references:None}))); }
}
