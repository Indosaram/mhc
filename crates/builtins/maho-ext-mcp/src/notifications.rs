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
        let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.timer.is_some(){return;}
        let wait=state.last_refresh.map_or(self.delay,|last|self.delay.max(self.min_interval.saturating_sub(last.elapsed())));
        let deadline=Instant::now()+wait;let shared=self.state.clone();
        state.timer=Some(tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            {let mut state=shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);state.timer=None;state.last_refresh=Some(Instant::now());}
            refresh().await;
        }));
    }
    pub fn dispose(&self) {let mut state=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if let Some(timer)=state.timer.take(){timer.abort();}}
}
impl Drop for McpListChangeCoalescer {fn drop(&mut self){self.dispose();}}
