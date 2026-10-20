use std::rc::Rc;
use super::types::{Context, Error, HookApi, Id, KindToken, Namespace, Result};

pub struct HookRegistration<H> {
    pub namespace:Namespace, pub kind:KindToken, pub handlers:Rc<H>,
    pub conversation_id:Option<Id>, pub subtree:bool,
}
impl<H> Clone for HookRegistration<H> {
    fn clone(&self)->Self { Self {namespace:self.namespace.clone(),kind:self.kind.clone(),handlers:self.handlers.clone(),
        conversation_id:self.conversation_id,subtree:self.subtree} }
}
pub struct HookBinding<H> { pub handlers:Rc<H>, pub namespace:Namespace, pub api:HookApi }
pub struct HookRunners<H> {
    registrations:Rc<dyn Fn()->Vec<HookRegistration<H>>>,
    ancestors:Rc<dyn Fn(Id)->Vec<Id>>, on_report:Rc<dyn Fn(Error)>,
}
impl<H> Clone for HookRunners<H> {
    fn clone(&self)->Self { Self {registrations:self.registrations.clone(),ancestors:self.ancestors.clone(),on_report:self.on_report.clone()} }
}
pub struct HookRunner<H> { factory:HookRunners<H>, kind:KindToken, task_id:Id, conversation_id:Id }
impl<H> HookRunners<H> {
    pub fn new(registrations:impl Fn()->Vec<HookRegistration<H>>+'static,
        ancestors:impl Fn(Id)->Vec<Id>+'static,on_report:impl Fn(Error)+'static)->Self {
        Self {registrations:Rc::new(registrations),ancestors:Rc::new(ancestors),on_report:Rc::new(on_report)}
    }
    pub fn runner(&self,kind:KindToken,task_id:Id,conversation_id:Id)->HookRunner<H> {
        HookRunner {factory:self.clone(),kind,task_id,conversation_id}
    }
}
impl<H> HookRunner<H> {
    pub fn handlers(&self)->Vec<HookBinding<H>> {
        (self.factory.registrations)().into_iter()
            .filter(|r|r.kind.same_identity(&self.kind))
            .filter(|r|match r.conversation_id {
                None=>true,
                Some(id)=>id==self.conversation_id || (r.subtree && (self.factory.ancestors)(self.conversation_id).contains(&id)),
            })
            .map(|r|HookBinding {handlers:r.handlers,namespace:r.namespace,
                api:HookApi {kind:self.kind.name().into(),task_id:self.task_id,conversation_id:self.conversation_id}}).collect()
    }
    pub async fn each<V,F,Fut>(&self,ctx:&Context,mut callback:F,mut on_value:impl FnMut(V)->bool)->Result<()>
    where F:FnMut(Rc<H>,HookApi)->Fut, Fut:std::future::Future<Output=Result<Option<V>>> {
        for binding in self.handlers() {
            let result=callback(binding.handlers,binding.api).await;
            let result=match result {
                Ok(Some(value))=>if on_value(value) { return Ok(()); } else { Ok(()) },
                Ok(None)=>Ok(()),
                Err(error)=>Err(error),
            };
            if let Err(error)=result {
                if ctx.is_aborted() { return Err(error); }
                (self.factory.on_report)(error);
            }
        }
        Ok(())
    }
    pub async fn first<V,F,Fut>(&self,ctx:&Context,callback:F)->Result<Option<V>>
    where F:FnMut(Rc<H>,HookApi)->Fut, Fut:std::future::Future<Output=Result<Option<V>>> {
        let mut first=None;
        self.each(ctx,callback,|v| {first=Some(v);true}).await?;
        Ok(first)
    }
}

#[cfg(test)]
#[path="hooks_tests.rs"]
mod tests;
