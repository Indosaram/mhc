use maho_ext_api::{CustomMessage,ExtensionApi,ToolContent,LoadedExtension,SourceInfo,ExtensionSessionProfile,EventBus,ExtensionRuntime};
use maho_omo_task::renderers::{register_task_message_renderers,CATEGORY_UNAVAILABLE_MESSAGE_TYPE,TASK_COMPLETION_MESSAGE_TYPE,TEAM_MEMBER_LIVENESS_MESSAGE_TYPE};
use serde_json::Value;
#[test]
fn registered_renderers_match_pinned_upstream_goldens() {
    let fixture: Value = serde_json::from_str(include_str!("golden/renderers.json")).expect("generated fixture");
    let mut api = ExtensionApi::new(LoadedExtension::new("task-golden","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default()); register_task_message_renderers(&mut api);
    for case in fixture["cases"].as_array().expect("cases") {
        let renderer = case["renderer"].as_str().expect("renderer");
        let custom_type = match renderer { "renderCategoryUnavailable" => CATEGORY_UNAVAILABLE_MESSAGE_TYPE, "renderTaskCompletion" => TASK_COMPLETION_MESSAGE_TYPE, "renderTeamMemberLiveness" => TEAM_MEMBER_LIVENESS_MESSAGE_TYPE, _ => panic!("unknown renderer") };
        let message = CustomMessage { custom_type:custom_type.into(), content:case["message"].get("content").and_then(Value::as_str).map(|text| vec![ToolContent::text(text)]).unwrap_or_default(), details:case["message"].get("details").cloned(), display:true };
        for output in case["renders"].as_array().expect("renders") {
            let width = output["width"].as_u64().expect("width") as usize;
            let mut component = api.registered.message_renderers.get(custom_type).expect("registered renderer")(&message,&Default::default(),&Default::default()).expect("component");
            let expected: Vec<String> = serde_json::from_value(output["lines"].clone()).expect("lines");
            assert_eq!(component.render(width),expected,"{} width {width}",case["name"]);
        }
    }
}
