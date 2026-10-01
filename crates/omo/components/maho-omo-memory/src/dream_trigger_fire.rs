use memory_core::{identity::layout::MemoryIdentityPaths,journal::cursor::ReflectionSnapshot,reflection::{machine::{CapturedConversation,DreamOrigin,ReflectionRequest,ReflectionTrigger,ReservedRun},reservation::ReservationResult}};
use crate::{dream_selector::{DreamSelectorOptions,compute_unreflected_volume,select_dream_conversations},dream_trigger_gates::{DreamGateRejection,DreamTriggerSettings,evaluate_dream_gates,read_last_dream_at_ms}};
pub trait DreamTriggerSession{
    type Error:From<std::io::Error>+From<crate::dream_selector::DreamSelectorError>;
    fn conversation_id(&self)->&str;
    fn paths(&self)->&MemoryIdentityPaths;
    fn capture_snapshot(&mut self,conversation:&str)->Result<Option<ReflectionSnapshot>,Self::Error>;
    fn try_reserve(&mut self,request:ReflectionRequest)->Result<ReservationResult,Self::Error>;
    fn launch(&mut self,run:ReservedRun)->Result<(),Self::Error>;
}
#[derive(Default)]pub struct ManualDreamRequest{pub focus:Option<String>,pub conversation_ids:Option<Vec<String>>,pub target_doc:Option<String>,pub deadline_at:Option<f64>}
#[derive(Debug,PartialEq,Eq)]pub enum DreamFireRejection{Gate(DreamGateRejection),Aborted,NoUnreflectedContent}
#[derive(Debug,PartialEq,Eq)]pub enum DreamFireOutcome{Fired{run_id:String,status:String},Rejected(DreamFireRejection)}
pub fn fire_dream<S:DreamTriggerSession>(session:&mut S,origin:DreamOrigin,settings:&DreamTriggerSettings,request:&ManualDreamRequest,now:&dyn Fn()->f64,aborted:&dyn Fn()->bool,warn:&mut dyn FnMut(&S::Error))->Result<DreamFireOutcome,S::Error>{
    let stopped=||aborted()||request.deadline_at.is_some_and(|deadline|now()>=deadline);
    let paths=session.paths();let options=DreamSelectorOptions{transcripts_dir:&paths.transcripts,current_conversation_id:Some(session.conversation_id()),auto_select_max:settings.auto_select_max,auto_select_max_bytes:settings.auto_select_max_chars,now_ms:now()};
    let decision=evaluate_dream_gates(origin,settings,now(),||read_last_dream_at_ms(&paths.runtime).map_err(S::Error::from),||compute_unreflected_volume(&options).map_err(S::Error::from))?;
    if let Err(rejection)=decision{return Ok(DreamFireOutcome::Rejected(DreamFireRejection::Gate(rejection)));}
    if stopped(){return Ok(DreamFireOutcome::Rejected(DreamFireRejection::Aborted));}
    let selected=match &request.conversation_ids{Some(ids)=>ids.clone(),None=>select_dream_conversations(&options,request.focus.as_deref()).map_err(S::Error::from)?.conversation_ids};
    if stopped(){return Ok(DreamFireOutcome::Rejected(DreamFireRejection::Aborted));}
    let mut conversation_ids=vec![];let mut snapshots=vec![];
    for conversation in selected{
        if stopped(){return Ok(DreamFireOutcome::Rejected(DreamFireRejection::Aborted));}
        let snapshot=match session.capture_snapshot(&conversation){Ok(snapshot)=>snapshot,Err(_) if stopped()=>return Ok(DreamFireOutcome::Rejected(DreamFireRejection::Aborted)),Err(error)=>return Err(error)};
        if stopped(){return Ok(DreamFireOutcome::Rejected(DreamFireRejection::Aborted));}
        if let Some(snapshot)=snapshot{conversation_ids.push(conversation.clone());snapshots.push(CapturedConversation{conversation_id:conversation,snapshot});}
    }
    if conversation_ids.is_empty(){return Ok(DreamFireOutcome::Rejected(DreamFireRejection::NoUnreflectedContent));}
    if stopped(){return Ok(DreamFireOutcome::Rejected(DreamFireRejection::Aborted));}
    let result=match session.try_reserve(ReflectionRequest{trigger:ReflectionTrigger::Dream,origin:Some(origin),conversation_ids,snapshots,focus:request.focus.clone(),recent_n:None,target_doc:request.target_doc.clone()}){Ok(result)=>result,Err(_) if stopped()=>return Ok(DreamFireOutcome::Rejected(DreamFireRejection::Aborted)),Err(error)=>return Err(error)};
    if stopped(){return Ok(DreamFireOutcome::Rejected(DreamFireRejection::Aborted));}
    let run_id=result.run.run_id.clone();if result.status=="active"&&let Err(error)=session.launch(result.run){warn(&error);}
    Ok(DreamFireOutcome::Fired{run_id,status:result.status})
}
#[cfg(test)]mod tests{
    use super::*;
    #[derive(Debug)]struct Error;
    impl From<std::io::Error> for Error{fn from(_:std::io::Error)->Self{Self}}
    impl From<crate::dream_selector::DreamSelectorError> for Error{fn from(_:crate::dream_selector::DreamSelectorError)->Self{Self}}
    struct Session{paths:MemoryIdentityPaths,captured:Vec<String>,reserved:Option<ReflectionRequest>,launches:usize,pending:bool,empty:bool,launch_fails:bool,abort:std::rc::Rc<std::cell::Cell<bool>>,abort_after_capture:bool}
    impl DreamTriggerSession for Session{
        type Error=Error;
        fn conversation_id(&self)->&str{"current"}fn paths(&self)->&MemoryIdentityPaths{&self.paths}
        fn capture_snapshot(&mut self,id:&str)->Result<Option<ReflectionSnapshot>,Error>{self.captured.push(id.into());if self.abort_after_capture{self.abort.set(true);}Ok((!self.empty).then(||ReflectionSnapshot{start_message_id:"a".into(),end_message_id:"a".into(),start_line:1,end_snapshot_line:1,entries:vec![]}))}
        fn try_reserve(&mut self,request:ReflectionRequest)->Result<ReservationResult,Error>{self.reserved=Some(request.clone());Ok(ReservationResult{status:if self.pending{"pending"}else{"active"}.into(),run:ReservedRun{run_id:"dream-1".into(),request,reserved_at:None,launcher_pid:None,launcher_hostname:None,launcher_process_start:None}})}
        fn launch(&mut self,_:ReservedRun)->Result<(),Error>{self.launches+=1;if self.launch_fails{Err(Error)}else{Ok(())}}
    }
    fn session(root:&std::path::Path)->Session{Session{paths:memory_core::identity::layout::build_identity_paths(root,"agent"),captured:vec![],reserved:None,launches:0,pending:false,empty:false,launch_fails:false,abort:Default::default(),abort_after_capture:false}}
    fn request()->ManualDreamRequest{ManualDreamRequest{conversation_ids:Some(vec!["one".into(),"two".into()]),focus:Some("focus".into()),target_doc:Some("target".into()),..Default::default()}}
    #[test]fn manual_bypasses_disabled_and_launches_only_active(){let root=tempfile::tempdir().unwrap();for pending in [false,true]{let mut session=session(root.path());session.pending=pending;let settings=DreamTriggerSettings{enabled:false,..Default::default()};let result=fire_dream(&mut session,DreamOrigin::Manual,&settings,&request(),&||0.0,&||false,&mut |_|panic!("unexpected warning")).unwrap();assert_eq!(result,DreamFireOutcome::Fired{run_id:"dream-1".into(),status:if pending{"pending"}else{"active"}.into()});assert_eq!(session.captured,["one","two"]);assert_eq!(session.launches,usize::from(!pending));let reserved=session.reserved.unwrap();assert_eq!(reserved.trigger,ReflectionTrigger::Dream);assert_eq!(reserved.focus.as_deref(),Some("focus"));assert_eq!(reserved.target_doc.as_deref(),Some("target"));assert_eq!(reserved.snapshots.len(),2);}}
    #[test]fn aborted_capture_does_not_reserve(){let root=tempfile::tempdir().unwrap();let mut session=session(root.path());session.abort_after_capture=true;let abort=session.abort.clone();let result=fire_dream(&mut session,DreamOrigin::Manual,&Default::default(),&request(),&||0.0,&||abort.get(),&mut |_|{}).unwrap();assert_eq!(result,DreamFireOutcome::Rejected(DreamFireRejection::Aborted));assert_eq!(session.captured,["one"]);assert!(session.reserved.is_none());}
    #[test]fn empty_snapshots_reject_without_reservation(){let root=tempfile::tempdir().unwrap();let mut session=session(root.path());session.empty=true;let result=fire_dream(&mut session,DreamOrigin::Manual,&Default::default(),&request(),&||0.0,&||false,&mut |_|{}).unwrap();assert_eq!(result,DreamFireOutcome::Rejected(DreamFireRejection::NoUnreflectedContent));assert!(session.reserved.is_none());}
    #[test]fn launch_error_warns_without_losing_reservation(){let root=tempfile::tempdir().unwrap();let mut session=session(root.path());session.launch_fails=true;let mut warnings=0;let result=fire_dream(&mut session,DreamOrigin::Manual,&Default::default(),&request(),&||0.0,&||false,&mut |_|warnings+=1).unwrap();assert!(matches!(result,DreamFireOutcome::Fired{..}));assert_eq!(warnings,1);assert!(session.reserved.is_some());}
    #[test]fn deadline_boundary_stops_before_capture(){let root=tempfile::tempdir().unwrap();let mut session=session(root.path());let mut request=request();request.deadline_at=Some(1.0);let result=fire_dream(&mut session,DreamOrigin::Manual,&Default::default(),&request,&||1.0,&||false,&mut |_|{}).unwrap();assert_eq!(result,DreamFireOutcome::Rejected(DreamFireRejection::Aborted));assert!(session.captured.is_empty());}
}
