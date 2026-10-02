use maho_server::app_server::{approval_bridge::ApprovalBridge,approval_ui_context::AppServerUiContext,user_input_bridge::UserInputBridge,approval_types::PERMISSION_OPTIONS};
use maho_ext_api::{ExtensionUi,ExtensionUiActions,ExtensionUiDialogOptions};
use serde_json::json;
use std::sync::{Arc,Mutex};

#[tokio::test]
async fn permission_selection_maps_wire_decision_and_preserves_feedback_for_next_input() {
    let (send,mut receive) = tokio::sync::mpsc::unbounded_channel();
    let approvals = Arc::new(Mutex::new(ApprovalBridge::new(Arc::new(move |_,message| {send.send(message).unwrap();1}))));
    let input = Arc::new(Mutex::new(UserInputBridge::new(Arc::new(|_,_|0))));
    let directory = tempfile::tempdir().unwrap();
    let ui = Arc::new(AppServerUiContext::new(approvals.clone(),input,"thread".into(),Arc::new(||"turn".into()),directory.path()).unwrap());
    let choosing = ui.clone();
    let task = tokio::spawn(async move {choosing.select("Permission required: bash\nCommand: $ ls",&PERMISSION_OPTIONS.iter().map(|value|(*value).into()).collect::<Vec<_>>(),ExtensionUiDialogOptions::default()).await});
    let request = tokio::time::timeout(std::time::Duration::from_secs(3),receive.recv()).await.unwrap().unwrap();
    assert_eq!(request["method"],"item/commandExecution/requestApproval");assert_eq!(request["params"]["command"],"ls");
    approvals.lock().unwrap().resolve_response(&json!({"id":request["id"],"result":{"decision":"decline","reason":"use a narrower command"}}));
    assert_eq!(task.await.unwrap().as_deref(),Some("Deny with feedback"));
    assert_eq!(ui.input("feedback",None,ExtensionUiDialogOptions::default()).await.as_deref(),Some("use a narrower command"));
    assert!(ui.input("feedback",None,ExtensionUiDialogOptions::default()).await.is_none());
    assert!(ui.select("unrelated",&[],ExtensionUiDialogOptions::default()).await.is_none());
}
#[test]
fn app_server_ui_retains_editor_and_tool_expansion_state_without_theme_mutation() {
    let approvals = Arc::new(Mutex::new(ApprovalBridge::new(Arc::new(|_,_|0))));
    let input = Arc::new(Mutex::new(UserInputBridge::new(Arc::new(|_,_|0))));
    let directory = tempfile::tempdir().unwrap();
    let ui = AppServerUiContext::new(approvals,input,"thread".into(),Arc::new(||"turn".into()),directory.path()).unwrap();
    ui.paste_to_editor("hello");assert_eq!(ui.get_editor_text(),"hello");
    ExtensionUiActions::set_tools_expanded(&ui,true);assert!(ExtensionUiActions::get_tools_expanded(&ui));
    assert!(!ExtensionUiActions::set_theme(&ui,maho_ext_api::ThemeSelection::Name("light".into())).success);
    assert!(ExtensionUiActions::get_theme(&ui,"dark").is_some());
}
