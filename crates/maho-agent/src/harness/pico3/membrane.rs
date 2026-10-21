use std::{cell::{Cell, RefCell}, rc::{Rc, Weak}};
use maho_chord::delta::{Draft, Seg};
use super::types::{Error, JsString, JsonValue, Result};

struct State { alive:Cell<bool>, what:String, wrappers:RefCell<Vec<Weak<Handle>>> }
struct Handle { draft:Draft }
pub struct Membrane { state:Rc<State> }
#[derive(Clone)]
pub struct Document { state:Rc<State>, handle:Rc<Handle> }
#[derive(Clone)]
pub enum PlainInput {
    Json(JsonValue), Wrapper(Document), Array(Vec<PlainInput>),
    Object(Vec<(JsString,PlainInput)>), Shared(Rc<RefCell<PlainInput>>),
}
impl Membrane {
    pub fn new(what:&str)->Self {
        Self {state:Rc::new(State {alive:Cell::new(true),what:what.into(),wrappers:RefCell::new(Vec::new())})}
    }
    pub fn revoke(&self) { self.state.alive.set(false); }
    pub fn wrap(&self,draft:Draft)->Result<Document> { wrap(&self.state,draft) }
}
impl Drop for Membrane { fn drop(&mut self) { self.revoke(); } }
fn check(state:&State)->Result<()> {
    if state.alive.get() { Ok(()) } else { Err(Error::Revoked(state.what.clone())) }
}
fn wrap(state:&Rc<State>,draft:Draft)->Result<Document> {
    check(state)?;
    let mut wrappers=state.wrappers.borrow_mut();
    wrappers.retain(|w|w.strong_count()>0);
    for handle in wrappers.iter().filter_map(Weak::upgrade) {
        if handle.draft.same_identity(&draft) {
            return Ok(Document {state:state.clone(),handle});
        }
    }
    let handle=Rc::new(Handle {draft});
    wrappers.push(Rc::downgrade(&handle));
    Ok(Document {state:state.clone(),handle})
}
impl Document {
    pub fn same_identity(&self,other:&Self)->Result<bool> {
        check(&self.state)?; check(&other.state)?;
        Ok(Rc::ptr_eq(&self.state,&other.state)&&Rc::ptr_eq(&self.handle,&other.handle))
    }
    pub fn value(&self)->Result<JsonValue> { check(&self.state)?; Ok(self.handle.draft.value()?) }
    pub fn get(&self,key:impl Into<Seg>)->Result<Option<Self>> {
        check(&self.state)?;
        self.handle.draft.get(key)?.map(|d|wrap(&self.state,d)).transpose()
    }
    pub fn at(&self,path:&[Seg])->Result<Self> {
        check(&self.state)?; wrap(&self.state,self.handle.draft.at(path)?)
    }
    pub fn has(&self,key:impl Into<Seg>)->Result<bool> { Ok(self.get(key)?.is_some()) }
    pub fn keys(&self)->Result<Vec<JsString>> { check(&self.state)?; Ok(self.handle.draft.keys()?) }
    pub fn len(&self)->Result<usize> { check(&self.state)?; Ok(self.handle.draft.len()?) }
    pub fn is_empty(&self)->Result<bool> { Ok(self.len()?==0) }
    pub fn set(&self,key:impl Into<Seg>,value:&JsonValue)->Result<()> {
        check(&self.state)?;
        let key=key.into();
        if matches!(&key,Seg::Key(k) if k.as_str()==Some("__proto__")) {
            return self.reject("change prototypes");
        }
        Ok(self.handle.draft.set(key,value)?)
    }
    pub fn set_input(&self,key:impl Into<Seg>,value:&PlainInput)->Result<()> {
        check(&self.state)?;
        let value=self.plain(value,&mut Vec::new())?;
        self.set(key,&value)
    }
    pub fn delete(&self,key:impl Into<Seg>)->Result<()> { check(&self.state)?; Ok(self.handle.draft.delete(key)?) }
    pub fn splice(&self,start:isize,remove:usize,items:&[JsonValue])->Result<Vec<Self>> {
        check(&self.state)?;
        self.handle.draft.splice(start,remove,items)?.into_iter().map(|d|wrap(&self.state,d)).collect()
    }
    pub fn splice_input(&self,start:isize,remove:usize,items:&[PlainInput])->Result<Vec<Self>> {
        check(&self.state)?;
        let items:Vec<_>=items.iter().map(|v|self.plain(v,&mut Vec::new())).collect::<Result<_>>()?;
        self.splice(start,remove,&items)
    }
    pub fn push(&self,items:&[JsonValue])->Result<usize> { check(&self.state)?; Ok(self.handle.draft.push(items)?) }
    pub fn pop(&self)->Result<Option<Self>> {
        check(&self.state)?; self.handle.draft.pop()?.map(|d|wrap(&self.state,d)).transpose()
    }
    pub fn shift(&self)->Result<Option<Self>> {
        check(&self.state)?; self.handle.draft.shift()?.map(|d|wrap(&self.state,d)).transpose()
    }
    pub fn unshift(&self,items:&[JsonValue])->Result<usize> { check(&self.state)?; Ok(self.handle.draft.unshift(items)?) }
    pub fn set_length(&self,length:usize)->Result<()> { check(&self.state)?; Ok(self.handle.draft.set_length(length)?) }
    pub fn reverse(&self)->Result<()> { check(&self.state)?; Ok(self.handle.draft.reverse()?) }
    pub fn sort(&self)->Result<()> { check(&self.state)?; Ok(self.handle.draft.sort()?) }
    pub fn fill(&self,value:&JsonValue,start:isize,end:Option<isize>)->Result<()> {
        check(&self.state)?; Ok(self.handle.draft.fill(value,start,end)?)
    }
    pub fn copy_within(&self,target:isize,start:isize,end:Option<isize>)->Result<()> {
        check(&self.state)?; Ok(self.handle.draft.copy_within(target,start,end)?)
    }
    /// Callbacks receive only revocable handles, including the array receiver.
    pub fn for_each(&self,mut callback:impl FnMut(Self,usize,Self)->Result<()>)->Result<()> {
        let length=self.len()?;
        for index in 0..length {
            check(&self.state)?;
            if let Some(item)=self.get(index)? { callback(item,index,self.clone())?; }
        }
        Ok(())
    }
    pub fn define_value(&self,key:impl Into<Seg>,value:&PlainInput)->Result<()> { self.set_input(key,value) }
    pub fn define_accessor(&self)->Result<()> { self.reject("define accessors") }
    pub fn set_prototype(&self)->Result<()> { self.reject("change prototypes") }
    pub fn prevent_extensions(&self)->Result<()> { self.reject("change extensibility") }
    pub fn is_extensible(&self)->Result<bool> { check(&self.state)?; Ok(true) }
    fn reject<T>(&self,operation:&'static str)->Result<T> {
        check(&self.state)?;
        Err(Error::DocumentOperation {what:self.state.what.clone(),operation})
    }
    fn plain(&self,input:&PlainInput,seen:&mut Vec<Rc<RefCell<PlainInput>>>)->Result<JsonValue> {
        match input {
            PlainInput::Json(v)=>Ok(v.clone()),
            PlainInput::Wrapper(_)=>Err(Error::WrapperAssignment(self.state.what.clone())),
            PlainInput::Array(items)=>Ok(JsonValue::array(items.iter().map(|v|self.plain(v,seen)).collect::<Result<_>>()?)),
            PlainInput::Object(items)=>Ok(JsonValue::object(items.iter().map(|(k,v)|Ok((k.clone(),self.plain(v,seen)?))).collect::<Result<_>>()?)),
            PlainInput::Shared(node)=> {
                if seen.iter().any(|old|Rc::ptr_eq(old,node)) { return Err(Error::CyclicAssignment(self.state.what.clone())); }
                seen.push(node.clone());
                let value=node.try_borrow().map_err(|_|Error::InvalidShape("input graph is mutably borrowed".into()))?;
                let result=self.plain(&value,seen);
                seen.pop();
                result
            }
        }
    }
}

#[cfg(test)]
#[path="membrane_tests.rs"]
mod tests;
