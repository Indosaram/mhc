pub fn command_invocation(event:&maho_ext_api::AgentSessionEvent)->Option<&serde_json::Value>{
    match event{maho_ext_api::AgentSessionEvent::CommandInvocation{command}=>Some(command),_=>None}
}
#[cfg(test)]mod tests{use super::*;#[test]fn extracts_only_canonical_invocation_payload(){let command=serde_json::json!({"name":"build","source":"extension"});let event=maho_ext_api::AgentSessionEvent::CommandInvocation{command:command.clone()};assert_eq!(command_invocation(&event),Some(&command));assert!(command_invocation(&maho_ext_api::AgentSessionEvent::AgentIdle).is_none());}}
