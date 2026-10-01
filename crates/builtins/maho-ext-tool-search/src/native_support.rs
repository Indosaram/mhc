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
            let mut segments=tail.split('-');
            let Some(major)=segments.next().and_then(|n|n.parse::<u32>().ok()) else { return false; };
            let minor=segments.next().filter(|n|n.len()<8).and_then(|n|n.parse::<u32>().ok()).unwrap_or(0);
            major>4 || (major==4 && minor>=5)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn api_gate() { assert!(supports_anthropic_native_tool_search(Some(&AnthropicToolSearchTarget::Api("anthropic-messages")))); assert!(!supports_anthropic_native_tool_search(None)); }
    #[test] fn model_gate() { for (id,expected) in [("claude-opus-4-5-20251101",true),("claude-opus-4-20251101",false),("claude-opus-4-1",false),("claude-sonnet-5",true),("claude-haiku-5",false),("claude-fable-5",true)] { assert_eq!(supports_anthropic_native_tool_search(Some(&AnthropicToolSearchTarget::Model { api:"anthropic-messages", id, provider:"anthropic", supports_tool_references:None })),expected); } }
    #[test] fn compatibility_override() { assert!(supports_anthropic_native_tool_search(Some(&AnthropicToolSearchTarget::Model { api:"anthropic-messages",id:"proxy",provider:"gateway",supports_tool_references:Some(true) }))); }
}
