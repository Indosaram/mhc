use crate::constants::{AGENT_ROW_CAP,TOOL_ROW_CAP};
#[derive(Clone,Copy,Debug,PartialEq,Eq)] pub enum PanelChildStatus { Queued,Running,Resuming,Ending,Finished,Failed,Cancelled }
impl PanelChildStatus { pub fn terminal(self)->bool { matches!(self,Self::Finished|Self::Failed|Self::Cancelled) } pub fn running(self)->bool { matches!(self,Self::Queued|Self::Running) } }
#[derive(Clone,Debug,PartialEq)] pub struct PanelChild { pub id:String,pub name:String,pub status:PanelChildStatus,pub started_at:u64,pub finished_at:Option<u64>,pub activity:Option<String>,pub cost:Option<f64>,pub card:Vec<crate::types::PanelRow>,pub category:Option<String>,pub parked_reason:Option<String>,pub host:Option<String>,pub turns:Option<u64>,pub tokens:Option<u64> }
#[derive(Clone,Debug)] pub struct PanelToolCall { pub name:String,pub detail:Option<String>,pub at:u64 }
#[derive(Clone,Debug,Default)]
pub struct PanelChildUpdate {
    pub id:String, pub name:Option<String>, pub status:Option<PanelChildStatus>,
    pub started_at:Option<u64>, pub finished_at:Option<u64>, pub activity:Option<String>,
    pub cost:Option<f64>, pub card:Option<Vec<crate::types::PanelRow>>,
    pub category:Option<String>, pub parked_reason:Option<String>, pub host:Option<String>,
    pub turns:Option<u64>, pub tokens:Option<u64>,
}
#[derive(Clone,Debug,Default)] pub struct PanelState { pub children:Vec<PanelChild>,pub tools:Vec<PanelToolCall>,pub child_spend:f64 }
impl From<PanelChild> for PanelChildUpdate {
    fn from(child:PanelChild)->Self {
        Self {id:child.id,name:Some(child.name),status:Some(child.status),started_at:Some(child.started_at),
            finished_at:child.finished_at,activity:child.activity,cost:child.cost,card:Some(child.card),
            category:child.category,parked_reason:child.parked_reason,host:child.host,turns:child.turns,tokens:child.tokens}
    }
}
impl PanelState {
    pub fn merge_child(&mut self,update:PanelChildUpdate,now:u64)->bool {
        let previous=self.children.iter().find(|child|child.id==update.id);
        let status=update.status.or_else(||previous.map(|child|child.status)).unwrap_or(PanelChildStatus::Queued);
        let child=PanelChild {
            name:update.name.or_else(||previous.map(|child|child.name.clone())).unwrap_or_else(||update.id.clone()),
            status,
            category:update.category.or_else(||previous.and_then(|child|child.category.clone())),
            parked_reason:if matches!(status,PanelChildStatus::Resuming|PanelChildStatus::Ending) {update.parked_reason.or_else(||previous.and_then(|child|child.parked_reason.clone()))} else {None},
            host:update.host.or_else(||previous.and_then(|child|child.host.clone())),
            turns:update.turns.or_else(||previous.and_then(|child|child.turns)),
            tokens:update.tokens.or_else(||previous.and_then(|child|child.tokens)),
            started_at:update.started_at.or_else(||previous.map(|child|child.started_at)).unwrap_or(now),
            finished_at:update.finished_at.or_else(||previous.and_then(|child|child.finished_at)),
            activity:update.activity.or_else(||previous.and_then(|child|child.activity.clone())),
            cost:update.cost.or_else(||previous.and_then(|child|child.cost)),
            card:update.card.or_else(||previous.map(|child|child.card.clone())).unwrap_or_default(),
            id:update.id,
        };
        self.upsert_child(child)
    }
    pub fn upsert_child(&mut self,child:PanelChild)->bool {
        let previous=self.children.iter().position(|c|c.id==child.id);
        if previous.is_some_and(|i|self.children[i]==child) { return false; }
        if child.status.terminal() && !previous.is_some_and(|i|self.children[i].status.terminal()) { self.child_spend+=child.cost.unwrap_or(0.0); }
        if let Some(i)=previous { self.children[i]=child; } else { self.children.push(child); }
        while self.children.len()>AGENT_ROW_CAP { let oldest=self.children.iter().enumerate().filter(|(_,c)|c.status.terminal()).min_by_key(|(_,c)|c.finished_at.unwrap_or(c.started_at)).map(|(i,_)|i);if let Some(i)=oldest { self.children.remove(i); } else { break; } }
        self.children.sort_by_key(|c|c.started_at);true
    }
    pub fn record_tool(&mut self,call:PanelToolCall) { self.tools.push(call);if self.tools.len()>TOOL_ROW_CAP { self.tools.drain(..self.tools.len()-TOOL_ROW_CAP); } }
    pub fn clear_tools(&mut self) { self.tools.clear(); }
    pub fn reset(&mut self) { *self=Self::default(); }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_updates_keep_known_fields_and_count_terminal_cost_once() {
        let mut state=PanelState::default();
        state.merge_child(PanelChildUpdate {id:"child".into(),name:Some("worker".into()),cost:Some(0.5),..Default::default()},10);
        assert!(state.merge_child(PanelChildUpdate {id:"child".into(),status:Some(PanelChildStatus::Finished),..Default::default()},20));
        assert_eq!(state.children[0].name,"worker");
        assert_eq!(state.children[0].started_at,10);
        assert!((state.child_spend-0.5).abs()<f64::EPSILON);
        assert!(!state.merge_child(PanelChildUpdate {id:"child".into(),status:Some(PanelChildStatus::Finished),..Default::default()},30));
        assert!((state.child_spend-0.5).abs()<f64::EPSILON);
    }
    #[test]
    fn capacity_never_evicts_running_children() {
        let mut state=PanelState::default();
        for index in 0..AGENT_ROW_CAP+1 {
            state.merge_child(PanelChildUpdate {id:index.to_string(),status:Some(PanelChildStatus::Running),..Default::default()},0);
        }
        assert_eq!(state.children.len(),AGENT_ROW_CAP+1);
        state.merge_child(PanelChildUpdate {id:"0".into(),status:Some(PanelChildStatus::Finished),..Default::default()},0);
        assert_eq!(state.children.len(),AGENT_ROW_CAP);
        assert!(state.children.iter().all(|child|child.status==PanelChildStatus::Running));
    }
}
