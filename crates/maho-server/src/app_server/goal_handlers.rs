use super::{goal_wire::to_thread_goal,handler_params::{object_value,required_string},registry::{JsonRpcError,MethodRegistration,MethodScope},server_core::ServerCore,thread_registry::{ThreadEntry,ThreadRegistry}};
use maho_ext_goal::{GoalStatus,GoalUpdate,GoalUpdateSource};
use serde_json::{Map,Value,json};
use std::sync::{Arc,Weak};
use tokio::sync::{Mutex,RwLock};

pub async fn register_thread_goal_handlers(core: &Arc<RwLock<ServerCore>>,threads: Arc<ThreadRegistry>) {
    let weak = Arc::downgrade(core);
    let mut core = core.write().await;
    for method in ["thread/goal/set","thread/goal/get","thread/goal/clear"] {
        let threads = threads.clone(); let weak = weak.clone();
        core.registry.register(method.into(),MethodRegistration {requires_init:true,experimental:false,scope:MethodScope::Thread,handler:Arc::new(move |context| {
            let threads = threads.clone(); let weak = weak.clone();
            Box::pin(async move {
                let params = object_value(&context.request["params"]);
                let thread_id = required_string(&value(&params,"threadId"),"threadId")?.to_owned();
                match method {
                    "thread/goal/get" => {
                        let entry = require_thread(&threads,&thread_id).await?;
                        let (session,cwd,tasks) = session_parts(&entry).await;
                        let reference = session.with_session_manager(|manager|maho_ext_goal::goal_store_ref(manager,&cwd));
                        let _task = tasks.lock_owned().await;
                        let goal = maho_ext_goal::read_goal(&reference).map_err(goal_error)?;
                        Ok(json!({"goal":goal.as_ref().map(to_thread_goal)}))
                    },
                    "thread/goal/clear" => {
                        let entry = require_thread(&threads,&thread_id).await?;
                        let (session,cwd,tasks) = session_parts(&entry).await;
                        let reference = session.with_session_manager(|manager|maho_ext_goal::goal_store_ref(manager,&cwd));
                        let cleared = {let _task = tasks.lock_owned().await;maho_ext_goal::clear_goal(&reference).await.map_err(goal_error)?};
                        if cleared {
                            let weak = weak.clone(); let notify_thread = thread_id.clone();
                            context.connection.defer_until_responded(move || {tokio::spawn(async move {broadcast(&weak,json!({"method":"thread/goal/cleared","params":{"threadId":notify_thread}})).await;});});
                        }
                        Ok(json!({"cleared":cleared}))
                    },
                    _ => {
                        let status = parse_status(&value(&params,"status"))?;
                        let objective = parse_objective(&value(&params,"objective"))?;
                        let token_budget = parse_token_budget(&params)?;
                        let entry = require_thread(&threads,&thread_id).await?;
                        let (session,cwd,tasks) = session_parts(&entry).await;
                        let reference = session.with_session_manager(|manager|maho_ext_goal::goal_store_ref(manager,&cwd));
                        let now = chrono::Utc::now().timestamp_millis() as u64;
                        let goal = {
                            let _task = tasks.lock_owned().await;
                            match maho_ext_goal::read_goal(&reference).map_err(goal_error)? {
                                None => {
                                    let Some(objective) = objective.as_deref() else {return Err(JsonRpcError::new(-32600,format!("objective is required for new goal: {thread_id}")));};
                                    let created = maho_ext_goal::create_goal(&reference,objective,token_budget.if_present(),now).await.map_err(goal_error)?;
                                    match status {
                                        None|Some(GoalStatus::Active) => created,
                                        Some(status) => maho_ext_goal::update_goal(&reference,&GoalUpdate {status:Some(status),..Default::default()},source(status),now).await.map_err(goal_error)?,
                                    }
                                },
                                Some(_) => maho_ext_goal::update_goal(&reference,&GoalUpdate {objective,status,reason:None,token_budget:token_budget.update_value()},source(status),now).await.map_err(goal_error)?,
                            }
                        };
                        if goal.status == GoalStatus::Active {
                            let context = session.extension_command_context().await;
                            session.emit_extension_event(maho_ext_goal::GOAL_STORE_CHANGED_EVENT,&json!({"threadId":thread_id}));
                            session.emit_extension_event_typed(maho_ext_goal::GOAL_STORE_CHANGED_EVENT,&maho_ext_goal::GoalStoreChangedEvent { thread_id:thread_id.clone(), ctx:context.map(|context|context.context) });
                        }
                        let response = json!({"goal":to_thread_goal(&goal)});
                        let weak = weak.clone(); let notify_thread = thread_id.clone(); let notify_goal = response["goal"].clone();
                        context.connection.defer_until_responded(move || {tokio::spawn(async move {broadcast(&weak,json!({"method":"thread/goal/updated","params":{"threadId":notify_thread,"turnId":null,"goal":notify_goal}})).await;});});
                        Ok(response)
                    },
                }
            })
        })});
    }
}

