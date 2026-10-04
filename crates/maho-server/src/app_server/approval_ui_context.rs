use super::{approval_bridge::ApprovalBridge,approval_types::*,user_input_bridge::UserInputBridge};
use maho_ext_api::*;
use serde_json::json;
use std::sync::{Arc,Mutex};

pub struct AppServerUiContext {
    approvals: Arc<Mutex<ApprovalBridge>>,
    user_input: Arc<Mutex<UserInputBridge>>,
    thread_id: String,
    turn_id: Arc<dyn Fn()->String+Send+Sync>,
    pending_input: Mutex<Option<String>>,
    editor_text: Mutex<String>,
    editor_factory: Mutex<Option<EditorFactory>>,
    tools_expanded: Mutex<bool>,
    themes: Mutex<maho_interactive::theme::registry::ThemeRegistry>,
}
impl AppServerUiContext {
    pub fn new(approvals: Arc<Mutex<ApprovalBridge>>,user_input: Arc<Mutex<UserInputBridge>>,thread_id: String,turn_id: Arc<dyn Fn()->String+Send+Sync>,agent_dir: &std::path::Path) -> Result<Self,ExtensionFailure> {
        let themes = maho_interactive::theme::registry::ThemeRegistry::new(agent_dir.join("themes"),"dark",maho_interactive::theme::ColorMode::Truecolor).map_err(|error|ExtensionFailure::new(error.to_string()))?;
        Ok(Self {approvals,user_input,thread_id,turn_id,pending_input:Mutex::new(None),editor_text:Mutex::new(String::new()),editor_factory:Mutex::new(None),tools_expanded:Mutex::new(false),themes:Mutex::new(themes)})
    }
}
impl ExtensionUi for AppServerUiContext {
    fn actions(&self) -> Option<&dyn ExtensionUiActions> {Some(self)}
    fn select<'a>(&'a self,title: &'a str,options: &'a [String],_: ExtensionUiDialogOptions) -> UiFuture<'a,Option<String>> {
        Box::pin(async move {
            let (first,reason) = title.split_once('\n').unwrap_or((title,""));
            let tool = first.strip_prefix("Permission required: ")?;
            if !PERMISSION_OPTIONS.iter().all(|option|options.iter().any(|value|value == option)) {return None;}
            let tool = maho_ai::utils::js::trim(tool);let reason = maho_ai::utils::js::trim(reason);
            let command = ["Command: $ ","File: ","Path: "].into_iter().find_map(|prefix|reason.lines().find_map(|line|line.strip_prefix(prefix)));
            let kind = if matches!(tool,"edit"|"write"|"apply_patch"|"multiedit") {ApprovalKind::FileChange} else {ApprovalKind::CommandExecution};
            let result = self.approvals.lock().unwrap_or_else(std::sync::PoisonError::into_inner).request_approval(&self.thread_id,kind,&json!({"turnId":"turn-approval","itemId":format!("approval-{tool}"),"toolName":tool,"command":command,"reason":reason}),chrono::Utc::now().timestamp_millis() as u64);
            let outcome = result.await.ok()?;
            Some(match outcome.decision {
                ApprovalDecision::Accept=>"Allow once",
                ApprovalDecision::AcceptForSession=>"Allow always",
                _=>if let Some(reason) = outcome.reason.filter(|reason|reason != NO_SUBSCRIBER_REASON && reason != CANCEL_REASON) {*self.pending_input.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reason);"Deny with feedback"} else {"Deny"},
            }.into())
        })
    }
    fn confirm<'a>(&'a self,title: &'a str,message: &'a str,_: ExtensionUiDialogOptions) -> UiFuture<'a,bool> {
        Box::pin(async move {
            let result = self.approvals.lock().unwrap_or_else(std::sync::PoisonError::into_inner).request_approval(&self.thread_id,ApprovalKind::CommandExecution,&json!({"turnId":"turn-approval","itemId":"approval-confirm","toolName":"confirm","command":title,"reason":message}),chrono::Utc::now().timestamp_millis() as u64);
            result.await.is_ok_and(|outcome|outcome.allow)
        })
    }
    fn input<'a>(&'a self,_: &'a str,_: Option<&'a str>,_: ExtensionUiDialogOptions) -> UiFuture<'a,Option<String>> {Box::pin(async {self.pending_input.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()})}
    fn notify(&self,_: &str,_: NotificationType) {}
    fn set_status(&self,_: &str,_: Option<&str>) {}
    fn set_widget(&self,_: &str,_: Option<WidgetContent>,_: ExtensionWidgetOptions) {}
    fn set_header(&self,_: Option<ComponentFactory>) {}
    fn set_footer(&self,_: Option<ComponentFactory>) {}
    fn set_title(&self,_: &str) {}
    fn paste_to_editor(&self,text: &str) {self.set_editor_text(text);}
    fn set_editor_text(&self,text: &str) {*self.editor_text.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = text.into();}
    fn get_editor_text(&self) -> String {self.editor_text.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()}
    fn custom(&self,_: ComponentFactory,_: CustomUiOptions) -> ExtensionFuture<'_,JsonValue> {Box::pin(async {Err(ExtensionFailure::new("Custom UI is not available in app-server mode."))})}
    fn theme(&self) -> Theme {let themes = self.themes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);Theme {name:Some(themes.current.name.clone()),colors:themes.current.resolved_colors(),..Default::default()}}
}
impl ExtensionUiActions for AppServerUiContext {
    fn question(&self,request: QuestionRequest,options: QuestionOptions) -> ExtensionFuture<'_,QuestionResponse> {
        Box::pin(async move {UserInputBridge::request_user_input(&self.user_input,&self.thread_id,&(self.turn_id)(),&request.request_id.clone(),request,options).await.map_err(|error|ExtensionFailure::new(error.to_string()))})
    }
    fn on_terminal_input(&self,_: TerminalInputHandler) -> UiUnsubscribe {Box::new(||{})}
    fn set_working_message(&self,_: Option<&str>) {}
    fn set_working_visible(&self,_: bool) {}
    fn set_working_indicator(&self,_: Option<WorkingIndicatorOptions>) {}
    fn set_hidden_thinking_label(&self,_: Option<&str>) {}
    fn editor<'a>(&'a self,_: &'a str,prefill: Option<&'a str>) -> ExtensionFuture<'a,Option<String>> {Box::pin(async move {Ok(prefill.map(str::to_owned))})}
    fn add_autocomplete_provider(&self,_: AutocompleteProviderFactory) {}
    fn set_editor_component(&self,factory: Option<EditorFactory>) {*self.editor_factory.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = factory;}
    fn get_editor_component(&self) -> Option<EditorFactory> {self.editor_factory.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()}
    fn get_all_themes(&self) -> Vec<ThemeInfo> {self.themes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_available_themes_with_paths().into_iter().map(|theme|ThemeInfo {name:theme.name,path:theme.path}).collect()}
    fn get_theme(&self,name: &str) -> Option<Theme> {self.themes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).load_theme(name).ok().map(|theme|Theme {name:Some(theme.name.clone()),colors:theme.resolved_colors(),..Default::default()})}
    fn set_theme(&self,_: ThemeSelection) -> SetThemeResult {SetThemeResult {success:false,error:Some("Theme switching is not available in app-server mode.".into())}}
    fn get_tools_expanded(&self) -> bool {*self.tools_expanded.lock().unwrap_or_else(std::sync::PoisonError::into_inner)}
    fn set_tools_expanded(&self,expanded: bool) {*self.tools_expanded.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = expanded;}
}
