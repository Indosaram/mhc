//! Ordered command inventory and post-baseline change notifications.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcCommandInput {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub source_info: Value,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RpcCommandSource { Extension, Prompt, Skill }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RpcCommandSyntax { Slash, Dollar }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSlashCommand {
    #[serde(flatten)]
    pub command: RpcCommandInput,
    pub source: RpcCommandSource,
    pub syntax: RpcCommandSyntax,
}

pub fn build_rpc_commands(extensions: &[RpcCommandInput], templates: &[RpcCommandInput], skills: &[RpcCommandInput]) -> Vec<RpcSlashCommand> {
    extensions.iter().map(|command| RpcSlashCommand { command: command.clone(), source: RpcCommandSource::Extension, syntax: RpcCommandSyntax::Slash })
        .chain(templates.iter().map(|command| RpcSlashCommand { command: command.clone(), source: RpcCommandSource::Prompt, syntax: RpcCommandSyntax::Slash }))
        .chain(skills.iter().map(|command| RpcSlashCommand { command: RpcCommandInput { name: format!("skill:{}",command.name), ..command.clone() }, source: RpcCommandSource::Skill, syntax: RpcCommandSyntax::Dollar })).collect()
}
pub fn rpc_command_list_digest(commands: &[RpcSlashCommand]) -> Result<String,serde_json::Error> { serde_json::to_string(commands) }
pub fn registered_extension_commands(commands:&[maho_ext_api::ResolvedCommand])->Vec<RpcCommandInput>{
    commands.iter().map(|resolved|{
        let info=&resolved.command.source_info;
        let scope=match info.scope{maho_ext_api::SourceScope::User=>"user",maho_ext_api::SourceScope::Project=>"project",maho_ext_api::SourceScope::Temporary=>"temporary",maho_ext_api::SourceScope::System=>"system"};
        let origin=match info.origin{maho_ext_api::SourceOrigin::Package=>"package",maho_ext_api::SourceOrigin::TopLevel=>"top-level"};
        let mut source_info=serde_json::json!({"path":info.path,"source":info.source,"scope":scope,"origin":origin});
        if let Some(base_dir)=&info.base_dir{source_info["baseDir"]=base_dir.clone().into();}
        RpcCommandInput{name:resolved.invocation_name.clone(),description:resolved.command.description.clone(),source_info}
    }).collect()
}
/// Classifies one `get_commands` group into the RPC input shape, mirroring the source metadata
/// `registered_extension_commands` emits so every group's `sourceInfo` serializes identically.
pub fn command_inputs(commands: &[maho_ext_api::SlashCommandInfo]) -> Vec<RpcCommandInput> {
    commands.iter().map(|command| {
        let source_info = command.source_info.as_ref().map(|info| {
            let scope = match info.scope { maho_ext_api::SourceScope::User => "user", maho_ext_api::SourceScope::Project => "project", maho_ext_api::SourceScope::Temporary => "temporary", maho_ext_api::SourceScope::System => "system" };
            let origin = match info.origin { maho_ext_api::SourceOrigin::Package => "package", maho_ext_api::SourceOrigin::TopLevel => "top-level" };
            let mut value = serde_json::json!({ "path": info.path, "source": info.source, "scope": scope, "origin": origin });
            if let Some(base_dir) = &info.base_dir { value["baseDir"] = base_dir.clone().into(); }
            value
        }).unwrap_or_else(|| serde_json::json!({}));
        RpcCommandInput { name: command.name.clone(), description: command.description.clone(), source_info }
    }).collect()
}

pub fn create_commands_changed_event(previous_digest: Option<&str>, commands: &[RpcSlashCommand]) -> Result<Option<Value>,serde_json::Error> {
    let Some(previous) = previous_digest else { return Ok(None); };
    if previous == rpc_command_list_digest(commands)? { return Ok(None); }
    Ok(Some(serde_json::json!({"type":"commands_changed","commands":commands})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn input(name: &str) -> RpcCommandInput { RpcCommandInput { name: name.into(),description:None,source_info:json!({"source":"test"}) } }
    #[test]fn merged_command_registration_uses_resolved_invocation_and_source_metadata(){
        let commands=vec![maho_ext_api::ResolvedCommand{invocation_name:"build:2".into(),command:maho_ext_api::RegisteredCommand{name:"build".into(),source_info:maho_ext_api::SourceInfo{path:"extension.rs".into(),source:"builtin".into(),scope:maho_ext_api::SourceScope::System,origin:maho_ext_api::SourceOrigin::Package,base_dir:None},description:Some("Build".into()),argument_hint:None,handler:std::sync::Arc::new(|_,_|Box::pin(async{Ok(())}))}}];
        let inputs=registered_extension_commands(&commands);assert_eq!(inputs[0].name,"build:2");assert_eq!(inputs[0].source_info["scope"],"system");assert!(inputs[0].source_info.get("baseDir").is_none());
    }
    #[test] fn command_surface_preserves_order_and_skill_syntax() {
        let commands = build_rpc_commands(&[input("hooks")], &[input("review")], &[input("debugging")]);
        assert_eq!(commands.iter().map(|c| (&*c.command.name,c.source,c.syntax)).collect::<Vec<_>>(),vec![("hooks",RpcCommandSource::Extension,RpcCommandSyntax::Slash),("review",RpcCommandSource::Prompt,RpcCommandSyntax::Slash),("skill:debugging",RpcCommandSource::Skill,RpcCommandSyntax::Dollar)]);
    }
    #[test] fn baseline_does_not_emit() { assert_eq!(create_commands_changed_event(None,&[build_rpc_commands(&[input("hooks")],&[],&[]).remove(0)]).unwrap(),None); }
    #[test] fn unchanged_surface_does_not_emit() { let commands = build_rpc_commands(&[input("hooks")],&[],&[]); assert_eq!(create_commands_changed_event(Some(&rpc_command_list_digest(&commands).unwrap()),&commands).unwrap(),None); }
    #[test] fn changed_surface_emits_new_snapshot() { let commands = build_rpc_commands(&[input("reload")],&[],&[]); assert_eq!(create_commands_changed_event(Some("[]"),&commands).unwrap(),Some(json!({"type":"commands_changed","commands":commands}))); }
}
