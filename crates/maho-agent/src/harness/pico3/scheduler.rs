use std::{cell::{Cell,RefCell},collections::BTreeMap,rc::{Rc,Weak}};
use maho_ai::utils::abort::AbortController;
use crate::harness::context::with_abort_signal;
use super::{types::*,session::{Session,TxImpl,ExecutableKind,CommitOptions,check_abort,is_core_kind}};

pub(crate) type Handler = Rc<dyn for<'a> Fn(&'a Task,InvocationToken,InvocationMode,&'a Context)->LocalBoxFuture<'a,Result<Step>>>;
pub(crate) type AbortHandler = Rc<dyn for<'a> Fn(&'a Task,InvocationToken,InvocationMode,&'a Context)->LocalBoxFuture<'a,Result<AbortClosure>>>;
pub type TypedRuntimeFactory<K> = Rc<dyn Fn(Task,InvocationToken,InvocationMode,Context)->Result<Rc<dyn Runtime<<K as Kind>::Hooks,<K as Kind>::Tx>>>>;
pub type TxAccess<K> = for<'a> fn(&'a mut TxImpl)->&'a mut <K as Kind>::Tx;
pub fn core_access(tx:&mut TxImpl)->&mut (dyn CoreTx+'static){tx}
pub fn task_access(tx:&mut TxImpl)->&mut (dyn TaskTx+'static){tx}
pub(crate) type Closure = Box<dyn for<'a> FnOnce(&'a mut TxImpl,Task,Context)->LocalBoxFuture<'a,Result<Completion>>>;
pub(crate) type AbortClosure = Box<dyn for<'a> FnOnce(&'a mut TxImpl,Task,Context)->LocalBoxFuture<'a,Result<JsonValue>>>;
pub(crate) type NextBuilder = Box<dyn for<'a> FnOnce(&'a mut TxImpl,Task,Context)->LocalBoxFuture<'a,Result<Transition>>>;
pub(crate) enum Step { Next(Checkpoint), Build(NextBuilder), Done(Closure) }
pub(crate) enum Transition { Checkpoint(Checkpoint), Done(Completion), Retry }
struct Invocation { mode:InvocationMode,controller:AbortController,tokens:Rc<RefCell<Vec<InvocationToken>>>,done:tokio::sync::watch::Receiver<bool> }
type Waiters<T> = RefCell<BTreeMap<Id,BTreeMap<u64,tokio::sync::oneshot::Sender<T>>>>;
type IdleWaiters = RefCell<BTreeMap<u64, (Option<Id>, tokio::sync::oneshot::Sender<()>)>>;
pub struct Scheduler {
    pub session:Rc<Session>,ctx:Context,
    enabled:Cell<bool>,holds:Cell<usize>,dirty:Cell<bool>,draining:Cell<bool>,
    invocations:RefCell<BTreeMap<Id,Invocation>>,
    tasks:Waiters<Task>,inputs:Waiters<Input>,idle:IdleWaiters,
    waiter_id:Cell<u64>,myself:Weak<Self>,
}
pub struct Hold {scheduler:Weak<Scheduler>,released:bool}
impl Drop for Hold{fn drop(&mut self){if !self.released{if let Some(s)=self.scheduler.upgrade(){s.holds.set(s.holds.get()-1);if s.holds.get()==0{s.kick();}}self.released=true;}}}
struct Lease {token:InvocationToken}
impl Drop for Lease{fn drop(&mut self){self.token.revoke();}}
impl Scheduler {
    pub fn new(session:Rc<Session>,ctx:Context)->Rc<Self>{
        let scheduler=Rc::new_cyclic(|myself|Self{session:session.clone(),ctx,enabled:Cell::new(false),holds:Cell::new(0),dirty:Cell::new(false),draining:Cell::new(false),invocations:RefCell::new(BTreeMap::new()),tasks:RefCell::new(BTreeMap::new()),inputs:RefCell::new(BTreeMap::new()),idle:RefCell::new(BTreeMap::new()),waiter_id:Cell::new(0),myself:myself.clone()});
        let weak=Rc::downgrade(&scheduler);
        session.subscribe(Rc::new(move|changes|{
            let Some(s)=weak.upgrade()else{return Ok(())};
            for task in &changes.tasks{
                if task.abort.is_some(){s.revoke_run(task.id);}
                if task.status==TaskStatus::Terminal && let Some(waiters)=s.tasks.borrow_mut().remove(&task.id){for waiter in waiters.into_values(){let _=waiter.send(task.clone());}}
            }
            for input in &changes.inputs{if matches!(input.status,InputStatus::Done|InputStatus::Unanswered) && let Some(waiters)=s.inputs.borrow_mut().remove(&input.id){for waiter in waiters.into_values(){let _=waiter.send(input.clone());}}}
            if !changes.tasks.is_empty(){s.kick();}s.check_idle();Ok(())
        }));scheduler
    }
    pub fn resume(&self){self.enabled.set(true);self.kick();}
    pub fn stop(&self){self.enabled.set(false);}
    pub fn hold(&self)->Hold{self.holds.set(self.holds.get()+1);Hold{scheduler:self.myself.clone(),released:false}}
    pub fn kick(&self){
        if !self.enabled.get(){return;}self.dirty.set(true);
        if self.holds.get()>0||self.draining.replace(true){return;}
        let weak=self.myself.clone();tokio::task::spawn_local(async move{if let Some(s)=weak.upgrade(){if let Err(error)=s.drain().await{s.session.report(&error);}s.draining.set(false);s.check_idle();}});
    }
    async fn drain(&self)->Result<()>{
        while self.dirty.replace(false)&&self.enabled.get()&&self.holds.get()==0{
            let session=self.session.clone();let active:std::collections::BTreeSet<_>=self.invocations.borrow().keys().copied().collect();
            let reserved=self.session.commit(Invoker::Kernel{conversation_id:None},move|tx,_|Box::pin(async move{
                let tasks:Vec<_>=session.live_tasks.borrow().values().cloned().collect();let mut out=Vec::new();
                for mut task in tasks{
                    if !session.kinds.borrow().contains_key(&task.kind){continue;}
                    if active.contains(&task.id){continue;}
                    let mode=if task.abort.is_some(){InvocationMode::Abort}else{InvocationMode::Run};
                    if mode==InvocationMode::Run&&task.status==TaskStatus::Pending{
                        let mut ready=true;for dependency in &task.after{if tx.task(*dependency).await?.is_none_or(|t|t.status!=TaskStatus::Terminal){ready=false;break;}}if !ready{continue;}
                    }
                    if task.status==TaskStatus::Pending{task.status=TaskStatus::Running;tx.set_task(task.clone())?;}
                    out.push((task,mode));
                }Ok(out)
            }),&self.ctx,CommitOptions::default()).await?.value;
            if !self.enabled.get()||self.holds.get()>0{self.dirty.set(true);break;}
            for(task,mode)in reserved{self.dispatch(task,mode);}
        }Ok(())
    }
    fn dispatch(&self,task:Task,mode:InvocationMode){
        let controller=AbortController::new();let ctx=with_abort_signal(controller.signal(),&self.ctx);
        let (finished,done)=tokio::sync::watch::channel(false);let tokens=Rc::new(RefCell::new(Vec::new()));
        self.invocations.borrow_mut().insert(task.id,Invocation{mode,controller,tokens:tokens.clone(),done});
        let weak=self.myself.clone();tokio::task::spawn_local(async move{
            let Some(s)=weak.upgrade()else{return};
            let result=s.invoke(task.clone(),mode,&ctx,&tokens).await;
            for token in tokens.borrow().iter(){token.revoke();}
            if let Err(error)=result && !ctx.is_aborted(){s.session.report(&error);if let Err(fault)=s.fault(&task,error.to_string()).await{s.session.report(&fault);}}
            s.invocations.borrow_mut().remove(&task.id);finished.send_replace(true);
                let retired = !s.session.live_tasks.borrow().contains_key(&task.id);
                if retired && let Err(error)=s.session.retire(&task,&s.ctx).await{s.session.report(&error);}
            s.kick();s.check_idle();
        });
    }
    fn lease(&self,task:&Task,kind:&ExecutableKind,mode:InvocationMode,tokens:&RefCell<Vec<InvocationToken>>)->(Lease,Invoker){
        let token=InvocationToken::new(task.id,mode);tokens.borrow_mut().push(token.clone());
        let invoker=Invoker::Task{token:token.clone(),id:task.id,conversation_id:task.conversation_id,kind:kind.token.clone(),core:is_core_kind(&task.kind),mode};
        (Lease{token},invoker)
    }
    async fn invoke(&self,task:Task,mode:InvocationMode,ctx:&Context,tokens:&RefCell<Vec<InvocationToken>>)->Result<()>{
        let kind=self.session.kinds.borrow().get(&task.kind).cloned().ok_or_else(||Error::TaskContractFault{kind:task.kind.clone(),what:"kind was unregistered".into()})?;
        if mode==InvocationMode::Abort{
            let (lease,_)=self.lease(&task,&kind,mode,tokens);
            let closure=(kind.abort)(&task,lease.token.clone(),mode,ctx).await?;drop(lease);
            let (_lease,invoker)=self.lease(&task,&kind,mode,tokens);
            return self.session.commit(invoker,|tx,line_ctx|Box::pin(async move{
                let Some(current)=tx.task(task.id).await?else{return Ok(())};let result=closure(tx,current.clone(),line_ctx).await?;
                let mut patched=tx.task(task.id).await?.unwrap_or(current);patched.status=TaskStatus::Terminal;patched.outcome=Some(Outcome::Aborted{result});tx.set_task(patched)
            }),ctx,CommitOptions{docs:Vec::new(),closing:true}).await.map(|_|());
        }
        let mut current=task.clone();
        loop{
            check_abort(ctx)?;
            let handler=match &current.checkpoint{Some(cp)=>kind.phases.get(&cp.phase).cloned(),None=>Some(kind.initial.clone())}.ok_or_else(||Error::TaskContractFault{kind:task.kind.clone(),what:"no handler for checkpoint phase".into()})?;
            let (lease,_)=self.lease(&task,&kind,mode,tokens);
            let step=handler(&current,lease.token.clone(),mode,ctx).await?;drop(lease);check_abort(ctx)?;
            let (_lease,invoker)=self.lease(&task,&kind,mode,tokens);
            let closing=matches!(&step,Step::Done(_));
            let task=task.clone();let kind=kind.clone();
            let result=self.session.commit(invoker,|tx,line_ctx|Box::pin(async move{
                let Some(live)=tx.task(task.id).await?else{return Ok(None)};if live.abort.is_some(){return Ok(None);}
                let next=match step{Step::Done(closure)=>Transition::Done(closure(tx,live.clone(),line_ctx).await?),Step::Build(builder)=>builder(tx,live.clone(),line_ctx).await?,Step::Next(cp)=>Transition::Checkpoint(cp)};
                match next{
                    Transition::Retry=>Ok(Some(tx.task(task.id).await?.unwrap_or(live))),
                    Transition::Done(completion)=>{let mut patched=tx.task(task.id).await?.unwrap_or(live);patched.status=TaskStatus::Terminal;patched.outcome=Some(match completion{Completion::Completed{result}=>Outcome::Completed{result},Completion::Failed{failure}=>Outcome::Failed{failure}});tx.set_task(patched)?;Ok(None)},
                    Transition::Checkpoint(cp)=>{validate_checkpoint(&kind,&cp)?;tx.checkpoint(cp)?;Ok(tx.task(task.id).await?)},
                }
            }),ctx,CommitOptions{docs:Vec::new(),closing}).await?;
            match result.value{Some(next)=>current=next,None=>return Ok(())}
        }
    }
    async fn fault(&self,task:&Task,error:String)->Result<()>{
        let id=task.id;
        let turn=self.session.kinds.borrow().get(&task.kind).is_some_and(|kind|kind.turn);
        self.session.commit(Invoker::Kernel{conversation_id:None},|tx,_|Box::pin(async move{
            let Some(mut task)=tx.task(id).await?else{return Ok(())};
            if turn{
                let input:JsonObject=serde_json::from_str(&serde_json::to_string(&task.input)?)?;
                if let Some(JsonValue::Array(ids))=input.get("inputs"){
                    let ids:Vec<_>=ids.iter().filter_map(|id|match id{JsonValue::Number(n)=>n.as_u64(),_=>None}).collect();
                    tx.resolve_inputs(&ids,InputResolution::Unanswered{reason:InputReason::Failed,detail:Some(error.clone())}).await?;
                }
                tx.sticky(task.conversation_id)?.set("turn",&JsonValue::object(vec![("tools".into(),JsonValue::array(Vec::new()))]))?;
            }
            task.status=TaskStatus::Terminal;task.outcome=Some(Outcome::Faulted{error});tx.set_task(task)
        }),&self.ctx,CommitOptions{docs:vec![DocRef::Sticky{conversation_id:task.conversation_id}],closing:false}).await.map(|_|())
    }
    fn revoke_run(&self,id:Id){if let Some(inv)=self.invocations.borrow().get(&id) && inv.mode==InvocationMode::Run{for token in inv.tokens.borrow().iter(){token.revoke();}inv.controller.abort(None);}}
    pub async fn abort_task(&self,id:Id,ctx:&Context)->Result<AbortTaskResult>{
        let result=self.session.commit(Invoker::Kernel{conversation_id:None},|tx,_|Box::pin(async move{
            let task=tx.task(id).await?.ok_or_else(||Error::InvalidShape(format!("task {id} not found")))?;
            if task.status==TaskStatus::Terminal{return Ok(AbortTaskResult::Terminal);}tx.mark_task(id)?;Ok(AbortTaskResult::Marked)
        }),ctx,CommitOptions::default()).await?.value;
        if matches!(result,AbortTaskResult::Terminal){return Ok(result);}
        self.revoke_run(id);
        let done=self.invocations.borrow().get(&id).filter(|inv|inv.mode==InvocationMode::Run).map(|inv|inv.done.clone());
        if let Some(mut done)=done{while !*done.borrow_and_update(){done.changed().await.map_err(|_|Error::InvalidShape("invocation join disconnected".into()))?;}}
        self.kick();Ok(result)
    }
    fn next_waiter(&self)->Result<u64>{let id=self.waiter_id.get().checked_add(1).ok_or_else(||Error::InvalidShape("waiter id exhausted".into()))?;self.waiter_id.set(id);Ok(id)}
    pub async fn wait_for_task(&self,id:Id,ctx:&Context)->Result<Task>{
        check_abort(ctx)?;let key=self.next_waiter()?;
        let registration=self.session.on_line(ctx,|line_ctx|Box::pin(async move{
            let retired = !self.session.live_tasks.borrow().contains_key(&id);
            if retired && let Some(task)=self.session.storage.task(id,&line_ctx).await? && task.status==TaskStatus::Terminal{return Ok(Ok(task));}
            let (send,receive)=tokio::sync::oneshot::channel();self.tasks.borrow_mut().entry(id).or_default().insert(key,send);Ok(Err(receive))
        })).await?;
        match registration{Ok(task)=>Ok(task),Err(receive)=>{let _registration=WaitRegistration{table:&self.tasks,id,key};receive_cancel(receive,ctx).await}}
    }
    pub async fn wait_for_input(&self,id:Id,ctx:&Context)->Result<Input>{
        check_abort(ctx)?;let key=self.next_waiter()?;
        let registration=self.session.on_line(ctx,|line_ctx|Box::pin(async move{
            if let Some(input)=self.session.storage.input(id,&line_ctx).await? && matches!(input.status,InputStatus::Done|InputStatus::Unanswered){return Ok(Ok(input));}
            let(send,receive)=tokio::sync::oneshot::channel();self.inputs.borrow_mut().entry(id).or_default().insert(key,send);Ok(Err(receive))
        })).await?;
        match registration{Ok(input)=>Ok(input),Err(receive)=>{let _registration=WaitRegistration{table:&self.inputs,id,key};receive_cancel(receive,ctx).await}}
    }
    fn is_idle(&self,conversation_id:Option<Id>)->bool{!self.session.live_tasks.borrow().values().any(|task|conversation_id.is_none_or(|id|id==task.conversation_id)&&task.background.is_none())}
    pub async fn wait_for_idle(&self,conversation_id:Option<Id>,ctx:&Context)->Result<()>{
        check_abort(ctx)?;let key=self.next_waiter()?;
        let registration=self.session.on_line(ctx,|_|Box::pin(async move{
            if self.is_idle(conversation_id)&&!self.draining.get(){return Ok(None);}
            let(send,receive)=tokio::sync::oneshot::channel();self.idle.borrow_mut().insert(key,(conversation_id,send));Ok(Some(receive))
        })).await?;
        if let Some(receive)=registration{let _registration=IdleRegistration{scheduler:self,key};receive_cancel(receive,ctx).await}else{Ok(())}
    }
    fn check_idle(&self){if self.draining.get(){return;}let ready:Vec<_>=self.idle.borrow().iter().filter(|(_, (id,_))|self.is_idle(*id)).map(|(key,_)|*key).collect();for key in ready{if let Some((_,send))=self.idle.borrow_mut().remove(&key){let _=send.send(());}}}
    pub async fn join_all(&self)->Result<()>{
        self.stop();let mut joins=Vec::new();for inv in self.invocations.borrow().values(){for token in inv.tokens.borrow().iter(){token.revoke();}inv.controller.abort(None);joins.push(inv.done.clone());}
        for mut done in joins{while !*done.borrow_and_update(){done.changed().await.map_err(|_|Error::InvalidShape("invocation join disconnected".into()))?;}}Ok(())
    }
    pub fn quiescent(&self)->bool{self.invocations.borrow().is_empty()}
    pub fn live_invocations(&self)->usize{self.invocations.borrow().len()}
}
fn validate_checkpoint(kind:&ExecutableKind,checkpoint:&Checkpoint)->Result<()>{
    if !kind.phases.contains_key(&checkpoint.phase){return Err(Error::TaskContractFault{kind:kind.name().into(),what:format!("checkpoint names unknown phase {}",checkpoint.phase)});}
    if kind.inflight.contains(&checkpoint.phase){return Err(Error::TaskContractFault{kind:kind.name().into(),what:format!("transition into in-flight phase {}; write with runtime commit before effect",checkpoint.phase)});}Ok(())
}
fn convert<A:serde::Serialize,B:serde::de::DeserializeOwned>(value:&A)->Result<B>{Ok(serde_json::from_str(&serde_json::to_string(value)?)?)}
pub fn executable<K>(kind:Rc<K>,phases:&[String],runtime:TypedRuntimeFactory<K>,access:TxAccess<K>)->Result<Rc<ExecutableKind>>
where K:Kind+'static,K::Input:'static,K::Checkpoint:'static,K::Result:'static,K::Failure:'static,K::Aborted:'static,K::Hooks:'static,K::Slot:'static,K::Tx:'static {
    let make_handler=|phase:Option<String>|->Handler{
        let kind=kind.clone();let runtime=runtime.clone();
        Rc::new(move|task,token,mode,ctx|{
            let kind=kind.clone();let runtime=runtime.clone();let phase=phase.clone();
            Box::pin(async move{
                let typed:Task<K::Input,K::Checkpoint>=convert(task)?;
                let rt=runtime(task.clone(),token,mode,ctx.clone())?;
                let step=match phase{Some(phase)=>kind.phase(&phase,&typed,rt.as_ref(),ctx).await?,None=>kind.initial(&typed,rt.as_ref(),ctx).await?};
                match step{
                    super::types::Step::Next(checkpoint)=>Ok(Step::Next(convert(&checkpoint)?)),
                    super::types::Step::Build(builder)=>Ok(Step::Build(Box::new(move|tx,current,ctx|Box::pin(async move{
                        let current:Task=convert(&current)?;
                        match builder(access(tx),&current,&ctx).await?{
                            super::types::Transition::Checkpoint(checkpoint)=>Ok(Transition::Checkpoint(convert(&checkpoint)?)),
                            super::types::Transition::Done(completion)=>Ok(Transition::Done(convert(&completion)?)),
                            super::types::Transition::Retry=>Ok(Transition::Retry),
                        }
                    })))),
                    super::types::Step::Done(closure)=>Ok(Step::Done(Box::new(move|tx,current,ctx|Box::pin(async move{let current:Task=convert(&current)?;convert(&closure(access(tx),&current,&ctx).await?)})))),
                }
            })
        })
    };
    let initial=make_handler(None);let handlers=phases.iter().map(|phase|(phase.clone(),make_handler(Some(phase.clone())))).collect();
    let abort_kind=kind.clone();let abort_runtime=runtime.clone();
    let abort:AbortHandler=Rc::new(move|task,token,mode,ctx|{
        let kind=abort_kind.clone();let runtime=abort_runtime.clone();
        Box::pin(async move{
            let typed:Task<K::Input,K::Checkpoint>=convert(task)?;let rt=runtime(task.clone(),token,mode,ctx.clone())?;
            let closure=kind.abort(&typed,rt.as_ref(),ctx).await?;
            let erased:AbortClosure=Box::new(move|tx,current,ctx|Box::pin(async move{convert(&closure(access(tx),&current,&ctx).await?)}));
            Ok(erased)
        })
    });
    let slot_kind=kind.clone();let slot=Rc::new(move|input:&JsonValue|->Result<JsonObject>{let input:K::Input=convert(input)?;convert(&slot_kind.slot(&input)?) });
    let describe_kind=kind.clone();let describe=Rc::new(move|task:&Task,slot:Option<&JsonObject>|->Result<JsonValue>{let task:Task<K::Input,K::Checkpoint>=convert(task)?;let slot:Option<K::Slot>=slot.map(convert).transpose()?;describe_kind.describe(&task,slot.as_ref())});
    Ok(Rc::new(ExecutableKind{token:kind.token().clone(),turn:kind.turn(),config:kind.config().clone(),slot:Some(slot),describe:Some(describe),initial,phases:handlers,inflight:kind.inflight().iter().cloned().collect(),abort}))
}
fn remove_waiter<T>(table:&Waiters<T>,id:Id,key:u64){let mut table=table.borrow_mut();if let Some(waiters)=table.get_mut(&id){waiters.remove(&key);if waiters.is_empty(){table.remove(&id);}}}
struct WaitRegistration<'a,T>{table:&'a Waiters<T>,id:Id,key:u64}
impl<T> Drop for WaitRegistration<'_,T>{fn drop(&mut self){remove_waiter(self.table,self.id,self.key);}}
struct IdleRegistration<'a>{scheduler:&'a Scheduler,key:u64}
impl Drop for IdleRegistration<'_>{fn drop(&mut self){self.scheduler.idle.borrow_mut().remove(&self.key);}}
async fn receive_cancel<T>(receive:tokio::sync::oneshot::Receiver<T>,ctx:&Context)->Result<T>{
    match ctx.abort_signal(){Some(signal)=>tokio::select!{biased;()=signal.cancelled()=>Err(Error::Forbidden("wait aborted".into())),value=receive=>value.map_err(|_|Error::Closed)},None=>receive.await.map_err(|_|Error::Closed)}
}

#[cfg(test)]
#[path="scheduler_tests.rs"]
mod tests;
