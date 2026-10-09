use maho_ai::{compat::extension_oauth_types::{OAuthPrompt,OAuthSelectPrompt},utils::abort::{AbortSignal,ListenerId}};
use maho_ext_api::{ExtensionUi,ExtensionUiDialogOptions,ExtensionFailure};
/// The two OAuth prompt callbacks the RPC connection installs (senpi
/// `createRpcLoginPromptCallbacks`): one login-wide signal releases both dialogs.
pub struct RpcLoginPromptCallbacks{login:AbortSignal,provider:String,sink:std::sync::Arc<dyn Fn(serde_json::Value)+Send+Sync>,ui:Option<std::sync::Arc<dyn ExtensionUi>>}
impl RpcLoginPromptCallbacks{
    pub fn new(login:AbortSignal)->Self{Self{login,provider:String::new(),sink:std::sync::Arc::new(|_|{}),ui:None}}
    pub fn with_sink(login:AbortSignal,provider:String,sink:std::sync::Arc<dyn Fn(serde_json::Value)+Send+Sync>,ui:std::sync::Arc<dyn ExtensionUi>)->Self{Self{login,provider,sink,ui:Some(ui)}}
    pub async fn on_prompt(&self,ui:&dyn ExtensionUi,prompt:OAuthPrompt)->Result<String,ExtensionFailure>{on_prompt(ui,&self.login,prompt).await}
    pub async fn on_select(&self,ui:&dyn ExtensionUi,prompt:OAuthSelectPrompt)->Result<Option<String>,ExtensionFailure>{on_select(ui,&self.login,prompt).await}
}
impl maho_ai::compat::extension_oauth_types::OAuthLoginCallbacks for RpcLoginPromptCallbacks{
    fn on_auth(&self,info:maho_ai::compat::extension_oauth_types::OAuthAuthInfo){(self.sink)(serde_json::json!({"type":"auth_login_url","provider":self.provider,"url":info.url}));}
    fn on_device_code(&self,info:maho_ai::compat::extension_oauth_types::OAuthDeviceCodeInfo){(self.sink)(serde_json::json!({"type":"auth_login_url","provider":self.provider,"url":info.verification_uri}));}
    fn on_prompt(&self,prompt:OAuthPrompt)->maho_ai::types::BoxFuture<'_,String>{let ui=self.ui.clone();let login=self.login.clone();Box::pin(async move{match ui{Some(ui)=>on_prompt(ui.as_ref(),&login,prompt).await.unwrap_or_default(),None=>String::new()}})}
    fn on_select(&self,prompt:OAuthSelectPrompt)->maho_ai::types::BoxFuture<'_,Option<String>>{let ui=self.ui.clone();let login=self.login.clone();Box::pin(async move{match ui{Some(ui)=>on_select(ui.as_ref(),&login,prompt).await.unwrap_or(None),None=>None}})}
    fn signal(&self)->Option<AbortSignal>{Some(self.login.clone())}
}
struct DialogSignal{signal:maho_ext_api::AbortSignal,listeners:Vec<(AbortSignal,ListenerId)>}
impl DialogSignal{
    fn new(login:&AbortSignal,prompt:Option<&AbortSignal>)->Self{
        let signal=maho_ext_api::AbortSignal::default();let mut listeners=Vec::new();
        for source in std::iter::once(login).chain(prompt){let target=signal.clone();let id=source.add_abort_listener(move|_|target.abort());listeners.push((source.clone(),id));if source.aborted(){signal.abort();}}
        Self{signal,listeners}
    }
}
impl Drop for DialogSignal{fn drop(&mut self){for (signal,id) in self.listeners.drain(..){signal.remove_abort_listener(id);}}}
pub async fn on_prompt(ui:&dyn ExtensionUi,login:&AbortSignal,prompt:OAuthPrompt)->Result<String,ExtensionFailure>{
    let dialog=DialogSignal::new(login,prompt.signal.as_ref());
    if dialog.signal.is_aborted(){return Err(ExtensionFailure::new("Login cancelled"));}
    let value=ui.input(&prompt.message,prompt.placeholder.as_deref(),ExtensionUiDialogOptions{signal:Some(dialog.signal.clone()),timeout_ms:None}).await;
    if dialog.signal.is_aborted()||value.is_none(){return Err(ExtensionFailure::new("Login cancelled"));}
    Ok(value.expect("prompt value checked"))
}
pub async fn on_select(ui:&dyn ExtensionUi,login:&AbortSignal,prompt:OAuthSelectPrompt)->Result<Option<String>,ExtensionFailure>{
    let dialog=DialogSignal::new(login,prompt.signal.as_ref());
    if dialog.signal.is_aborted(){return Err(ExtensionFailure::new("Login cancelled"));}
    let labels=prompt.options.iter().map(|option|option.label.clone()).collect::<Vec<_>>();
    let value=ui.select(&prompt.message,&labels,ExtensionUiDialogOptions{signal:Some(dialog.signal.clone()),timeout_ms:None}).await;
    if dialog.signal.is_aborted(){return Err(ExtensionFailure::new("Login cancelled"));}
    Ok(value.and_then(|label|prompt.options.iter().find(|option|option.label==label).map(|option|option.id.clone())))
}
#[cfg(test)]mod tests{
    use super::*;
    use maho_ai::utils::abort::AbortController;
    #[test]fn either_abort_source_releases_dialog(){let login=AbortController::new();let prompt=AbortController::new();let dialog=DialogSignal::new(&login.signal(),Some(&prompt.signal()));prompt.abort(None);assert!(dialog.signal.is_aborted());assert!(!login.signal().aborted());let login=AbortController::new();let dialog=DialogSignal::new(&login.signal(),None);login.abort(None);assert!(dialog.signal.is_aborted());}
    #[test]fn already_aborted_source_is_observed(){let source=AbortController::new();source.abort(None);assert!(DialogSignal::new(&source.signal(),None).signal.is_aborted());}
}
