use std::collections::BTreeSet;
use std::{future::Future,sync::{Arc,Mutex},time::Duration};
use tokio::{task::JoinHandle,time::Instant};
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct McpCatalogDiff {pub added:Vec<String>,pub removed:Vec<String>,pub unchanged:Vec<String>}
pub fn diff_mcp_tool_names(previous:&[String],next:&[String])->McpCatalogDiff {
    let previous:BTreeSet<_>=previous.iter().cloned().collect();let next:BTreeSet<_>=next.iter().cloned().collect();
    McpCatalogDiff {added:next.difference(&previous).cloned().collect(),removed:previous.difference(&next).cloned().collect(),unchanged:next.intersection(&previous).cloned().collect()}
}
pub fn format_mcp_list_changed_delta(diff:&McpCatalogDiff)->String {
    let mut parts=Vec::new();
    if !diff.added.is_empty(){parts.push(format!("{} added (inactive)",diff.added.len()));}
    if !diff.removed.is_empty(){parts.push(format!("{} removed",diff.removed.len()));}
    if parts.is_empty(){"no change".into()}else{parts.join(", ")}
}
struct CoalescerState {timer:Option<JoinHandle<()>>,last_refresh:Option<Instant>}
pub struct McpListChangeCoalescer {state:Arc<Mutex<CoalescerState>>,delay:Duration,min_interval:Duration}
impl McpListChangeCoalescer {
    pub fn new(delay:Option<Duration>,min_interval:Option<Duration>)->Self {
        Self {state:Arc::new(Mutex::new(CoalescerState {timer:None,last_refresh:None})),delay:delay.unwrap_or(Duration::from_millis(300)),min_interval:min_interval.unwrap_or(Duration::from_secs(1))}
    }
    pub fn notify<F,Fut>(&self,refresh:F) where F:FnOnce()->Fut+Send+'static,Fut:Future<Output=()>+Send+'static {
        let sink=crate::wrap::McpAsyncErrorSink {logger:Arc::new(|scope,data|{eprintln!("MCP {scope}: {data}");Ok(())}),notify:None};
        self.notify_guarded(move ||async move {refresh().await;Ok(())},sink);
    }
    pub fn notify_guarded<F,Fut>(&self,refresh:F,sink:crate::wrap::McpAsyncErrorSink) where F:FnOnce()->Fut+Send+'static,Fut:Future<Output=Result<(),crate::errors::McpError>>+Send+'static {
        let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.timer.is_some(){return;}
        let wait=state.last_refresh.map_or(self.delay,|last|self.delay.max(self.min_interval.saturating_sub(last.elapsed())));
        let deadline=Instant::now()+wait;let shared=self.state.clone();
        state.timer=Some(tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            {let mut state=shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);state.timer=None;state.last_refresh=Some(Instant::now());}
            crate::wrap::wrap_async("mcp.list_changed",async {refresh().await},&sink).await;
        }));
    }
    pub fn dispose(&self) {let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if let Some(timer)=state.timer.take(){timer.abort();}}
}
impl Drop for McpListChangeCoalescer {fn drop(&mut self){self.dispose();}}
pub fn build_mcp_tombstone_definition(name:&str,server:&str)->maho_ext_api::ToolDefinition {
    let server=server.to_owned();
    let mut tool=maho_ext_api::ToolDefinition::new(name,&format!("This MCP tool was removed from {server} and is no longer available."),serde_json::json!({"type":"object","properties":{},"required":[]}),Arc::new(move |_| {
        let server=server.clone();
        Box::pin(async move {Err(maho_ext_api::ToolError::Message(format!("tool no longer available on {server}")))})
    }));
    tool.label=name.into();tool.execution_mode=Some(maho_ext_api::ToolExecutionMode::Parallel);tool
}
pub fn subscribe_mcp_list_changed(client:&crate::transport_sdk::McpClient,on_change:Arc<dyn Fn()+Send+Sync>)->JoinHandle<()> {
    let mut notifications=client.notifications.subscribe();
    tokio::spawn(async move {loop {
        match notifications.recv().await {
            Ok(value)=>{if matches!(crate::notification_schemas::parse_notification(&value),Some(crate::notification_schemas::McpNotification::ListChanged {..})){on_change();}}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed)=>return,
        }
    }})
}
