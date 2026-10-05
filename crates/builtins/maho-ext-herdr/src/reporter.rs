use crate::{client::{HerdrClient,HerdrMethod},state::{HerdrState,HerdrStateEvent,reduce_herdr_state,select_herdr_report,is_herdr_blocked_event}};
use maho_ext_api::{Extension,ExtensionApi,ExtensionContext,ExtensionEvent,EventKind,EventResult,ExtensionMode,SessionReason,SessionManager,BusSubscription};
use serde_json::{Value,json};
use std::{sync::{Arc,Mutex},path::Path,io::Read};
use tokio::task::JoinHandle;

pub type HerdrEnv = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;
pub type HerdrClock = Arc<dyn Fn() -> u64 + Send + Sync>;
pub type HerdrDebug = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone)]
pub struct HerdrDependencies{
    pub env:HerdrEnv,
    pub now:HerdrClock,
    pub debug:HerdrDebug,
}
impl Default for HerdrDependencies{
    fn default()->Self{
        Self{
            env:Arc::new(|name|std::env::var(name).ok()),
            now:Arc::new(||std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0,|elapsed|elapsed.as_millis().try_into().unwrap_or(u64::MAX))),
            debug:Arc::new(|_message|{}),
        }
    }
}

pub fn has_user_reporter(paths:&[String])->bool{
    paths.iter().any(|path|{
        let name=path.rsplit(['/', '\\']).next().unwrap_or(path);
        if !name.starts_with("herdr-")||![".ts",".js",".mjs"].iter().any(|ext|name.ends_with(ext)){return false;}
        let mut header=[0u8;400];
        std::fs::File::open(path).and_then(|mut file|file.read(&mut header)).map_or(true,|length|!header[..length].windows(b"HERDR_INTEGRATION_ID=".len()).any(|window|window==b"HERDR_INTEGRATION_ID="))
    })
}
pub fn count_running_child_tasks(cwd:&Path,session:&str)->u64{
    let Ok(entries)=std::fs::read_dir(cwd.join(".omo/senpi-task/tasks"))else{return 0};
    entries.filter_map(Result::ok).filter(|entry|entry.path().extension().is_some_and(|ext|ext=="json")).filter(|entry|{
        let Ok(bytes)=std::fs::read(entry.path())else{return false};let Ok(record)=serde_json::from_slice::<Value>(&bytes)else{return false};
        matches!(record.get("status").and_then(Value::as_str),Some("running"|"pending"))&&["root_session_id","parent_session_id"].iter().any(|key|record.get(key).and_then(Value::as_str)==Some(session))
    }).count().try_into().expect("task count fits u64")
}
struct ReporterState{
    deps:HerdrDependencies,
    client:Option<Arc<HerdrClient>>,bound:Option<Arc<dyn SessionManager>>,state:HerdrState,stopped:bool,deferred:bool,
    last_report:Option<String>,title:Option<String>,subscriptions:Vec<BusSubscription>,poll:Option<JoinHandle<()>>,pending:Vec<JoinHandle<()>>,
}
impl ReporterState{
    fn owns(&self,ctx:&ExtensionContext)->bool{!self.stopped&&self.bound.as_ref().is_some_and(|bound|Arc::ptr_eq(bound,&ctx.session_manager))}
    fn session_ref(&self)->serde_json::Map<String,Value>{let Some(bound)=&self.bound else{return Default::default()};match bound.session_file(){Some(path)=>json!({"agent_session_path":path.to_string_lossy()}).as_object().expect("object").clone(),None=>json!({"agent_session_id":bound.session_id()}).as_object().expect("object").clone()}}
}
fn enqueue(state:&mut ReporterState,method:HerdrMethod,params:serde_json::Map<String,Value>)->Option<JoinHandle<std::io::Result<()>>>{
    let work=state.client.as_ref()?.send(method,params);
    let debug=state.deps.debug.clone();
    Some(tokio::spawn(async move{
        let result=work.await;
        if let Err(error)=&result{debug(&error.to_string());}
        result
    }))
}
fn publish_begin(state:&mut ReporterState)->Option<(JoinHandle<std::io::Result<()>>,String)>{
    if state.stopped||state.bound.is_none(){return None;}
    let report=select_herdr_report(&state.state);
    let key=serde_json::to_string(&report).expect("report serializes");
    if state.last_report.as_ref()==Some(&key){return None;}
    state.last_report=Some(key.clone());
    let mut params=state.session_ref();
    params.insert("agent".into(),"pi".into());
    params.insert("state".into(),report.state.into());
    if let Some(message)=report.message{params.insert("message".into(),message.into());}
    enqueue(state,HerdrMethod::ReportAgent,params).map(|work|(work,key))
}
async fn publish_finish(state:Arc<Mutex<ReporterState>>,work:JoinHandle<std::io::Result<()>>,key:String){
    if !work.await.is_ok_and(|result|result.is_ok()){
        let mut guard=state.lock().expect("reporter lock");
        if guard.last_report.as_ref()==Some(&key){guard.last_report=None;}
    }
}
async fn publish(state:Arc<Mutex<ReporterState>>){
    let work={let mut guard=state.lock().expect("reporter lock");publish_begin(&mut guard)};
    if let Some((work,key))=work{publish_finish(state,work,key).await;}
}
async fn report_title(state:Arc<Mutex<ReporterState>>,ctx:&ExtensionContext){
    let title=ctx.session_manager.get_session_name().unwrap_or_default();
    let work={let mut guard=state.lock().expect("reporter lock");if guard.title.as_ref()==Some(&title){return;}guard.title=Some(title.clone());enqueue(&mut guard,HerdrMethod::ReportMetadata,json!({"title":title,"display_agent":title}).as_object().expect("object").clone())};
    if let Some(work)=work && !work.await.is_ok_and(|result|result.is_ok()){let mut guard=state.lock().expect("reporter lock");if guard.title.as_ref()==Some(&title){guard.title=None;}}
}
pub struct Herdr;
impl Extension for Herdr{ fn register(&self,api:&mut ExtensionApi){ HerdrExtension::default().register(api); } }

