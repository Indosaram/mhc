use std::{future::Future,pin::Pin};
pub const SESSION_SHUTDOWN_DRAIN_BUDGET_MS:u64=1500;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum ShutdownReason{Quit,Reload,New,Resume,Fork}
#[derive(Clone,Default)]
pub struct ShutdownSignal(memory_core::facts::queue::FactsAbortSignal);
impl ShutdownSignal{pub fn aborted(&self)->bool{self.0.is_aborted()}fn abort(&self){self.0.abort();}pub fn facts_signal(&self)->&memory_core::facts::queue::FactsAbortSignal{&self.0}}
struct AbortOnReturn(ShutdownSignal);
impl Drop for AbortOnReturn{fn drop(&mut self){self.0.abort();}}
pub struct ShutdownEvaluatorInput<'a>{pub reason:ShutdownReason,pub session_id:&'a str,pub deadline_at:f64,pub signal:ShutdownSignal}
pub type ShutdownWork<'a>=Pin<Box<dyn Future<Output=Result<(),String>>+Send+'a>>;
pub trait ShutdownDrainSteps{
    fn flush_journal<'a>(&'a mut self,session:&'a str,signal:ShutdownSignal)->ShutdownWork<'a>;
    fn enqueue_final_delta<'a>(&'a mut self,session:&'a str,signal:ShutdownSignal)->ShutdownWork<'a>;
    fn flush_skills_usage<'a>(&'a mut self,session:&'a str,signal:ShutdownSignal)->ShutdownWork<'a>;
    fn launch_facts<'a>(&'a mut self,session:&'a str,signal:ShutdownSignal)->ShutdownWork<'a>;
}
pub trait ShutdownEvaluator:Send{fn evaluate<'a>(&'a mut self,input:ShutdownEvaluatorInput<'a>)->ShutdownWork<'a>;}
pub struct ShutdownDrain<S>{pub steps:S,pub(crate) evaluators:Vec<Box<dyn ShutdownEvaluator>>}
pub fn create_shutdown_drain<S:ShutdownDrainSteps>(steps:S)->ShutdownDrain<S>{ShutdownDrain{steps,evaluators:vec![]}}
pub fn shutdown_deadline_at(now:impl FnOnce()->f64)->f64{now()+SESSION_SHUTDOWN_DRAIN_BUDGET_MS as f64}
impl<S:ShutdownDrainSteps> ShutdownDrain<S>{
    pub fn register_evaluator(&mut self,evaluator:Box<dyn ShutdownEvaluator>){self.evaluators.push(evaluator);}
    pub async fn run<N:Fn()->f64+?Sized,W:FnMut(&str,Option<&str>)+?Sized>(&mut self,reason:ShutdownReason,session:&str,deadline:f64,now:&N,warn:&mut W){
        let signal=ShutdownSignal::default();let _abort=AbortOnReturn(signal.clone());
        for name in ["journal-flush","facts-enqueue","skills-usage-flush","facts-launch"]{
            if reason!=ShutdownReason::Quit&&name=="skills-usage-flush"{return;}
            if now()>=deadline{signal.abort();warn(name,None);return;}
            let work=match name{"journal-flush"=>self.steps.flush_journal(session,signal.clone()),"facts-enqueue"=>self.steps.enqueue_final_delta(session,signal.clone()),"skills-usage-flush"=>self.steps.flush_skills_usage(session,signal.clone()),_=>self.steps.launch_facts(session,signal.clone())};
            if !run_step(work,name,deadline,now,&signal,warn).await{return;}
        }
        for evaluator in &mut self.evaluators{
            let name="shutdown-evaluator";
            if now()>=deadline{signal.abort();warn(name,None);return;}
            let work=evaluator.evaluate(ShutdownEvaluatorInput{reason,session_id:session,deadline_at:deadline,signal:signal.clone()});
            if !run_step(work,name,deadline,now,&signal,warn).await{return;}
        }
    }
}
async fn run_step<N:Fn()->f64+?Sized,W:FnMut(&str,Option<&str>)+?Sized>(work:ShutdownWork<'_>,name:&str,deadline:f64,now:&N,signal:&ShutdownSignal,warn:&mut W)->bool{
    let remaining=std::time::Duration::from_secs_f64(((deadline-now()).max(0.0)/1000.0).min(u64::MAX as f64));
    tokio::select!{biased;
        result=work=>{if let Err(error)=result{warn(name,Some(&error));}true}
        ()=tokio::time::sleep(remaining)=>{signal.abort();warn(name,None);false}
    }
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn facts_signal_shares_abort_state_with_shutdown_clone(){let signal=ShutdownSignal::default();let facts=signal.facts_signal().clone();assert!(!facts.is_aborted());signal.clone().abort();assert!(facts.is_aborted());assert!(signal.aborted());}
    #[derive(Default)]struct Steps{order:Vec<&'static str>,signal:Option<ShutdownSignal>,fail:bool,stall:bool}
    impl ShutdownDrainSteps for Steps{
        fn flush_journal<'a>(&'a mut self,_:&'a str,signal:ShutdownSignal)->ShutdownWork<'a>{
            self.order.push("a");self.signal=Some(signal);
            Box::pin(async move{
                if self.stall{std::future::pending::<()>().await;}
                if self.fail{Err("journal".into())}else{Ok(())}
            })
        }
        fn enqueue_final_delta<'a>(&'a mut self,_:&'a str,_:ShutdownSignal)->ShutdownWork<'a>{self.order.push("b");Box::pin(std::future::ready(Ok(())))}
        fn flush_skills_usage<'a>(&'a mut self,_:&'a str,_:ShutdownSignal)->ShutdownWork<'a>{self.order.push("c-prime");Box::pin(std::future::ready(Ok(())))}
        fn launch_facts<'a>(&'a mut self,_:&'a str,_:ShutdownSignal)->ShutdownWork<'a>{self.order.push("c");Box::pin(std::future::ready(Ok(())))}
    }
    struct Evaluator{order:std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,fail:bool}
    impl ShutdownEvaluator for Evaluator{fn evaluate<'a>(&'a mut self,input:ShutdownEvaluatorInput<'a>)->ShutdownWork<'a>{assert!(!input.signal.aborted());self.order.lock().unwrap_or_else(|error|panic!("order lock: {error}")).push(if self.fail{"d1"}else{"d2"});Box::pin(std::future::ready(if self.fail{Err("evaluator".into())}else{Ok(())}))}}
    #[test]fn budget_is_1500(){assert_eq!(shutdown_deadline_at(||10000.0),11500.0);}
    #[tokio::test]async fn reload_skips_registered_evaluators(){let mut drain=create_shutdown_drain(Steps::default());let order=std::sync::Arc::new(std::sync::Mutex::new(vec![]));drain.register_evaluator(Box::new(Evaluator{order:order.clone(),fail:false}));drain.run(ShutdownReason::Reload,"session",1500.0,&||0.0,&mut |_,_|panic!("warn")).await;assert!(order.lock().unwrap().is_empty());assert_eq!(drain.steps.order,["a","b"]);}
    #[tokio::test]async fn shared_budget_blocks_next_step_at_boundary(){let mut drain=create_shutdown_drain(Steps::default());let reads=std::cell::Cell::new(0);let clock=||{let read=reads.get();reads.set(read+1);if read<2{0.0}else{1500.0}};let mut warnings=vec![];drain.run(ShutdownReason::Quit,"session",1500.0,&clock,&mut |name,error|warnings.push((name.to_owned(),error.map(str::to_owned)))).await;assert_eq!(drain.steps.order,["a"]);assert_eq!(warnings,[("facts-enqueue".into(),None)]);assert!(drain.steps.signal.as_ref().unwrap().aborted());}
    #[tokio::test]async fn evaluator_receives_reason_session_and_absolute_deadline(){struct Check;impl ShutdownEvaluator for Check{fn evaluate<'a>(&'a mut self,input:ShutdownEvaluatorInput<'a>)->ShutdownWork<'a>{assert_eq!(input.reason,ShutdownReason::Quit);assert_eq!(input.session_id,"session");assert_eq!(input.deadline_at,2500.0);assert!(!input.signal.aborted());Box::pin(std::future::ready(Ok(())))}}let mut drain=create_shutdown_drain(Steps::default());drain.register_evaluator(Box::new(Check));drain.run(ShutdownReason::Quit,"session",2500.0,&||1000.0,&mut |_,_|panic!("warn")).await;assert!(drain.steps.signal.as_ref().unwrap().aborted());}
    #[tokio::test]async fn quit_order_and_return_abort(){let mut drain=create_shutdown_drain(Steps::default());let order=std::sync::Arc::new(std::sync::Mutex::new(vec![]));for fail in [true,false]{drain.register_evaluator(Box::new(Evaluator{order:order.clone(),fail}));}let mut warnings=vec![];drain.run(ShutdownReason::Quit,"session",1500.0,&||0.0,&mut |name,error|warnings.push((name.to_owned(),error.map(str::to_owned)))).await;assert_eq!(drain.steps.order,["a","b","c-prime","c"]);assert_eq!(*order.lock().unwrap(),["d1","d2"]);assert_eq!(warnings.len(),1);assert!(drain.steps.signal.as_ref().unwrap().aborted());}
    #[tokio::test]async fn nonquit_runs_only_flush_and_enqueue(){for reason in [ShutdownReason::Reload,ShutdownReason::New,ShutdownReason::Resume,ShutdownReason::Fork]{let mut drain=create_shutdown_drain(Steps::default());drain.run(reason,"session",1500.0,&||0.0,&mut |_,_|panic!("unexpected warning")).await;assert_eq!(drain.steps.order,["a","b"]);assert!(drain.steps.signal.as_ref().unwrap().aborted());}}
    #[tokio::test(start_paused=true)]async fn stalled_step_exhausts_exact_budget_without_later_work(){let mut drain=create_shutdown_drain(Steps{stall:true,..Default::default()});let before=tokio::time::Instant::now();let mut warnings=vec![];drain.run(ShutdownReason::Quit,"session",1500.0,&||0.0,&mut |name,error|warnings.push((name.to_owned(),error.map(str::to_owned)))).await;assert_eq!(before.elapsed(),std::time::Duration::from_millis(1500));assert_eq!(drain.steps.order,["a"]);assert!(drain.steps.signal.as_ref().unwrap().aborted());assert_eq!(warnings,[("journal-flush".into(),None)]);}
    #[tokio::test]async fn expired_deadline_starts_no_work(){let mut drain=create_shutdown_drain(Steps::default());let mut warnings=0;drain.run(ShutdownReason::Quit,"session",1500.0,&||1500.0,&mut |_,_|warnings+=1).await;assert!(drain.steps.order.is_empty());assert_eq!(warnings,1);}
    #[tokio::test]async fn failed_step_continues(){let mut drain=create_shutdown_drain(Steps{fail:true,..Default::default()});let mut warnings=0;drain.run(ShutdownReason::Quit,"session",1500.0,&||0.0,&mut |_,_|warnings+=1).await;assert_eq!(drain.steps.order,["a","b","c-prime","c"]);assert_eq!(warnings,1);}
}
