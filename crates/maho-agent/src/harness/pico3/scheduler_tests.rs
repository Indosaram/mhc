use super::*;
use crate::harness::context::{background_context,with_abort_signal};
use super::super::{memory::MemoryStorage,session::{Kinds,Namespaces}};

fn session()->Result<Rc<Session>>{
    let kinds:Kinds=Rc::new(RefCell::new(BTreeMap::new()));let namespaces:Namespaces=Rc::new(RefCell::new(BTreeMap::new()));
    Session::new(Rc::new(MemoryStorage::new()),kinds,namespaces,Rc::new(||1))
}
#[tokio::test(flavor="current_thread")]
async fn cancellation_removes_only_waiter_not_durable_input()->Result<()>{
    let session=session()?;let ctx=background_context();let id=session.storage.mint_id()?;
    session.storage.commit(&[Write::Input{input:Input{id,conversation_id:1,request_id:None,status:InputStatus::Queued,entry:None,answer:None,reason:None,detail:None}}],&ctx).await?;
    let scheduler=Scheduler::new(session.clone(),ctx.clone());let controller=AbortController::new();let wait_ctx=with_abort_signal(controller.signal(),&ctx);
    let wait=scheduler.wait_for_input(id,&wait_ctx);tokio::pin!(wait);
    assert!(matches!(futures::poll!(&mut wait),std::task::Poll::Pending));
    assert_eq!(scheduler.inputs.borrow().get(&id).map(BTreeMap::len),Some(1));controller.abort(None);
    assert!(matches!(tokio::time::timeout(std::time::Duration::from_secs(2),wait).await.map_err(|_|Error::InvalidShape("wait cancellation timed out".into()))?,Err(Error::Forbidden(_))));assert!(scheduler.inputs.borrow().is_empty());
    assert_eq!(session.storage.input(id,&ctx).await?.map(|i|i.status),Some(InputStatus::Queued));Ok(())
}
#[tokio::test(flavor="current_thread")]
async fn registered_waiter_receives_exact_commit_and_late_wait_is_immediate()->Result<()>{
    let session=session()?;let ctx=background_context();let id=session.storage.mint_id()?;
    session.storage.commit(&[Write::Input{input:Input{id,conversation_id:1,request_id:None,status:InputStatus::Queued,entry:None,answer:None,reason:None,detail:None}}],&ctx).await?;
    let scheduler=Scheduler::new(session.clone(),ctx.clone());let wait=scheduler.wait_for_input(id,&ctx);tokio::pin!(wait);
    assert!(matches!(futures::poll!(&mut wait),std::task::Poll::Pending));
    session.commit(Invoker::Kernel{conversation_id:None},|tx,_|Box::pin(async move{tx.resolve_inputs(&[id],InputResolution::Done{answer:99}).await}),&ctx,CommitOptions::default()).await?;
    assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(2),wait).await.map_err(|_|Error::InvalidShape("wait completion timed out".into()))??.answer,Some(99));assert_eq!(scheduler.wait_for_input(id,&ctx).await?.answer,Some(99));Ok(())
}
#[test]
fn lease_drop_revokes_all_captured_token_clones(){
    let token=InvocationToken::new(1,InvocationMode::Run);let captured=token.clone();{let _lease=Lease{token};assert!(captured.alive());}assert!(!captured.alive());
}
#[tokio::test(flavor="current_thread")]
async fn idle_ignores_background_tasks_but_not_pending_foreground()->Result<()>{
    let session=session()?;let scheduler=Scheduler::new(session.clone(),background_context());
    let task=Task{id:2,conversation_id:1,kind:"test".into(),input:JsonValue::Null,status:TaskStatus::Pending,checkpoint:None,abort:None,outcome:None,after:vec![900],owns:Vec::new(),background:Some(True)};
    session.live_tasks.borrow_mut().insert(task.id,task.clone());assert!(scheduler.is_idle(None));
    session.live_tasks.borrow_mut().insert(task.id,Task{background:None,..task});assert!(!scheduler.is_idle(None));assert!(scheduler.is_idle(Some(3)));Ok(())
}
#[tokio::test(flavor="current_thread")]
async fn dependencies_gate_dispatch_and_abort_revokes_before_join()->Result<()>{
    tokio::task::LocalSet::new().run_until(async{
        let session=session()?;let ctx=background_context();
        let(arrived,arrival)=tokio::sync::oneshot::channel();let arrived=Rc::new(RefCell::new(Some(arrived)));
        let captured=Rc::new(RefCell::new(None::<InvocationToken>));let run_capture=captured.clone();
        let handler:Handler=Rc::new(move|_,token,_,ctx|{
            let arrived=arrived.clone();let captured=run_capture.clone();
            Box::pin(async move{
                *captured.borrow_mut()=Some(token.clone());if let Some(send)=arrived.borrow_mut().take(){let _=send.send(());}
                let signal=ctx.abort_signal().ok_or_else(||Error::InvalidShape("missing invocation signal".into()))?;signal.cancelled().await;
                assert!(!token.alive());Err(Error::Forbidden("cancelled run".into()))
            })
        });
        let aborted=Rc::new(Cell::new(false));let abort_observed=aborted.clone();
        let abort:AbortHandler=Rc::new(move|_,token,mode,_|{
            let aborted=abort_observed.clone();
            Box::pin(async move{assert_eq!(mode,InvocationMode::Abort);assert!(token.alive());aborted.set(true);let closure:AbortClosure=Box::new(|_,_,_|Box::pin(async{Ok(JsonValue::Null)}));Ok(closure)})
        });
        session.register_kind(Rc::new(ExecutableKind{token:define_task("gated")?,turn:false,config:KindConfig::default(),slot:None,describe:None,initial:handler,phases:BTreeMap::new(),inflight:Default::default(),abort}))?;
        let root=session.commit(Invoker::Kernel{conversation_id:None},|tx,_|Box::pin(async{tx.create_conversation(ConversationSpec::default())}),&ctx,CommitOptions::default()).await?.value;
        let task=Task{id:100,conversation_id:root,kind:"gated".into(),input:JsonValue::Null,status:TaskStatus::Pending,checkpoint:None,abort:None,outcome:None,after:vec![99],owns:Vec::new(),background:None};
        session.storage.commit(&[Write::Task{task:Task{id:99,kind:"unregistered".into(),after:Vec::new(),..task.clone()}},Write::Task{task:task.clone()}],&ctx).await?;session.load(&ctx).await?;
        let scheduler=Scheduler::new(session.clone(),ctx.clone());scheduler.enabled.set(true);scheduler.dirty.set(true);scheduler.draining.set(true);scheduler.drain().await?;scheduler.draining.set(false);
        assert!(scheduler.quiescent());assert_eq!(session.live_tasks.borrow().get(&100).map(|task|task.status),Some(TaskStatus::Pending));
        session.commit(Invoker::Kernel{conversation_id:None},|tx,_|Box::pin(async move{let mut dependency=tx.task(99).await?.ok_or_else(||Error::InvalidShape("dependency missing".into()))?;dependency.status=TaskStatus::Terminal;dependency.outcome=Some(Outcome::Orphaned);tx.set_task(dependency)}),&ctx,CommitOptions::default()).await?;
        tokio::time::timeout(std::time::Duration::from_secs(2),arrival).await.map_err(|_|Error::InvalidShape("handler not dispatched after dependency".into()))?.map_err(|_|Error::Closed)?;
        assert!(captured.borrow().as_ref().is_some_and(InvocationToken::alive));
        let result=tokio::time::timeout(std::time::Duration::from_secs(2),scheduler.abort_task(100,&ctx)).await.map_err(|_|Error::InvalidShape("abort join timed out".into()))??;assert!(matches!(result,AbortTaskResult::Marked));
        assert!(captured.borrow().as_ref().is_some_and(|token|!token.alive()));
        let task=tokio::time::timeout(std::time::Duration::from_secs(2),scheduler.wait_for_task(100,&ctx)).await.map_err(|_|Error::InvalidShape("abort closure did not settle".into()))??;
        assert!(matches!(task.outcome,Some(Outcome::Aborted{..})));assert!(aborted.get());scheduler.join_all().await?;Ok(())
    }).await
}