async fn session_parts(entry: &Arc<Mutex<ThreadEntry>>) -> (maho_core::agent_session::AgentSession,String,Arc<Mutex<()>>) {
    let entry = entry.lock().await;
    (entry.session.clone(),entry.cwd.clone(),entry.tasks.clone())
}

async fn broadcast(weak: &Weak<RwLock<ServerCore>>,notification: Value) {
    let Some(core) = weak.upgrade() else {return;};
    if let Err(error) = core.read().await.broadcast_notification(notification,chrono::Utc::now().timestamp_millis() as u64).await {eprintln!("app-server goal notification: {}",error.message);}
}

async fn require_thread(threads: &ThreadRegistry,thread_id: &str) -> Result<Arc<Mutex<ThreadEntry>>,JsonRpcError> {
    let entry = threads.resume_thread(thread_id).await.map_err(|error|if error == format!("Thread not found: {thread_id}") {JsonRpcError::new(-32600,format!("thread not found: {thread_id}"))} else {JsonRpcError::new(-32603,error)})?;
    if entry.lock().await.session.session_file().is_none() {return Err(JsonRpcError::new(-32600,format!("ephemeral thread does not support goals: {thread_id}")));}
    Ok(entry)
}

fn value<'a>(params: &'a Map<String,Value>,key: &str) -> &'a Value {
    static NULL: Value = Value::Null;
    params.get(key).unwrap_or(&NULL)
}

fn source(status: Option<GoalStatus>) -> GoalUpdateSource {if status == Some(GoalStatus::Complete) {GoalUpdateSource::Model} else {GoalUpdateSource::User}}

fn goal_error(error: maho_ext_goal::GoalError) -> JsonRpcError {JsonRpcError::new(-32603,error.to_string())}

fn parse_status(value: &Value) -> Result<Option<GoalStatus>,JsonRpcError> {
    if value.is_null() {return Ok(None);}
    match value.as_str() {
        Some("active") => Ok(Some(GoalStatus::Active)),
        Some("paused") => Ok(Some(GoalStatus::Paused)),
        Some("complete") => Ok(Some(GoalStatus::Complete)),
        Some(other @ ("blocked"|"usageLimited"|"budgetLimited")) => Err(JsonRpcError::new(-32600,format!("unsupported goal status: {other}"))),
        _ => Err(JsonRpcError::new(-32602,"Invalid params: status must be active, paused, or complete")),
    }
}

fn parse_objective(value: &Value) -> Result<Option<String>,JsonRpcError> {
    if value.is_null() {return Ok(None);}
    value.as_str().map(|objective|Some(objective.to_owned())).ok_or_else(||JsonRpcError::new(-32602,"Invalid params: objective must be a string or null"))
}

struct TokenBudgetInput {present: bool,value: Option<f64>}
impl TokenBudgetInput {
    fn if_present(&self) -> Option<f64> {if self.present {self.value} else {None}}
    fn update_value(&self) -> Option<Option<f64>> {self.present.then_some(self.value)}
}

fn parse_token_budget(params: &Map<String,Value>) -> Result<TokenBudgetInput,JsonRpcError> {
    let Some(value) = params.get("tokenBudget") else {return Ok(TokenBudgetInput {present:false,value:None});};
    if value.is_null() {return Ok(TokenBudgetInput {present:true,value:None});}
    let number = value.as_f64().filter(|number|number.is_finite() && number.fract() == 0.0 && *number >= 0.0 && *number <= 9_007_199_254_740_991.0);
    number.map(|number|TokenBudgetInput {present:true,value:Some(number)}).ok_or_else(||JsonRpcError::new(-32602,"Invalid params: tokenBudget must be a non-negative integer or null"))
}
