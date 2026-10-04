#[cfg(test)]
mod tests {
    use super::super::{base::*,thread::*,turn::*};
    #[test]
    fn facade_constructions_cover_upstream_compile_time_examples() {
        let _initialize=InitializeParams {client_info:ClientInfo {name:"senpi".into(),title:None,version:"0.0.0".into()},capabilities:Some(InitializeCapabilities {experimental_api:true,request_attestation:false,mcp_server_openai_form_elicitation:None,opt_out_notification_methods:None})};
        let _start=ThreadStartParams::default();
        let _resume=ThreadResumeParams {thread_id:"thread-1".into(),overrides:ThreadRuntimeOverrides::default(),history:None,path:None,personality:None,exclude_turns:None,initial_turns_page:None};
        let _read=ThreadReadParams {thread_id:"thread-1".into(),include_turns:Some(true)};
        let _list=ThreadListParams::default();let _loaded=ThreadLoadedListParams::default();
        let _fork=ThreadForkParams {thread_id:"thread-1".into(),overrides:ThreadRuntimeOverrides::default(),path:None,ephemeral:None,thread_source:None,exclude_turns:None};
        let _name=ThreadSetNameParams {thread_id:"thread-1".into(),name:"name".into()};
        let _archive=ThreadArchiveParams {thread_id:"thread-1".into()};let _delete=ThreadDeleteParams {thread_id:"thread-1".into()};let _unsubscribe=ThreadUnsubscribeParams {thread_id:"thread-1".into()};
        let common=TurnCommonParams {thread_id:"thread-1".into(),input:Vec::new(),client_user_message_id:None,responsesapi_client_metadata:None,additional_context:None};
        let _start=TurnStartParams {common:common.clone(),environments:None,cwd:None,runtime_workspace_roots:None,approval_policy:None,approvals_reviewer:None,sandbox_policy:None,permissions:None,model:None,service_tier:None,effort:None,summary:None,personality:None,output_schema:None,collaboration_mode:None,multi_agent_mode:None};
        let _steer=TurnSteerParams {common,expected_turn_id:"turn-1".into()};let _interrupt=TurnInterruptParams {thread_id:"thread-1".into(),turn_id:"turn-1".into()};
    }
}
