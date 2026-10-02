use maho_ext_api::*;
use std::{collections::VecDeque,sync::Mutex};
struct DialogUi{answers:Mutex<VecDeque<Option<String>>>}
impl DialogUi{fn next(&self)->Option<String>{self.answers.lock().expect("answers").pop_front().expect("scripted answer")}}
impl ExtensionUi for DialogUi{
    fn select<'a>(&'a self,_:&'a str,_:&'a [String],_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>>{Box::pin(async move{self.next()})}
    fn input<'a>(&'a self,_:&'a str,_:Option<&'a str>,_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>>{Box::pin(async move{self.next()})}
    fn confirm<'a>(&'a self,_:&'a str,_:&'a str,_:ExtensionUiDialogOptions)->UiFuture<'a,bool>{Box::pin(async{false})}
    fn notify(&self,_:&str,_:NotificationType){}
    fn set_status(&self,_:&str,_:Option<&str>){}
    fn set_widget(&self,_:&str,_:Option<WidgetContent>,_:ExtensionWidgetOptions){}
    fn set_header(&self,_:Option<ComponentFactory>){}
    fn set_footer(&self,_:Option<ComponentFactory>){}
    fn set_title(&self,_:&str){}
    fn paste_to_editor(&self,_:&str){}
    fn set_editor_text(&self,_:&str){}
    fn get_editor_text(&self)->String{String::new()}
    fn custom(&self,_:ComponentFactory,_:CustomUiOptions)->ExtensionFuture<'_,JsonValue>{Box::pin(async{Err(ExtensionFailure::new("not used"))})}
    fn theme(&self)->Theme{Theme::default()}
}
#[tokio::test]
async fn question_fallback_uses_native_select_input_and_unanswered_status(){
    let ui=DialogUi{answers:Mutex::new(VecDeque::from([Some("Other (type an answer)".into()),Some("typed answer".into()),Some("comment".into())]))};
    let request=QuestionRequest{request_id:"id".into(),wait_for_answer:true,timeout_ms:100,questions:vec![Question{id:"q".into(),header:"h".into(),question:"choose".into(),options:vec![QuestionOption{label:"yes".into(),description:None}],multi_select:false}]};
    let result=maho_rpc::connection_question_bridge::degrade_question(&ui,request,ExtensionUiDialogOptions::default()).await.expect("question");
    assert_eq!(result.status,QuestionStatus::CommentSubmitted);assert_eq!(result.answers["q"].text.as_deref(),Some("typed answer"));assert!(result.unanswered.is_empty());
}
#[tokio::test]
async fn login_maps_labels_to_ids_and_missing_prompt_to_cancellation(){
    use maho_ai::{oauth::{OAuthPrompt,OAuthSelectPrompt,OAuthSelectOption},utils::abort::AbortController};
    let ui=DialogUi{answers:Mutex::new(VecDeque::from([Some("Account".into()),None]))};let signal=AbortController::new().signal();
    let value=maho_rpc::login_prompts::on_select(&ui,&signal,OAuthSelectPrompt{message:"select".into(),options:vec![OAuthSelectOption{id:"account-id".into(),label:"Account".into(),description:None}],signal:None}).await.expect("selection");assert_eq!(value.as_deref(),Some("account-id"));
    assert_eq!(maho_rpc::login_prompts::on_prompt(&ui,&signal,OAuthPrompt::default()).await.expect_err("cancelled").message,"Login cancelled");
}
