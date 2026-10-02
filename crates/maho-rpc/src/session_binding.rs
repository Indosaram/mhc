use crate::session_attribution::{SessionActivityRegistry,ToolAttributionSpans};
pub struct BindingRecords{tools:ToolAttributionSpans}
impl BindingRecords{
    pub fn new(session_id:String,activity:SessionActivityRegistry)->Self{Self{tools:ToolAttributionSpans::new(session_id,activity)}}
    pub fn enqueue_records(&mut self,chunk:&str,mut enqueue:impl FnMut(serde_json::Value))->Result<(),serde_json::Error>{
        for line in chunk.split('\n').filter(|line|!line.is_empty()){let record=serde_json::from_str(line)?;self.tools.observe(&record);enqueue(record);}
        Ok(())
    }
    pub fn dispose(&mut self){self.tools.close_all();}
}
#[cfg(test)]mod tests{use super::*;#[test]fn attribution_commits_before_event_publication_and_dispose_closes_spans(){let activity=SessionActivityRegistry::default();let mut binding=BindingRecords::new("session".into(),activity.clone());let mut published=0;binding.enqueue_records("\n{\"type\":\"tool_execution_start\",\"toolCallId\":\"id\",\"toolName\":\"bash\"}\n",|record|{published+=1;assert_eq!(record["toolName"],"bash");assert_eq!(activity.since(activity.mark()).unwrap().tool.as_deref(),Some("bash"));}).unwrap();assert_eq!(published,1);binding.dispose();assert!(activity.since(activity.mark()).is_none());}#[test]fn malformed_record_stops_stream_before_later_publication(){let activity=SessionActivityRegistry::default();let mut binding=BindingRecords::new("s".into(),activity);let mut records=vec![];assert!(binding.enqueue_records("{}\ninvalid\n{}",|record|records.push(record)).is_err());assert_eq!(records.len(),1);}}