#[derive(Clone,Default)]
pub struct HerdrExtension{ pub deps:HerdrDependencies }
impl Extension for HerdrExtension{
    fn register(&self,api:&mut ExtensionApi){
        let deps=self.deps.clone();
        let state=Arc::new(Mutex::new(ReporterState{deps:deps.clone(),client:None,bound:None,state:HerdrState::default(),stopped:false,deferred:false,last_report:None,title:None,subscriptions:Vec::new(),poll:None,pending:Vec::new()}));
        let start=state.clone();let bus=api.events.clone();let start_deps=deps.clone();
        api.on(EventKind::SessionStart,Arc::new(move|event,ctx|{
            let state=start.clone();let bus=bus.clone();let deps=start_deps.clone();Box::pin(async move{
                let ExtensionEvent::SessionStart(event)=event else{return Ok(EventResult::None)};
                {
                    let mut guard=state.lock().expect("reporter lock");if guard.stopped||guard.deferred||guard.bound.is_some()||ctx.mode!=ExtensionMode::Tui{return Ok(EventResult::None);}
                    let (Some(enabled),Some(socket),Some(pane))=((deps.env)("HERDR_ENV"),(deps.env)("HERDR_SOCKET_PATH"),(deps.env)("HERDR_PANE_ID"))else{return Ok(EventResult::None);};
                    if enabled!="1"||socket.is_empty()||pane.is_empty(){return Ok(EventResult::None);}
                    if has_user_reporter(&ctx.loaded_extension_paths){guard.deferred=true;return Ok(EventResult::None);}
                    guard.bound=Some(ctx.session_manager.clone());guard.client=Some(Arc::new(HerdrClient::new(socket,pane,deps.now.clone())));
                    guard.state.turn_active = !ctx.is_idle();guard.state.child_count=count_running_child_tasks(&ctx.cwd,ctx.session_manager.session_id());
                    for channel in ["herdr:blocked","terminal_monitor_state"]{
                        let callback=state.clone();guard.subscriptions.push(bus.on(channel,Arc::new(move|value|{
                            let mut guard=callback.lock().expect("reporter lock");if guard.stopped{return;}
                            let event=if channel=="herdr:blocked"{
                                if !is_herdr_blocked_event(value){return;}
                                HerdrStateEvent::Blocked{active:value["active"].as_bool().expect("validated"),id:value["id"].as_str().expect("validated").into(),label:value.get("label").and_then(Value::as_str).map(str::to_owned)}
                            }else{
                                if !maho_ext_builtin_loose::monitor_state_event::is_terminal_monitor_state_event(value){return;}
                                let count=value["activeCount"].as_u64().or_else(||value["activeCount"].as_f64().and_then(|count|count.to_string().parse().ok())).expect("validated integer count");HerdrStateEvent::Monitors{count}
                            };
                            guard.state=reduce_herdr_state(guard.state.clone(),event);
                            guard.pending.retain(|handle|!handle.is_finished());
                            if let Some((work,key))=publish_begin(&mut guard){let state=callback.clone();guard.pending.push(tokio::spawn(publish_finish(state,work,key)));}
                        })));
                    }
                    let poll_state=state.clone();let poll_ctx=ctx.clone();guard.poll=Some(tokio::spawn(async move{
                        let mut interval=tokio::time::interval_at(tokio::time::Instant::now()+std::time::Duration::from_secs(4),std::time::Duration::from_secs(4));
                        loop{interval.tick().await;{let mut guard=poll_state.lock().expect("reporter lock");if guard.stopped{break;}guard.state.child_count=count_running_child_tasks(&poll_ctx.cwd,poll_ctx.session_manager.session_id());}publish(poll_state.clone()).await;}
                    }));
                }
                report_title(state.clone(),ctx).await;
                let work={let mut guard=state.lock().expect("reporter lock");let mut params=guard.session_ref();params.insert("agent".into(),"pi".into());params.insert("session_start_source".into(),format!("{:?}",event.reason).to_lowercase().into());enqueue(&mut guard,HerdrMethod::ReportAgentSession,params)};
                if let Some(work)=work{let _result=work.await;}
                publish(state).await;Ok(EventResult::None)
            })
        }));
        for kind in [EventKind::SessionInfoChanged,EventKind::AgentStart,EventKind::AgentSettled,EventKind::SessionShutdown]{
            let state=state.clone();api.on(kind,Arc::new(move|event,ctx|{
                let state=state.clone();Box::pin(async move{
                    if !state.lock().expect("reporter lock").owns(ctx){return Ok(EventResult::None);}
                    if kind==EventKind::SessionInfoChanged{report_title(state,ctx).await;return Ok(EventResult::None);}
                    if let ExtensionEvent::SessionShutdown(event)=event{
                        let (pending,poll)={let mut guard=state.lock().expect("reporter lock");if guard.stopped{return Ok(EventResult::None);}guard.stopped=true;let poll=guard.poll.take();if let Some(poll)=&poll{poll.abort();}guard.subscriptions.clear();(std::mem::take(&mut guard.pending),poll)};
                        if let Some(poll)=poll{let _result=poll.await;}
                        for work in pending{let _result=work.await;}
                        let drain=state.lock().expect("reporter lock").client.as_ref().expect("bound client").drain();drain.await;
                        if event.reason==SessionReason::Quit{let work=enqueue(&mut state.lock().expect("reporter lock"),HerdrMethod::ReleaseAgent,json!({"agent":"pi"}).as_object().expect("object").clone());if let Some(work)=work{let _result=work.await;}}
                    }else{
                        {let mut guard=state.lock().expect("reporter lock");guard.state.turn_active=kind==EventKind::AgentStart||!ctx.is_idle();if kind==EventKind::AgentSettled{guard.state.child_count=count_running_child_tasks(&ctx.cwd,ctx.session_manager.session_id());}}
                        publish(state).await;
                    }
                    Ok(EventResult::None)
                })
            }));
        }
    }
}
