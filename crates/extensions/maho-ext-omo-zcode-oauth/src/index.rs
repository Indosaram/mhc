use maho_ext_api::{Extension, ExtensionApi, ExtensionEvent, EventKind, EventResult, ProviderConfig, ExtensionOAuthConfig, ExtensionFuture};
use maho_ai::{models::{RefreshModelsContext, ModelsPublication}, models_store::ModelsStoreEntry, oauth::{OAuthCredentials, OAuthLoginCallbacks}, utils::abort::AbortSignal};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::{Arc, Mutex, OnceLock}, path::Path, io::Write};

#[derive(Default)]
struct Runtime { credential: Option<(String,String)>, ticket: Option<crate::offpeak::Ticket>, task_id: Option<String>, pending: Option<PendingTicket> }
struct PendingTicket { key: String, jwt: String, receiver: tokio::sync::watch::Receiver<Option<Option<String>>> }
async fn acquire_ticket(runtime: Arc<Mutex<Runtime>>, headers: BTreeMap<String,String>, key: String, jwt: String) -> Option<String> {
    acquire_ticket_from(runtime,crate::offpeak::TicketClient::new(headers),key,jwt).await
}
async fn acquire_ticket_from(runtime: Arc<Mutex<Runtime>>, client: crate::offpeak::TicketClient, key: String, jwt: String) -> Option<String> {
    let mut receiver = {
        let mut state = runtime.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(ticket) = &state.ticket && ticket.fresh(crate::oauth::now_ms(),&key,&jwt) { return Some(ticket.id.clone()); }
        if let Some(pending) = &state.pending && pending.key == key && pending.jwt == jwt { pending.receiver.clone() }
        else {
            let task = state.task_id.get_or_insert_with(||format!("omo-offpeak-{}",uuid::Uuid::new_v4())).clone();
            let (sender,receiver) = tokio::sync::watch::channel(None);
            state.pending = Some(PendingTicket { key: key.clone(), jwt: jwt.clone(), receiver: receiver.clone() });
            let runtime = Arc::clone(&runtime);
            tokio::spawn(async move {
                let id = client.ensure(&jwt,&key,&task).await;
                let mut state = runtime.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.credential.as_ref() == Some(&(key.clone(),jwt.clone())) && let Some(id) = &id { state.ticket = Some(crate::offpeak::Ticket::new(&key,&jwt,id.clone(),crate::oauth::now_ms())); }
                if state.pending.as_ref().is_some_and(|pending|pending.key == key && pending.jwt == jwt) { state.pending = None; }
                sender.send_replace(Some(id));
            });
            receiver
        }
    };
    loop {
        if let Some(result) = receiver.borrow_and_update().clone() { return result; }
        if receiver.changed().await.is_err() { return None; }
    }
}
struct Auth { client: crate::oauth::OAuthClient, runtime: Arc<Mutex<Runtime>> }
impl ExtensionOAuthConfig for Auth {
    fn name(&self) -> &str { "GLM ZCode (unofficial)" }
    fn login<'a>(&'a self, callbacks: &'a dyn OAuthLoginCallbacks) -> ExtensionFuture<'a, OAuthCredentials> { Box::pin(self.client.login(callbacks)) }
    fn refresh_token<'a>(&'a self, credential: &'a OAuthCredentials, signal: &'a AbortSignal) -> ExtensionFuture<'a, OAuthCredentials> { Box::pin(self.client.refresh(credential,signal)) }
    fn get_api_key(&self, credential: &OAuthCredentials) -> String {
        let jwt = credential.get_extra_str("zcodeJwtToken").filter(|value| !value.is_empty());
        let mut runtime = self.runtime.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        runtime.credential = jwt.map(|jwt|(credential.access.clone(),jwt.into()));
        if runtime.ticket.as_ref().is_some_and(|ticket| ticket.api_key != credential.access || Some(ticket.jwt.as_str()) != jwt) { runtime.ticket = None; }
        credential.access.clone()
    }
}
fn ascii_env(key: &str) -> Option<String> { std::env::var(key).ok().map(|value| value.trim().to_owned()).filter(|value| !value.is_empty() && value.bytes().all(|byte|(32..=126).contains(&byte))) }
pub fn device_mid(path: &Path) -> Option<String> {
    let read = || std::fs::read_to_string(path).ok().and_then(|text|serde_json::from_str::<Value>(&text).ok()).and_then(|state|state.get("deviceMid").and_then(Value::as_str).map(|value|value.trim().to_owned())).filter(|value| !value.is_empty() && value.bytes().all(|byte|(32..=126).contains(&byte)));
    match std::fs::read_to_string(path) { Ok(_) => return read(), Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}, Err(_) => return None }
    std::fs::create_dir_all(path.parent()?).ok()?;
    let mid = uuid::Uuid::new_v4().to_string();
    match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => { file.write_all(serde_json::to_string_pretty(&json!({"deviceMid":mid})).ok()?.as_bytes()).ok()?; Some(mid) }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => read(),
        Err(_) => None,
    }
}
pub fn source_headers() -> BTreeMap<String,String> {
    let platform = if cfg!(windows) { "win32" } else if cfg!(target_os="macos") { "darwin" } else { "linux" };
    let arch = if cfg!(target_arch="x86_64") { "x64" } else { std::env::consts::ARCH };
    let version = crate::models::app_version();
    let channel = crate::models::printable_ascii(&std::env::var("ZCODE_RELEASE_CHANNEL").ok().filter(|value| !value.is_empty()).unwrap_or_else(||"production".into()));
    let os_version = std::process::Command::new(if cfg!(windows) { "cmd" } else { "uname" }).args(if cfg!(windows) { &["/C","ver"][..] } else { &["-v"][..] }).output().ok().filter(|output|output.status.success()).map(|output|String::from_utf8_lossy(&output.stdout).trim().to_owned()).unwrap_or_default();
    let locale = ["LC_ALL","LC_TIME","LANG"].into_iter().find_map(|key|std::env::var(key).ok().filter(|value|!value.is_empty())).unwrap_or_else(||"en-US".into());
    let language = locale.split(['.','@']).next().unwrap_or("en-US").replace('_',"-");
    let language = if matches!(language.as_str(),"C"|"POSIX") { "en-US".into() } else { language };
    let timezone = iana_time_zone::get_timezone().unwrap_or_else(|_|"UTC".into());
    [("User-Agent",format!("ZCode/{version} ai-sdk/provider-utils/4.0.27 runtime/node.js/22.15.0")),("HTTP-Referer","https://zcode.z.ai".into()),("X-Title","Z Code@electron".into()),("X-ZCode-App-Version",version),("X-Release-Channel",channel),("X-Platform",format!("{platform}-{arch}")),("X-Os-Category",crate::models::os_category(platform).into()),("X-Os-Version",os_version),("X-Client-Language",language),("X-Client-Timezone",timezone),("X-ZCode-Agent","glm".into())].into_iter().map(|(key,value)|(key.into(),crate::models::printable_ascii(&value))).filter(|(_,value)| !value.is_empty()).collect()
}
async fn refresh(context: RefreshModelsContext, headers: BTreeMap<String,String>, runtime: Arc<Mutex<Runtime>>) -> Result<Vec<maho_ext_api::ProviderModelConfig>, maho_ext_api::ExtensionFailure> {
    refresh_from(context,headers,runtime,crate::live_catalog::LIVE_MODELS_URL,crate::models::MODELS_DEV_API_URL).await
}
async fn refresh_from(context: RefreshModelsContext, headers: BTreeMap<String,String>, runtime: Arc<Mutex<Runtime>>, live_url: &str, catalog_url: &str) -> Result<Vec<maho_ext_api::ProviderModelConfig>, maho_ext_api::ExtensionFailure> {
    let restored = crate::models::stored_to_config(context.stored.as_ref());
    if !context.allow_network || context.signal.aborted() || (context.force != Some(true) && context.stored.as_ref().and_then(|entry|entry.checked_at).is_some_and(|checked|crate::oauth::now_ms() - checked.to_string().parse::<f64>().unwrap_or(0.0) < 86_400_000.0)) { return Ok(restored); }
    let key = context.credential.as_ref().and_then(|credential| match credential.get("type").and_then(Value::as_str) { Some("oauth") => credential.get("access").and_then(Value::as_str), Some("api_key") => credential.get("key").and_then(Value::as_str), _ => None }).filter(|key| !key.is_empty());
    let client = reqwest::Client::new();
    let mut models = if let Some(key) = key { crate::live_catalog::fetch_live(&client,live_url,key,&headers,&context.signal).await.unwrap_or_default() } else { Vec::new() };
    if models.is_empty() {
        let fetched = async {
            let response = client.get(catalog_url).timeout(std::time::Duration::from_secs(30)).send().await.ok()?;
            if !response.status().is_success() { return None; }
            response.json::<Value>().await.ok().map(|payload|crate::models::parse_catalog(&payload))
        };
        models = tokio::select! { _ = context.signal.cancelled() => Vec::new(), models = fetched => models.unwrap_or_default() };
    }
    if models.is_empty() { return Ok(restored); }
    let mut active = false;
    if std::env::var("ZCODE_OFFPEAK_ENABLE").ok().as_deref() == Some("1") && crate::offpeak::is_window(crate::oauth::now_ms()) && let Some(credential) = &context.credential && credential.get("type").and_then(Value::as_str) == Some("oauth") && let Some(jwt) = credential.get("zcodeJwtToken").and_then(Value::as_str).filter(|jwt| !jwt.is_empty()) && let Some(key) = key {
        let tickets = crate::offpeak::TicketClient::new(headers.clone());
        active = tickets.availability(jwt,key).await;
        if active {
            let key = key.to_owned(); let jwt = jwt.to_owned();
            tokio::spawn(acquire_ticket(runtime,headers.clone(),key,jwt));
        }
    }
    models = crate::offpeak::route(models,active);
    let checked_at = crate::oauth::now_ms().floor().to_string().parse().ok();
    if context.publish(ModelsPublication { persist: Some(Some(ModelsStoreEntry { models: crate::models::persisted(&models), checked_at, ..Default::default() })), update: None }).await.is_err() { return Ok(restored); }
    Ok(models)
}
pub struct ZcodeOAuth;
fn add_device_metadata(payload: &mut Value, provider: &str, device: Option<&str>) {
    if provider != "glm-zcode" || !payload.is_object() || payload.pointer("/metadata/user_id").is_some() { return; }
    if let Some(device) = device {
        if !payload.get("metadata").is_some_and(Value::is_object) { payload["metadata"] = json!({}); }
        payload["metadata"]["user_id"] = json!(json!({"device_id":device,"account_uuid":"","session_id":crate::signing::session_id()}).to_string());
    }
}
impl Extension for ZcodeOAuth {
    fn register(&self, api: &mut ExtensionApi) {
        static DEVICE: OnceLock<Option<String>> = OnceLock::new();
        let default_device = DEVICE.get_or_init(||std::env::var_os("HOME").and_then(|home|device_mid(&Path::new(&home).join(".zcode/v2/telemetry-state.json")))).clone();
        api.on(EventKind::BeforeProviderRequest,Arc::new(move |event,_| {
            if let ExtensionEvent::BeforeProviderRequest { payload, model: Some(model), .. } = event {
                let device = ascii_env("ZCODE_DEVICE_ID").or_else(||default_device.clone());
                add_device_metadata(payload,&model.provider,device.as_deref());
            }
            Box::pin(async { Ok(EventResult::None) })
        }));
        let signing = Arc::new(crate::signing::Signing::default());
        let hook_signing = Arc::clone(&signing);
        api.on(EventKind::BeforeProviderHeaders,Arc::new(move |event,_| { let signing = Arc::clone(&hook_signing); Box::pin(async move { if let ExtensionEvent::BeforeProviderHeaders { headers } = event { for (key,value) in signing.resolve(headers).await { headers.insert(key,Some(value)); } } Ok(EventResult::None) }) }));
        let runtime = Arc::new(Mutex::new(Runtime::default()));
        let headers = source_headers();
        let mut model = crate::models::model_config("glm-5.3".into(),"GLM-5.3".into()); model.base_url = Some(crate::models::resolve_base_url());
        let auth = Arc::new(maho_ext_host::oauth::ExtensionOAuthAdapter::new(Arc::new(Auth { client: Default::default(), runtime: Arc::clone(&runtime) })));
        let refresh_runtime = Arc::clone(&runtime); let refresh_headers = headers.clone();
        let config = ProviderConfig { name: Some("GLM ZCode (unofficial)".into()), api: Some("anthropic-messages".into()), auth_header: Some(true), headers: Some(headers.clone()), models: Some(vec![model]), oauth: Some(auth), refresh_models: Some(Arc::new(move |context|Box::pin(refresh(context,refresh_headers.clone(),Arc::clone(&refresh_runtime))))),
            stream_simple: Some(Arc::new(move |model,context,options| {
                if model.base_url != crate::offpeak::OFFPEAK_BASE_URL { return maho_ai::providers::anthropic::stream_simple_anthropic(model,context,options); }
                let model = model.clone(); let context = context.clone(); let runtime = Arc::clone(&runtime); let headers = headers.clone(); let signing = Arc::clone(&signing);
                let outer = maho_ai::utils::event_stream::create_assistant_message_event_stream(); let stream = outer.clone();
                tokio::spawn(async move {
                    let mut options = options.unwrap_or_default(); let key = options.stream.request.api_key.clone().unwrap_or_default();
                    let (credential,ticket) = { let state = runtime.lock().unwrap_or_else(std::sync::PoisonError::into_inner); (state.credential.clone().filter(|(cached,_)|cached == &key),state.ticket.clone()) };
                    let enabled = std::env::var("ZCODE_OFFPEAK_ENABLE").ok().as_deref() == Some("1");
                    let ticket_id = if enabled && let Some((_,jwt)) = &credential && crate::offpeak::is_window(crate::oauth::now_ms()) {
                        if let Some(ticket) = ticket.filter(|ticket|ticket.fresh(crate::oauth::now_ms(),&key,jwt)) { Some(ticket.id) }
                        else { tokio::time::timeout(std::time::Duration::from_secs(60),acquire_ticket(runtime,headers,key.clone(),jwt.clone())).await.ok().flatten() }
                    } else { None };
                    let mut model = model; let mut request_headers = options.stream.request.headers.take().unwrap_or_default();
                    request_headers.remove("X-ZCode-Route");
                    if let (Some(id),Some((_,jwt))) = (ticket_id,credential) && crate::offpeak::is_window(crate::oauth::now_ms()) && std::env::var("ZCODE_OFFPEAK_ENABLE").ok().as_deref() == Some("1") {
                        for header in crate::offpeak::SIGNATURE_HEADERS { request_headers.remove(header); }
                        options.stream.request.api_key = Some(jwt.clone());
                        for (key,value) in crate::offpeak::request_headers(&jwt,&key,&id) { request_headers.insert(key,Some(value)); }
                    } else {
                        model.base_url = crate::models::resolve_base_url();
                        request_headers.insert("X-ZCode-Agent".into(),Some("glm".into())); request_headers.insert("Authorization".into(),Some(format!("Bearer {key}")));
                        for (key,value) in signing.resolve(&request_headers).await { request_headers.insert(key,Some(value)); }
                    }
                    options.stream.request.headers = Some(request_headers);
                    let inner = maho_ai::providers::anthropic::stream_simple_anthropic(&model,&context,Some(options));
                    loop { match inner.next().await { Ok(Some(event)) => stream.push(event), Ok(None) => break, Err(error) => { stream.fail(error); return; } } }
                    match inner.result().await { Ok(result) => stream.end(Some(result)), Err(error) => stream.fail(error) }
                });
                outer
            })), ..Default::default() };
        if let Err(error) = api.register_provider("glm-zcode",config) { eprintln!("glm-zcode: registration failed ({})", crate::oauth::redact_secrets(&error.to_string())); }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn offline_and_fresh_snapshots_restore_without_network_or_persistence_changes() {
        use maho_ai::models_store::ModelsStore;
        for allow_network in [false,true] {
            let store = Arc::new(maho_ai::models_store::InMemoryModelsStore::new());
            let stored = ModelsStoreEntry { models: crate::models::persisted(&[crate::models::model_config("stored".into(),"Stored".into())]), checked_at: Some(i64::MAX), ..Default::default() };
            store.write("glm-zcode",&stored,None).await.unwrap();
            let observed = Arc::new(Mutex::new(Vec::new()));
            let models = maho_ai::models::create_models(Some(maho_ai::models::CreateModelsOptions { models_store: Some(store.clone()), auth: Some(Arc::new(KeyAuth)) }));
            models.set_provider(Arc::new(RefreshProvider { urls: ("invalid-live-url".into(),"invalid-catalog-url".into()), observed: observed.clone() }));
            let result = models.refresh(maho_ai::models::ModelsRefreshOptions { allow_network: Some(allow_network), ..Default::default() }).await;
            assert!(result.errors.is_empty()); assert_eq!(observed.lock().unwrap()[0].id,"stored"); assert_eq!(store.read("glm-zcode",None).await.unwrap(),Some(stored));
        }
    }
#[tokio::test]
    async fn concurrent_ticket_waiters_share_one_acquisition_and_clear_pending() {
        use std::io::{BufRead,Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1",0)).unwrap(); let base = format!("http://{}",listener.local_addr().unwrap());
        let runtime = Arc::new(Mutex::new(Runtime { credential: Some(("key".into(),"jwt".into())), ..Default::default() }));
        let (accepted,seen) = tokio::sync::oneshot::channel(); let (release,ready) = std::sync::mpsc::channel();
        let peer = std::thread::spawn(move || { let (mut stream,_) = listener.accept().unwrap(); let mut reader = std::io::BufReader::new(stream.try_clone().unwrap()); let mut line = String::new(); loop { line.clear(); reader.read_line(&mut line).unwrap(); if line == "\r\n" { break; } } accepted.send(()).unwrap(); ready.recv_timeout(std::time::Duration::from_secs(3)).unwrap(); let body = json!({"ticket_id":"shared","state":"ready"}).to_string(); write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap(); });
        let first_runtime = runtime.clone(); let first_base = base.clone();
        let first = tokio::spawn(async move { acquire_ticket_from(first_runtime,crate::offpeak::TicketClient { base: first_base, ..crate::offpeak::TicketClient::new(BTreeMap::new()) },"key".into(),"jwt".into()).await });
        tokio::time::timeout(std::time::Duration::from_secs(3),seen).await.unwrap().unwrap();
        let mut second = Box::pin(acquire_ticket_from(runtime.clone(),crate::offpeak::TicketClient { base, ..crate::offpeak::TicketClient::new(BTreeMap::new()) },"key".into(),"jwt".into()));
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(second.as_mut().poll(cx).is_pending())).await);
        release.send(()).unwrap(); assert_eq!(first.await.unwrap().as_deref(),Some("shared")); assert_eq!(second.await.as_deref(),Some("shared"));
        assert!(runtime.lock().unwrap().pending.is_none()); peer.join().unwrap();
    }
    #[tokio::test]
    async fn failed_ticket_is_retried_with_same_task_identity() {
        use std::io::{BufRead,Read,Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1",0)).unwrap(); let base = format!("http://{}",listener.local_addr().unwrap());
        let peer = std::thread::spawn(move || { let mut tasks = Vec::new(); for status in [429,200] { let (mut stream,_) = listener.accept().unwrap(); stream.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap(); let mut reader = std::io::BufReader::new(stream.try_clone().unwrap()); let mut line = String::new(); let mut len = 0; reader.read_line(&mut line).unwrap(); loop { line.clear(); reader.read_line(&mut line).unwrap(); if line == "\r\n" { break; } else if let Some(n) = line.to_lowercase().strip_prefix("content-length:") { len = n.trim().parse().unwrap(); } } let mut body = vec![0;len]; reader.read_exact(&mut body).unwrap(); tasks.push(serde_json::from_slice::<Value>(&body).unwrap()["task_id"].clone()); let body = json!({"ticket_id":"retried","state":"ready"}).to_string(); write!(stream,"HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap(); } assert_eq!(tasks[0],tasks[1]); assert!(tasks[0].as_str().unwrap().starts_with("omo-offpeak-")); });
        let runtime = Arc::new(Mutex::new(Runtime::default()));
        for expected in [None,Some("retried")] { let result = acquire_ticket_from(runtime.clone(),crate::offpeak::TicketClient { base: base.clone(), ..crate::offpeak::TicketClient::new(BTreeMap::new()) },"key".into(),"jwt".into()).await; assert_eq!(result.as_deref(),expected); assert!(runtime.lock().unwrap().pending.is_none()); }
        peer.join().unwrap();
    }
struct RefreshProvider { urls: (String,String), observed: Arc<Mutex<Vec<maho_ext_api::ProviderModelConfig>>> }
    impl maho_ai::models::Provider for RefreshProvider {
        fn id(&self) -> &str { "glm-zcode" }
        fn name(&self) -> &str { "fixture" }
        fn get_models(&self) -> Vec<maho_ai::types::Model> { crate::models::persisted(&self.observed.lock().expect("observed models")) }
        fn supports_refresh(&self) -> bool { true }
        fn refresh_models<'a>(&'a self, context: RefreshModelsContext) -> maho_ai::types::BoxFuture<'a,Result<(),maho_ai::models::ModelsError>> {
            Box::pin(async move { let models = refresh_from(context,BTreeMap::new(),Arc::new(Mutex::new(Runtime::default())),&self.urls.0,&self.urls.1).await.map_err(|error|maho_ai::models::ModelsError::new(maho_ai::models::ModelsErrorCode::ModelSource,error.to_string()))?; *self.observed.lock().expect("capture models") = models; Ok(()) })
        }
        fn stream(&self, _: &maho_ai::types::Model, _: &maho_ai::types::Context, _: Option<maho_ai::types::StreamOptions>) -> maho_ai::types::AssistantMessageEventStream { panic!("refresh fixture does not stream") }
        fn stream_simple(&self, _: &maho_ai::types::Model, _: &maho_ai::types::Context, _: Option<maho_ai::types::SimpleStreamOptions>) -> maho_ai::types::AssistantMessageEventStream { panic!("refresh fixture does not stream") }
    }
    struct KeyAuth;
    impl maho_ai::models::ModelsAuth for KeyAuth {
        fn resolve<'a>(&'a self, _: &'a dyn maho_ai::models::Provider, _: &'a maho_ai::models::AuthResolutionOverrides) -> maho_ai::types::BoxFuture<'a,Result<Option<maho_ai::models::AuthResolution>,maho_ai::models::ModelsError>> { Box::pin(async { Ok(Some(Default::default())) }) }
        fn refresh_credential<'a>(&'a self, _: &'a dyn maho_ai::models::Provider, _: Option<&'a Value>, _: &'a AbortSignal) -> maho_ai::types::BoxFuture<'a,Result<Option<Value>,maho_ai::models::ModelsError>> { Box::pin(async { Ok(Some(json!({"type":"api_key","key":"fixture-key"}))) }) }
    }
    async fn refresh_fixture(live: Value, fallback: bool) -> (Vec<maho_ext_api::ProviderModelConfig>, Option<ModelsStoreEntry>) {
        use maho_ai::models_store::ModelsStore;
        use std::io::{BufRead,Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1",0)).unwrap(); let base = format!("http://{}",listener.local_addr().unwrap());
        let peer = std::thread::spawn(move || {
            let replies = if fallback { vec![("/live",live),("/catalog",json!({"zai-coding-plan":{"models":{"fallback":{"limit":{"context":123,"output":456}}}}}))] } else { vec![("/live",live)] };
            for (path,payload) in replies {
                let (mut stream,_) = listener.accept().unwrap(); stream.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap()); let mut line = String::new(); reader.read_line(&mut line).unwrap(); assert!(line.starts_with(&format!("GET {path} ")));
                let mut headers = String::new(); loop { line.clear(); reader.read_line(&mut line).unwrap(); if line == "\r\n" { break; } headers.push_str(&line.to_lowercase()); }
                if path == "/live" { assert!(headers.contains("authorization: bearer fixture-key")); }
                let body = payload.to_string(); write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let observed = Arc::new(Mutex::new(Vec::new())); let store = Arc::new(maho_ai::models_store::InMemoryModelsStore::new());
        let models = maho_ai::models::create_models(Some(maho_ai::models::CreateModelsOptions { models_store: Some(store.clone()), auth: Some(Arc::new(KeyAuth)) }));
        models.set_provider(Arc::new(RefreshProvider { urls: (format!("{base}/live"),format!("{base}/catalog")), observed: observed.clone() }));
        let result = models.refresh(maho_ai::models::ModelsRefreshOptions { allow_network: Some(true), ..Default::default() }).await; assert!(result.errors.is_empty(),"{:?}",result.errors); peer.join().unwrap();
        let output = observed.lock().unwrap().clone(); (output,store.read("glm-zcode",None).await.unwrap())
    }
    #[tokio::test] async fn hybrid_refresh_prefers_live_and_publishes_snapshot() { let (models,store) = refresh_fixture(json!({"data":[{"id":"live","display_name":"Live"}]}),false).await; assert_eq!(models[0].id,"live"); assert_eq!(store.unwrap().models[0].id,"live"); }
    #[tokio::test] async fn hybrid_refresh_empty_live_falls_back_to_models_dev() { let (models,store) = refresh_fixture(json!({"data":[]}),true).await; assert_eq!(models[0].id,"fallback"); assert_eq!(models[0].context_window,123); assert_eq!(store.unwrap().models[0].max_tokens,456); }
    #[test] fn request_metadata_preserves_existing_fields_and_uses_process_session() { let mut payload = json!({"metadata":{"custom":1}}); add_device_metadata(&mut payload,"glm-zcode",Some("fixture-device")); let metadata: Value = serde_json::from_str(payload["metadata"]["user_id"].as_str().unwrap()).unwrap(); assert_eq!(metadata,json!({"device_id":"fixture-device","account_uuid":"","session_id":crate::signing::session_id()})); assert_eq!(payload["metadata"]["custom"],1); }
    #[test] fn request_metadata_leaves_foreign_and_existing_identity_untouched() { for (provider,payload) in [("foreign",json!({})),("glm-zcode",json!({"metadata":{"user_id":null}})),("glm-zcode",json!({"metadata":{"user_id":"existing"}}))] { let mut actual = payload.clone(); add_device_metadata(&mut actual,provider,Some("fixture")); assert_eq!(actual,payload); } }
    #[test] fn source_header_names_match_wire_contract() { let headers = source_headers(); let expected = ["HTTP-Referer","User-Agent","X-Client-Language","X-Client-Timezone","X-Os-Category","X-Os-Version","X-Platform","X-Release-Channel","X-Title","X-ZCode-Agent","X-ZCode-App-Version"]; for key in expected { assert!(headers.contains_key(key),"{key}"); } assert_eq!(headers.len(),expected.len()); assert!(!headers.contains_key("X-ZCode-Version")); }
    #[test] fn source_user_agent_matches_version_header() { let headers = source_headers(); assert!(headers["User-Agent"].starts_with(&format!("ZCode/{} ai-sdk/provider-utils/4.0.27 runtime/node.js/",headers["X-ZCode-App-Version"]))); }
    #[test] fn device_mid_is_created_once() { let fixture = tempfile::tempdir().unwrap(); let path = fixture.path().join("state.json"); let first = device_mid(&path).unwrap(); assert_eq!(device_mid(&path).unwrap(),first); }
    #[test] fn malformed_existing_device_state_is_not_overwritten() { let fixture = tempfile::tempdir().unwrap(); let path = fixture.path().join("state.json"); std::fs::write(&path,"malformed").unwrap(); assert!(device_mid(&path).is_none()); assert_eq!(std::fs::read_to_string(path).unwrap(),"malformed"); }
}
