use maho_ext_api::*;
use maho_ext_mcp::elicitation::*;
use serde_json::json;
use std::time::Duration;
struct ScriptedUi {answer:Option<&'static str>,hanging:bool}
impl ExtensionUi for ScriptedUi {
    fn select<'a>(&'a self,_:&'a str,_:&'a [String],_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>> {Box::pin(async {Some("beta".into())})}
    fn confirm<'a>(&'a self,_:&'a str,_:&'a str,_:ExtensionUiDialogOptions)->UiFuture<'a,bool> {Box::pin(async {true})}
    fn input<'a>(&'a self,title:&'a str,_:Option<&'a str>,_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>> {Box::pin(async move {if self.hanging {return std::future::pending().await;}self.answer.map(|answer|if title.contains("count") {answer.into()}else{"Ada".into()})})}
    fn notify(&self,_:&str,_:NotificationType){}
    fn set_status(&self,_:&str,_:Option<&str>){}
    fn set_widget(&self,_:&str,_:Option<WidgetContent>,_:ExtensionWidgetOptions){}
    fn set_header(&self,_:Option<ComponentFactory>){}
    fn set_footer(&self,_:Option<ComponentFactory>){}
    fn set_title(&self,_:&str){}
    fn paste_to_editor(&self,_:&str){}
    fn set_editor_text(&self,_:&str){}
    fn get_editor_text(&self)->String {String::new()}
    fn custom(&self,_:ComponentFactory,_:CustomUiOptions)->ExtensionFuture<'_,JsonValue> {Box::pin(async {Err("custom UI must not be used".into())})}
    fn theme(&self)->Theme {Theme::default()}
}
fn schema()->JsonValue {json!({"properties":{"confirmed":{"type":"boolean"},"count":{"type":"integer"},"mode":{"enum":["alpha","beta"],"type":"string"},"name":{"type":"string"}},"required":["name"]})}
#[test]
fn capability_is_an_empty_object(){assert_eq!(mcp_client_elicitation_capability(),json!({"elicitation":{}}));}
#[tokio::test]
async fn form_accepts_typed_answers() {
    let ui=ScriptedUi {answer:Some("42"),hanging:false};
    let result=run_elicitation_form(&ui,"Ask",&schema(),Duration::from_secs(5)).await;
    assert_eq!(result.action,ElicitationAction::Accept);
    let content=result.content.expect("accepted content");
    assert_eq!(content["count"].as_f64(),Some(42.0));assert_eq!(content["confirmed"],true);assert_eq!(content["mode"],"beta");assert_eq!(content["name"],"Ada");
}
#[tokio::test]
async fn missing_required_answer_and_invalid_number_decline() {
    for answer in [None,Some("not-a-number")] {
        let ui=ScriptedUi {answer,hanging:false};
        assert_eq!(run_elicitation_form(&ui,"Ask",&schema(),Duration::from_secs(5)).await.action,ElicitationAction::Decline);
    }
}
#[tokio::test(start_paused=true)]
async fn hanging_form_is_cancelled_by_the_deadline() {
    let ui=ScriptedUi {answer:Some("42"),hanging:true};
    assert_eq!(run_elicitation_form(&ui,"Ask",&schema(),Duration::from_millis(50)).await.action,ElicitationAction::Cancel);
}
#[tokio::test]
async fn headless_and_url_mode_requests_decline() {
    let ui=ScriptedUi {answer:Some("42"),hanging:false};
    assert_eq!(handle_mcp_elicitation(None,&json!({"requestedSchema":schema()}),Duration::from_secs(5)).await.action,ElicitationAction::Decline);
    assert_eq!(handle_mcp_elicitation(Some(&ui),&json!({"url":"https://example.test"}),Duration::from_secs(5)).await.action,ElicitationAction::Decline);
}
