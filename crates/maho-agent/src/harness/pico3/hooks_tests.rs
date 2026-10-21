use super::*;
use std::cell::{Cell,RefCell};
use crate::harness::context::background_context;
use super::super::types::{define_task,JsonValue};

fn registration(kind:&KindToken,value:u32,conversation_id:Option<Id>,subtree:bool)->HookRegistration<u32> {
    HookRegistration {namespace:Namespace::new("acme"),kind:kind.clone(),handlers:Rc::new(value),conversation_id,subtree}
}
#[tokio::test]
async fn identity_ancestry_namespace_and_live_registration_order() {
    let kind=define_task("work").unwrap(); let unrelated=define_task("work").unwrap();
    let registrations=Rc::new(RefCell::new(vec![registration(&kind,1,None,false),registration(&kind,2,Some(9),true),
        registration(&kind,3,Some(9),false),registration(&kind,4,Some(10),false),registration(&unrelated,5,None,false)]));
    let source=registrations.clone();
    let runners=HookRunners::new(move||source.borrow().clone(),|_|vec![9],|_|panic!("unexpected hook error"));
    let runner=runners.runner(kind.clone(),7,10);
    let bindings=runner.handlers();
    assert_eq!(bindings.iter().map(|b|*b.handlers).collect::<Vec<_>>(),vec![1,2,4]);
    assert_eq!(bindings[0].namespace.id(),"acme");
    assert_eq!(bindings[0].api.kind,"work"); assert_eq!(bindings[0].api.task_id,7);
    registrations.borrow_mut().push(registration(&kind,6,None,false));
    assert_eq!(runner.handlers().len(),4);
    let mut visited=Vec::new();
    runner.each(&background_context(),|h,_|{visited.push(*h);std::future::ready(Ok(Some(*h)))},|v|v==2).await.unwrap();
    assert_eq!(visited,vec![1,2]);
}
#[tokio::test]
async fn undefined_is_skipped_null_is_first_value_and_errors_report() {
    let kind=define_task("work").unwrap(); let token=kind.clone();
    let reports=Rc::new(Cell::new(0)); let count=reports.clone();
    let runners=HookRunners::new(move||(0..4).map(|v|registration(&token,v,None,false)).collect(),|_|vec![],move |_|count.set(count.get()+1));
    let runner=runners.runner(kind,1,2);
    let mut visited=Vec::new();
    let value=runner.first(&background_context(),|h,_| {
        visited.push(*h);
        std::future::ready(match *h {0=>Err(Error::Closed),1=>Ok(None),_=>Ok(Some(JsonValue::Null))})
    }).await.unwrap();
    assert_eq!(value,Some(JsonValue::Null)); assert_eq!(visited,vec![0,1,2]); assert_eq!(reports.get(),1);
}
#[tokio::test]
async fn aborted_context_propagates_handler_error_without_reporting() {
    use crate::harness::context::with_abort_signal;
    use maho_ai::utils::abort::AbortController;
    let kind=define_task("work").unwrap(); let token=kind.clone();
    let reports=Rc::new(Cell::new(0)); let count=reports.clone();
    let runners=HookRunners::new(move||vec![registration(&token,0,None,false)],|_|vec![],move |_|count.set(count.get()+1));
    let controller=AbortController::new();
    let ctx=with_abort_signal(controller.signal(),&background_context()); controller.abort(None);
    let result=runners.runner(kind,1,2).each::<(),_,_>(&ctx,|_,_|std::future::ready(Err(Error::Closed)),|_|false).await;
    assert!(matches!(result,Err(Error::Closed))); assert_eq!(reports.get(),0);
}
