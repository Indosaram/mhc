//! Native contracts corresponding to upstream `pico3/types.ts`.
//! Durable extension values retain Chord's lossless UTF-16 JSON representation.
use std::{cell::Cell, collections::BTreeMap, future::Future, marker::PhantomData, pin::Pin, rc::Rc};
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize, Serializer};
pub use maho_chord::types::{JsonValue, JsString};
pub use crate::harness::context::Context;
pub use maho_ai::types::{AssistantMessage, AssistantMessageEvent, DeferredHandle, ToolCall, ToolResultMessage, UserMessage};
pub use maho_ai::types::ModelThinkingLevel as ThinkingLevel;
pub use maho_ai::model::Model;
use maho_chord::delta::Op;
use super::membrane::Document;

pub type Id = u64;
pub type Seq = u64;
pub type LocalBoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("storage: {0}")] Storage(String),
    #[error("forbidden: {0}")] Forbidden(String),
    #[error("{read} after {write} in the same transaction: the answer would not include the buffered write")]
    ReadAfterWrite { read: String, write: String },
    #[error("conversation {0} is busy")] ConversationBusy(Id),
    #[error("conversation {0} already has a live generation")] GenerationInProgress(Id),
    #[error("conversation {0} already has a live collapse")] CollapseInProgress(Id),
    #[error("Session faulted: {0}")] Faulted(String),
    #[error("Session is closed")] Closed,
    #[error("kind {kind} broke its contract: {what}")] TaskContractFault { kind: String, what: String },
    #[error("{category} kind names beginning with \"pi.\" are reserved: {name}")]
    ReservedName { category: &'static str, name: String },
    #[error("document proxy ({0}) used outside its transaction")] Revoked(String),
    #[error("assigning a document proxy into a document ({0}); assign a plain value")] WrapperAssignment(String),
    #[error("assigning a cyclic value into a document ({0})")] CyclicAssignment(String),
    #[error("document proxy ({what}) cannot {operation}")] DocumentOperation { what: String, operation: &'static str },
    #[error("invalid durable shape: {0}")] InvalidShape(String),
    #[error(transparent)] Json(#[from] serde_json::Error),
    #[error(transparent)] Delta(#[from] maho_chord::delta::DeltaError),
    #[error(transparent)] Chord(#[from] maho_chord::types::ServiceError),
}

/// Ordered object, encoded as a JSON object rather than a vector of pairs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JsonObject(maho_chord::types::JsonObject);
impl JsonObject {
    pub const fn new() -> Self { Self(Vec::new()) }
    pub fn get(&self, key: &str) -> Option<&JsonValue> { self.get_key(&key.into()) }
    pub fn get_key(&self, key: &JsString) -> Option<&JsonValue> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    pub fn insert(&mut self, key: JsString, value: JsonValue) -> Option<JsonValue> {
        if let Some((_, old)) = self.0.iter_mut().find(|(k, _)| k == &key) {
            return Some(std::mem::replace(old, value));
        }
        self.0.push((key, value));
        None
    }
    pub fn remove(&mut self, key: &str) -> Option<JsonValue> {
        let key = JsString::from(key);
        self.0.iter().position(|(k, _)| k == &key).map(|i| self.0.remove(i).1)
    }
    pub fn iter(&self) -> impl Iterator<Item = &(JsString, JsonValue)> { self.0.iter() }
    pub fn into_inner(self) -> maho_chord::types::JsonObject { self.0 }
}
impl From<maho_chord::types::JsonObject> for JsonObject {
    fn from(value: maho_chord::types::JsonObject) -> Self {
        match JsonValue::object(value) { JsonValue::Object(v) => Self((*v).clone()), _ => unreachable!() }
    }
}
impl Serialize for JsonObject {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok,S::Error> {
        JsonValue::object(self.0.clone()).serialize(s)
    }
}
impl<'de> Deserialize<'de> for JsonObject {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self,D::Error> {
        match JsonValue::deserialize(d)? {
            JsonValue::Object(v) => Ok(Self((*v).clone())),
            _ => Err(serde::de::Error::custom("expected JSON object")),
        }
    }
}

/// Serialized representation retains type information without coercing lossless JSON.
#[derive(Debug, PartialEq)]
pub struct Stored<T> { value: JsonValue, marker: PhantomData<fn() -> T> }
impl<T> Clone for Stored<T> {
    fn clone(&self) -> Self { Self { value: self.value.clone(), marker: PhantomData } }
}
impl<T> Stored<T> {
    pub fn from_json(value: JsonValue) -> Self { Self { value, marker: PhantomData } }
    pub fn as_json(&self) -> &JsonValue { &self.value }
    pub fn value(&self) -> &JsonValue { &self.value }
    pub fn decode(&self) -> Result<T> where T: DeserializeOwned {
        Ok(serde_json::from_str(&serde_json::to_string(&self.value)?)?)
    }
}
impl<T> Serialize for Stored<T> {
    fn serialize<S: Serializer>(&self,s:S)->std::result::Result<S::Ok,S::Error> { self.value.serialize(s) }
}
impl<'de,T> Deserialize<'de> for Stored<T> {
    fn deserialize<D:Deserializer<'de>>(d:D)->std::result::Result<Self,D::Error> {
        Ok(Self { value: JsonValue::deserialize(d)?, marker: PhantomData })
    }
}
pub fn to_stored<T: Serialize>(value: &T) -> Result<Stored<T>> {
    Ok(Stored { value: serde_json::from_str(&serde_json::to_string(value)?)?, marker: PhantomData })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ConversationParent { pub conversation_id: Id, pub at: Id }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SectionSeed { Set { key: String, value: JsonValue }, Remove { key: String, remove: True } }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Conversation {
    pub id: Id,
    #[serde(default,skip_serializing_if="Option::is_none")] pub parent: Option<ConversationParent>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub owner: Option<Id>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub sections: Option<Vec<SectionSeed>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct SystemTool { pub name: String, pub description: String, pub parameters: JsonObject }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolReference { pub name: String }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct SystemMessage {
    pub content: String,
    #[serde(default,skip_serializing_if="Option::is_none")] pub tools_added: Option<Vec<SystemTool>>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub tools_removed: Option<Vec<ToolReference>>,
    pub timestamp: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag="role")]
pub enum Message {
    #[serde(rename="system")] System(SystemMessage),
    #[serde(rename="user")] User(UserMessage),
    #[serde(rename="assistant")] Assistant(Box<AssistantMessage>),
    #[serde(rename="toolResult")] ToolResult(ToolResultMessage),
}
pub type StoredMessage = Stored<Message>;
pub type RequestMessage = StoredMessage;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag="action",rename_all="lowercase")]
pub enum ContextEdit {
    Omit { target: Id },
    Replace { target: Id, #[serde(default,skip_serializing_if="Option::is_none")] messages: Option<Vec<StoredMessage>> },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Entry {
    pub id: Id, pub conversation_id: Id, pub kind: String,
    #[serde(default,skip_serializing_if="Option::is_none")] pub model: Option<Vec<StoredMessage>>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub data: Option<JsonObject>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub head: Option<Id>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub edits: Option<Vec<ContextEdit>>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub by_task_id: Option<Id>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EntryHead { Id(Id), Own(SelfHead) }
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum SelfHead { #[serde(rename="self")] Own }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct NewEntry {
    pub kind: String,
    #[serde(default,skip_serializing_if="Option::is_none")] pub model: Option<Vec<StoredMessage>>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub data: Option<JsonObject>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub head: Option<EntryHead>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub edits: Option<Vec<ContextEdit>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Checkpoint { pub phase: String, #[serde(flatten)] pub fields: JsonObject }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag="status",rename_all="lowercase")]
pub enum Completion<R=JsonValue,F=JsonValue> { Completed { result:R }, Failed { failure:F } }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag="status",rename_all="lowercase")]
pub enum Outcome<R=JsonValue,F=JsonValue,A=JsonValue> {
    Completed { result:R }, Failed { failure:F }, Aborted { result:A }, Orphaned, Faulted { error:String },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="lowercase")]
pub enum TaskStatus { Pending, Running, Terminal }
/// Only the literal `true` is admitted for upstream optional true markers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct True;
impl Serialize for True {
    fn serialize<S:Serializer>(&self,s:S)->std::result::Result<S::Ok,S::Error>{s.serialize_bool(true)}
}
impl<'de> Deserialize<'de> for True {
    fn deserialize<D:Deserializer<'de>>(d:D)->std::result::Result<Self,D::Error>{
        if bool::deserialize(d)? { Ok(Self) } else { Err(serde::de::Error::custom("expected true")) }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Task<I=JsonValue,C=Checkpoint> {
    pub id: Id, pub conversation_id: Id, pub kind: String, pub input:I, pub status:TaskStatus,
    #[serde(default,skip_serializing_if="Option::is_none")] pub checkpoint:Option<C>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub abort:Option<True>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub outcome:Option<Outcome>,
    pub after:Vec<Id>, pub owns:Vec<Id>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub background:Option<True>,
}
fn nullable_patch<'de,D:Deserializer<'de>,T:Deserialize<'de>>(d:D)->std::result::Result<Option<Option<T>>,D::Error>{
    Option::<T>::deserialize(d).map(Some)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskPatch {
    pub id: Id,
    #[serde(default,skip_serializing_if="Option::is_none")] pub status:Option<TaskStatus>,
    #[serde(default,deserialize_with="nullable_patch",skip_serializing_if="Option::is_none")] pub checkpoint:Option<Option<Checkpoint>>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub abort:Option<True>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub outcome:Option<Outcome>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub owns:Option<Vec<Id>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="lowercase")]
pub enum InputStatus { Queued, Placed, Done, Unanswered }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="lowercase")]
pub enum InputReason { Aborted, Stale, Terminated, Failed }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Input {
    pub id:Id, pub conversation_id:Id,
    #[serde(default,skip_serializing_if="Option::is_none")] pub request_id:Option<String>,
    pub status:InputStatus,
    #[serde(default,skip_serializing_if="Option::is_none")] pub entry:Option<Id>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub answer:Option<Id>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub reason:Option<InputReason>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub detail:Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ModelRef { pub provider:String, pub model_id:String }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct RetryPolicy {
    pub enabled:bool, pub max_retries:u32, pub base_delay_ms:u64,
    #[serde(default,skip_serializing_if="Option::is_none")] pub max_agent_delay_ms:Option<u64>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all="lowercase")]
pub enum ToolStatus { Pending, Running, Done, Error, Aborted }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ToolSlot {
    pub call_id:String, pub name:String, pub args:JsonValue, pub status:ToolStatus,
    #[serde(default,skip_serializing_if="Option::is_none")] pub waiting_on:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub output:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub progress:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub details:Option<JsonValue>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub continued_by:Option<Id>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub entry:Option<Id>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub memos:Option<JsonObject>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TurnState {
    #[serde(default,skip_serializing_if="Option::is_none")] pub message:Option<Stored<AssistantMessage>>,
    pub tools:Vec<ToolSlot>,
}
pub type PluginSlices = BTreeMap<String,JsonObject>;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct RewindableState {
    #[serde(default,skip_serializing_if="Option::is_none")] pub model:Option<ModelRef>,
    pub thinking_level:ThinkingLevel, pub selected_tools:Vec<String>, pub profile:String,
    pub threshold:f64, pub keep_recent:u64, pub plugins:PluginSlices,
    #[serde(flatten)] pub fields:JsonObject,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum QueueMode { #[serde(rename="all")] All, #[serde(rename="one-at-a-time")] OneAtATime }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct StickyState {
    pub retry:RetryPolicy, pub steering_mode:QueueMode, pub follow_up_mode:QueueMode,
    pub inbox:Vec<QueuedInput>, pub turn:TurnState, pub tasks:BTreeMap<String,JsonObject>, pub plugins:PluginSlices,
    #[serde(flatten)] pub fields:JsonObject,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionState { pub plugins:PluginSlices, #[serde(flatten)] pub fields:JsonObject }
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub enum InputMode { Steer, FollowUp }
pub type UserInput = maho_ai::types::UserContent;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag="mode")]
pub enum QueuedInput {
    #[serde(rename="steer")] Steer { id:Id, input:Stored<UserInput> },
    #[serde(rename="followUp")] FollowUp { id:Id, input:Stored<UserInput> },
    #[serde(rename="write")] Write { id:Id, entry:Stored<NewEntry> },
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag="doc",rename_all="lowercase")]
pub enum DocRef {
    Session,
    Rewindable { #[serde(rename="conversationId")] conversation_id:Id },
    Sticky { #[serde(rename="conversationId")] conversation_id:Id },
}
#[derive(Clone, Debug, Default)]
pub struct NamespaceDefaults {
    pub rewindable:JsonObject, pub sticky:JsonObject, pub session:JsonObject,
}
struct NamespaceIdentity { id:String, registered:Cell<bool> }
pub struct Namespace<T=JsonObject> { identity:Rc<NamespaceIdentity>, marker:PhantomData<fn(T)->T> }
impl<T> Clone for Namespace<T> {
    fn clone(&self)->Self { Self { identity:self.identity.clone(), marker:PhantomData } }
}
impl<T> Namespace<T> {
    pub(crate) fn new(id:&str)->Self { Self { identity:Rc::new(NamespaceIdentity{id:id.into(),registered:Cell::new(true)}),marker:PhantomData } }
    pub fn id(&self)->&str { &self.identity.id }
    pub fn unregister(&self) { self.identity.registered.set(false); }
    pub fn is_registered(&self)->bool { self.identity.registered.get() }
    pub fn same_identity<U>(&self,other:&Namespace<U>)->bool { Rc::ptr_eq(&self.identity,&other.identity) }
}
pub struct NamespaceRegistration {
    pub token:Namespace, pub defaults:NamespaceDefaults, pub routes:BTreeMap<String,DocKind>,
    pub project:Option<Rc<dyn Fn(&JsonObject)->JsonValue>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocKind { Session, Rewindable, Sticky }
pub fn memo_once(slot:&mut JsonObject,key:&str,candidate:JsonValue)->Result<JsonValue> {
    let mut memos = match slot.get("memos") {
        None => JsonObject::new(),
        Some(JsonValue::Object(v)) => JsonObject::from((**v).clone()),
        Some(_) => return Err(Error::InvalidShape("memos must be an object".into())),
    };
    if let Some(winner)=memos.get(key) { return Ok(winner.clone()); }
    memos.insert(key.into(),candidate.clone());
    slot.insert("memos".into(),JsonValue::object(memos.into_inner()));
    Ok(candidate)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag="type")]
pub enum Write {
    #[serde(rename="conversation")] Conversation { conversation:Conversation },
    #[serde(rename="entry")] Entry { entry:Entry },
    #[serde(rename="task")] Task { task:Task },
    #[serde(rename="task.patch")] TaskPatch { patch:TaskPatch },
    #[serde(rename="input")] Input { input:Input },
    #[serde(rename="doc")] Doc { r#ref:DocRef, ops:Vec<Op> },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct EntryScan {
    pub conversation_id:Id,
    #[serde(default,skip_serializing_if="Option::is_none")] pub kind:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub with_head:Option<bool>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub before:Option<Id>,
    pub limit:usize,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct TaskScan {
    #[serde(default,skip_serializing_if="Option::is_none")] pub conversation_id:Option<Id>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub status:Option<Vec<TaskStatus>>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub kind:Option<String>,
}
pub trait Storage {
    fn commit<'a>(&'a self,writes:&'a [Write],ctx:&'a Context)->LocalBoxFuture<'a,Result<Seq>>;
    fn mint_id(&self)->Result<Id>;
    fn conversation<'a>(&'a self,id:Id,ctx:&'a Context)->LocalBoxFuture<'a,Result<Option<Conversation>>>;
    fn conversations<'a>(&'a self,ctx:&'a Context)->LocalBoxFuture<'a,Result<Vec<Conversation>>>;
    fn entries<'a>(&'a self,ids:&'a [Id],ctx:&'a Context)->LocalBoxFuture<'a,Result<BTreeMap<Id,Entry>>>;
    fn scan_entries<'a>(&'a self,scan:EntryScan,ctx:&'a Context)->LocalBoxFuture<'a,Result<Vec<Entry>>>;
    fn task<'a>(&'a self,id:Id,ctx:&'a Context)->LocalBoxFuture<'a,Result<Option<Task>>>;
    fn scan_tasks<'a>(&'a self,scan:TaskScan,ctx:&'a Context)->LocalBoxFuture<'a,Result<Vec<Task>>>;
    fn input<'a>(&'a self,id:Id,ctx:&'a Context)->LocalBoxFuture<'a,Result<Option<Input>>>;
    fn input_by_request<'a>(&'a self,conversation_id:Id,request_id:&'a str,ctx:&'a Context)->LocalBoxFuture<'a,Result<Option<Input>>>;
    fn doc<'a>(&'a self,r#ref:DocRef,ctx:&'a Context)->LocalBoxFuture<'a,Result<Option<JsonObject>>>;
    fn doc_as_of<'a>(&'a self,conversation_id:Id,at:Id,ctx:&'a Context)->LocalBoxFuture<'a,Result<Option<JsonObject>>>;
    fn truncate<'a>(&'a self,r#ref:DocRef,ctx:&'a Context)->LocalBoxFuture<'a,Result<()>>;
    fn close<'a>(&'a self,ctx:&'a Context)->LocalBoxFuture<'a,Result<()>>;
}

#[derive(Clone, Debug)]
pub struct KindToken { name:Rc<str>, identity:Rc<()> }
impl KindToken {
    pub fn name(&self)->&str { &self.name }
    pub fn same_identity(&self,other:&Self)->bool { Rc::ptr_eq(&self.identity,&other.identity) }
    pub(crate) fn core(name:&str)->Self { Self { name:Rc::from(name),identity:Rc::new(()) } }
}
pub fn define_task(name:&str)->Result<KindToken> {
    if name.starts_with("pi.") { return Err(Error::ReservedName { category:"task",name:name.into() }); }
    Ok(KindToken::core(name))
}
pub type AnyKind = KindToken;
#[derive(Clone, Debug)]
pub struct EntryKind<E=Entry> { kind:String, marker:PhantomData<fn()->E> }
impl<E> EntryKind<E> {
    pub fn kind(&self)->&str { &self.kind }
    pub fn is(&self,entry:Option<&Entry>)->bool { entry.is_some_and(|e| e.kind == self.kind) }
    pub(crate) fn core(kind:&str)->Self { Self {kind:kind.into(),marker:PhantomData} }
}
pub fn define_entry<E>(kind:&str)->Result<EntryKind<E>> {
    if kind.starts_with("pi.") { return Err(Error::ReservedName {category:"entry",name:kind.into()}); }
    Ok(EntryKind::core(kind))
}
#[derive(Clone, Debug)]
pub struct TaskRef<K=KindToken> { pub id:Id, pub kind:K }
#[derive(Clone, Debug)]
pub struct EntryRef<E=Entry> { pub id:Id, pub kind:EntryKind<E> }
#[derive(Clone, Debug, Default)]
pub struct KindConfig { pub rewindable:JsonObject, pub sticky:JsonObject }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct TaskSpec {
    pub kind:String, pub input:JsonValue,
    #[serde(default,skip_serializing_if="Option::is_none")] pub conversation_id:Option<Id>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub after:Option<Vec<Id>>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub background:Option<True>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ForkAt { Entry(Id), Start(Start) }
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Start { #[serde(rename="start")] Start }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ForkParent { pub conversation_id:Id, pub at:ForkAt }
#[derive(Clone, Debug, Default)]
pub struct ConversationSpec {
    pub parent:Option<ForkParent>, pub rewindable:Option<JsonObject>, pub sticky:Option<JsonObject>, pub sections:Option<Vec<SectionSeed>>,
}
#[derive(Clone, Debug, Default)]
pub struct OwnedConversationSpec { pub inherit:Option<bool>, pub rewindable:Option<JsonObject>, pub sticky:Option<JsonObject> }
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub enum WhenBusy { Steer, FollowUp, Reject }
#[derive(Clone, Debug)]
pub struct SendInput { pub content:UserInput, pub request_id:Option<String>, pub when_busy:Option<WhenBusy> }
#[derive(Clone, Debug)]
pub struct ContextView { pub head:Option<Entry>, pub entries:Vec<Entry>, pub messages:Vec<StoredMessage> }
#[derive(Clone, Debug)]
pub enum DocumentSnapshot { Rewindable(RewindableState), Sticky(StickyState), Session(SessionState) }

pub trait TxReads {
    fn conversation(&self,id:Id)->LocalBoxFuture<'_,Result<Option<Conversation>>>;
    fn entry(&self,id:Id)->LocalBoxFuture<'_,Result<Option<Entry>>>;
    fn entries<'a>(&'a self,ids:&'a [Id])->LocalBoxFuture<'a,Result<BTreeMap<Id,Entry>>>;
    fn newest_entry(&self,scan:EntryScan)->LocalBoxFuture<'_,Result<Option<Entry>>>;
    fn scan_entries(&self,scan:EntryScan)->LocalBoxFuture<'_,Result<Vec<Entry>>>;
    fn context(&self,conversation_id:Id,at:Option<Id>)->LocalBoxFuture<'_,Result<ContextView>>;
    fn task(&self,id:Id)->LocalBoxFuture<'_,Result<Option<Task>>>;
    fn tasks(&self,scan:TaskScan)->LocalBoxFuture<'_,Result<Vec<Task>>>;
    fn input(&self,id:Id)->LocalBoxFuture<'_,Result<Option<Input>>>;
    fn rewindable_as_of(&self,conversation_id:Id,at:Id)->LocalBoxFuture<'_,Result<Option<RewindableState>>>;
    fn snapshot(&self,r#ref:DocRef)->Result<DocumentSnapshot>;
}
pub trait SharedTx:TxReads {
    fn plugins(&self,namespace:&Namespace)->Result<Document>;
    fn emit_plugin(&mut self,namespace:&Namespace,name:&str,data:JsonValue)->Result<()>;
    fn config_get(&self,conversation_id:Id,key:&str)->Result<Option<JsonValue>>;
    fn write(&mut self,conversation_id:Id,entry:NewEntry)->LocalBoxFuture<'_,Result<Id>>;
    fn create_task(&mut self,kind:&KindToken,input:JsonValue,opts:TaskOptions)->Result<TaskRef>;
    fn create_conversation(&mut self,spec:ConversationSpec)->Result<Id>;
}
#[derive(Clone, Debug, Default)]
pub struct TaskOptions { pub conversation_id:Option<Id>, pub background:Option<True>, pub after:Vec<Id> }
pub trait HostTx:SharedTx {
    fn config_set(&mut self,conversation_id:Id,key:&str,value:JsonValue)->Result<()>;
    fn config_reset(&mut self,conversation_id:Id,key:&str)->Result<()>;
}
pub trait TaskTx:SharedTx {
    fn checkpoint(&mut self,value:Checkpoint)->Result<()>;
    fn slot(&self,task:&TaskRef)->Result<Document>;
}
pub trait CoreTx:TaskTx {
    fn emit(&mut self,event:ViewEvent)->Result<()>;
    fn rewindable(&self,conversation_id:Id)->Result<Document>;
    fn sticky(&self,conversation_id:Id)->Result<Document>;
    fn session(&self)->Result<Document>;
    fn tool_slot(&self,conversation_id:Id,index:usize)->Result<Document>;
    fn append_entry(&mut self,conversation_id:Id,entry:NewEntry)->Result<Id>;
    fn create_task_spec(&mut self,spec:TaskSpec)->Result<Id>;
    fn send(&mut self,conversation_id:Id,input:SendInput)->LocalBoxFuture<'_,Result<Id>>;
    fn resolve_inputs<'a>(&'a mut self,ids:&'a [Id],resolution:InputResolution)->LocalBoxFuture<'a,Result<()>>;
    fn boundary(&mut self,conversation_id:Id,at:Boundary,head_boundary:Option<Id>)->LocalBoxFuture<'_,Result<BoundaryResult>>;
    fn create_owned_conversation(&mut self,owner_task_id:Id,source_conversation_id:Id,spec:OwnedConversationSpec)->LocalBoxFuture<'_,Result<Id>>;
    fn create_fork_conversation(&mut self,spec:ConversationSpec)->Result<Id>;
    fn mark_task(&mut self,id:Id)->Result<()>;
}
#[derive(Clone, Debug)]
pub enum InputResolution { Done { answer:Id }, Unanswered { reason:InputReason, detail:Option<String> } }
#[derive(Clone, Copy, Debug)]
pub enum Boundary { PostTools, Final }
#[derive(Clone, Debug)]
pub struct BoundaryResult { pub triggers:Vec<Id>, pub terminated:bool }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvocationMode { Run, Abort }
#[derive(Clone, Debug)]
pub struct InvocationToken { task_id:Id, mode:InvocationMode, alive:Rc<Cell<bool>> }
impl InvocationToken {
    pub(crate) fn new(task_id:Id,mode:InvocationMode)->Self { Self {task_id,mode,alive:Rc::new(Cell::new(true))} }
    pub const fn task_id(&self)->Id { self.task_id }
    pub const fn mode(&self)->InvocationMode { self.mode }
    pub fn alive(&self)->bool { self.alive.get() }
    pub fn revoke(&self) { self.alive.set(false); }
    pub fn check(&self,task_id:Id,mode:InvocationMode)->Result<()> {
        if !self.alive() || self.task_id != task_id || self.mode != mode {
            return Err(Error::Forbidden("expired or mismatched invocation token".into()));
        }
        Ok(())
    }
}
pub(crate) enum Invoker {
    Host { conversation_id:Option<Id> }, Kernel { conversation_id:Option<Id> },
    Task { token:InvocationToken, id:Id, conversation_id:Id, kind:KindToken, core:bool, mode:InvocationMode },
}

#[derive(Clone, Debug)]
pub struct HookApi { pub kind:String, pub task_id:Id, pub conversation_id:Id }
pub type HookInfo = HookApi;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag="stage",rename_all="lowercase",rename_all_fields="camelCase")]
pub enum GenerationStatus {
    Waiting { on:CompactionWait }, Preparing, Requesting { attempt:u32 }, Streaming { attempt:u32 },
    Retrying { attempt:u32, retry_at:u64, last_error:String }, Deferred { attempt:u32, poll_at:u64 },
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum CompactionWait { #[serde(rename="compaction")] Compaction }
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all="lowercase")]
pub enum CompactionReason { Threshold, Manual, Overflow }
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all="lowercase")]
pub enum CompactionStage { Summarizing, Retrying }
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub enum GenerationFailure { Provider, Overflow, RetriesExhausted, NoModel }
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub enum CompactionFailure { Stale, Declined, Provider, RetriesExhausted, NoModel }
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all="lowercase")]
pub enum OutcomeStatus { Completed, Failed, Aborted, Orphaned, Faulted }
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub enum QueuedMode { Steer, FollowUp, Write }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ToolSlotView {
    pub call_id:String, pub name:String, pub args:JsonValue, pub status:ToolStatus,
    #[serde(default,skip_serializing_if="Option::is_none")] pub waiting_on:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub output:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub progress:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub details:Option<JsonValue>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub continued_by:Option<Id>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub entry:Option<Id>,
}
impl From<ToolSlot> for ToolSlotView {
    fn from(s:ToolSlot)->Self { Self {call_id:s.call_id,name:s.name,args:s.args,status:s.status,
        waiting_on:s.waiting_on,output:s.output,progress:s.progress,details:s.details,continued_by:s.continued_by,entry:s.entry} }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TurnView {
    pub inputs:Vec<Id>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub generation:Option<GenerationStatus>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub message:Option<Stored<AssistantMessage>>,
    pub tools:Vec<ToolSlotView>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConversationInfo {
    pub id:Id,
    #[serde(default,skip_serializing_if="Option::is_none")] pub parent:Option<ConversationParent>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub owner:Option<Id>,
}
impl From<Conversation> for ConversationInfo {
    fn from(c:Conversation)->Self { Self {id:c.id,parent:c.parent,owner:c.owner} }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct CompactionView {
    pub task_id:Id, pub reason:CompactionReason, pub stage:CompactionStage, pub attempt:u32,
    #[serde(default,skip_serializing_if="Option::is_none")] pub retry_at:Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskView {
    pub kind:String,
    #[serde(default,skip_serializing_if="Option::is_none")] pub background:Option<True>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub marked:Option<True>,
    pub status:JsonValue,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConversationView {
    pub conversation:ConversationInfo, pub entries:Vec<Entry>, pub config:JsonObject, pub inbox:Vec<QueuedInput>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub turn:Option<TurnView>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub compaction:Option<CompactionView>,
    pub tasks:BTreeMap<String,TaskView>, pub plugins:BTreeMap<String,JsonValue>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ToolControl {
    #[serde(default,skip_serializing_if="Option::is_none")] pub terminate:Option<True>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub handoff:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub add_tools:Option<Vec<String>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag="status",rename_all="lowercase")]
pub enum TurnResolution {
    Done { answer:Id },
    Unanswered { reason:InputReason, #[serde(default,skip_serializing_if="Option::is_none")] detail:Option<String> },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag="type",rename_all_fields="camelCase")]
pub enum CoreViewEvent {
    #[serde(rename="entry.added")] EntryAdded { entry:Entry },
    #[serde(rename="head.moved")] HeadMoved { entry:Entry },
    #[serde(rename="turn.started")] TurnStarted { inputs:Vec<Id> },
    #[serde(rename="turn.ended")] TurnEnded { inputs:Vec<Id>, #[serde(flatten)] resolution:TurnResolution },
    #[serde(rename="input.queued")] InputQueued { input:Id, mode:QueuedMode },
    #[serde(rename="input.placed")] InputPlaced { input:Id, entry:Id },
    #[serde(rename="input.aborted")] InputAborted { input:Id },
    #[serde(rename="generation.started")] GenerationStarted { task_id:Id, attempt:u32 },
    #[serde(rename="generation.retrying")] GenerationRetrying { task_id:Id, attempt:u32, retry_at:u64, error:String },
    #[serde(rename="generation.deferred")] GenerationDeferred { task_id:Id, poll_at:u64 },
    #[serde(rename="generation.completed")] GenerationCompleted { task_id:Id, entry:Id, tool_calls:usize },
    #[serde(rename="generation.failed")] GenerationFailed { task_id:Id, reason:GenerationFailure, detail:String,
        #[serde(default,skip_serializing_if="Option::is_none")] entry:Option<Id> },
    #[serde(rename="tool.waiting")] ToolWaiting { task_id:Id, call_id:String, on:String },
    #[serde(rename="tool.started")] ToolStarted { task_id:Id, call_id:String, name:String },
    #[serde(rename="tool.finished")] ToolFinished { task_id:Id, call_id:String, entry:Id, is_error:bool,
        #[serde(default,skip_serializing_if="Option::is_none")] control:Option<ToolControl> },
    #[serde(rename="tool.aborted")] ToolAborted { task_id:Id, call_id:String, entry:Id },
    #[serde(rename="compaction.started")] CompactionStarted { task_id:Id, reason:CompactionReason, through:Id },
    #[serde(rename="compaction.retrying")] CompactionRetrying { task_id:Id, attempt:u32, retry_at:u64, error:String },
    #[serde(rename="compaction.finished")] CompactionFinished { task_id:Id, summary:Id },
    #[serde(rename="compaction.failed")] CompactionFailed { task_id:Id, reason:CompactionFailure, detail:String },
    #[serde(rename="task.started")] TaskStarted { task_id:Id, kind:String,
        #[serde(default,skip_serializing_if="Option::is_none")] background:Option<True> },
    #[serde(rename="task.ended")] TaskEnded { task_id:Id, kind:String, outcome:OutcomeStatus },
    #[serde(rename="config.changed")] ConfigChanged { keys:Vec<String> },
    #[serde(rename="warning")] Warning { source:String, message:String },
}
#[derive(Clone, Debug)]
pub enum ViewEvent { Core(CoreViewEvent), Plugin { name:String, data:JsonValue } }
impl ViewEvent {
    pub fn plugin(namespace:&Namespace,name:&str,data:JsonValue)->Result<Self> {
        if !namespace.is_registered() { return Err(Error::Forbidden("unregistered namespace".into())); }
        Ok(Self::Plugin { name:format!("plugin.{}.{}",namespace.id(),name),data })
    }
}
impl Serialize for ViewEvent {
    fn serialize<S:Serializer>(&self,s:S)->std::result::Result<S::Ok,S::Error> {
        match self {
            Self::Core(event)=>event.serialize(s),
            Self::Plugin{name,data}=> {
                use serde::ser::SerializeStruct;
                let mut object=s.serialize_struct("PluginEvent",2)?;
                object.serialize_field("type",name)?; object.serialize_field("data",data)?; object.end()
            }
        }
    }
}
impl<'de> Deserialize<'de> for ViewEvent {
    fn deserialize<D:Deserializer<'de>>(d:D)->std::result::Result<Self,D::Error> {
        let value=JsonObject::deserialize(d)?;
        if let Some(JsonValue::String(name))=value.get("type") {
            if let Some(name)=name.as_str().filter(|name|name.starts_with("plugin.")) {
                let data=value.get("data").ok_or_else(||serde::de::Error::custom("plugin event requires data"))?;
                return Ok(Self::Plugin {name:name.into(),data:data.clone()});
            }
        }
        let json=serde_json::to_string(&value).map_err(serde::de::Error::custom)?;
        serde_json::from_str(&json).map(Self::Core).map_err(serde::de::Error::custom)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Envelope { pub revision:u64, pub ops:Vec<Op>, pub events:Vec<ViewEvent> }

pub trait ConfigFacade<Cfg> {
    fn get<'a>(&'a self,ctx:&'a Context)->LocalBoxFuture<'a,Result<Cfg>>;
    fn set<'a>(&'a self,patch:JsonObject,ctx:&'a Context)->LocalBoxFuture<'a,Result<()>>;
    fn reset<'a>(&'a self,keys:&'a [String],ctx:&'a Context)->LocalBoxFuture<'a,Result<()>>;
}
pub type Closure<R=JsonValue,F=JsonValue,Tx: ?Sized=dyn TaskTx> = Box<dyn for<'a> FnOnce(&'a mut Tx,&'a Task,&'a Context)->LocalBoxFuture<'a,Result<Completion<R,F>>>>;
pub type AbortClosure<A=JsonValue,Tx: ?Sized=dyn TaskTx> = Box<dyn for<'a> FnOnce(&'a mut Tx,&'a Task,&'a Context)->LocalBoxFuture<'a,Result<A>>>;
pub enum Transition<C=Checkpoint,R=JsonValue,F=JsonValue> { Checkpoint(C), Done(Completion<R,F>), Retry }
pub type NextBuilder<C,R,F,Tx: ?Sized> = Box<dyn for<'a> FnOnce(&'a mut Tx,&'a Task,&'a Context)->LocalBoxFuture<'a,Result<Transition<C,R,F>>>>;
pub enum Step<C=Checkpoint,R=JsonValue,F=JsonValue,Tx: ?Sized=dyn TaskTx> {
    Next(C), Build(NextBuilder<C,R,F,Tx>), Done(Closure<R,F,Tx>),
}
/// Associated contracts preserve each kind's input/checkpoint/result/slot types.
pub trait Kind {
    type Input:Serialize+DeserializeOwned;
    type Checkpoint:Serialize+DeserializeOwned;
    type Result:Serialize+DeserializeOwned;
    type Failure:Serialize+DeserializeOwned;
    type Aborted:Serialize+DeserializeOwned;
    type Hooks;
    type Slot:Serialize+DeserializeOwned;
    type Tx:TaskTx+?Sized;
    fn token(&self)->&KindToken;
    fn turn(&self)->bool;
    fn config(&self)->&KindConfig;
    fn inflight(&self)->&[String];
    fn slot(&self,input:&Self::Input)->Result<Self::Slot>;
    fn describe(&self,task:&Task<Self::Input,Self::Checkpoint>,slot:Option<&Self::Slot>)->Result<JsonValue>;
    fn initial<'a>(&'a self,task:&'a Task<Self::Input,Self::Checkpoint>,rt:&'a dyn Runtime<Self::Hooks,Self::Tx>,ctx:&'a Context)
        ->LocalBoxFuture<'a,Result<Step<Self::Checkpoint,Self::Result,Self::Failure,Self::Tx>>>;
    fn phase<'a>(&'a self,phase:&'a str,task:&'a Task<Self::Input,Self::Checkpoint>,rt:&'a dyn Runtime<Self::Hooks,Self::Tx>,ctx:&'a Context)
        ->LocalBoxFuture<'a,Result<Step<Self::Checkpoint,Self::Result,Self::Failure,Self::Tx>>>;
    fn abort<'a>(&'a self,task:&'a Task<Self::Input,Self::Checkpoint>,rt:&'a dyn Runtime<Self::Hooks,Self::Tx>,ctx:&'a Context)
        ->LocalBoxFuture<'a,Result<AbortClosure<Self::Aborted,Self::Tx>>>;
}
#[derive(Clone, Debug)]
pub struct RequestOptions { pub messages:Vec<StoredMessage>, pub thinking_level:ThinkingLevel }
pub type AssistantStream = Pin<Box<dyn futures::Stream<Item=AssistantMessageEvent>>>;
pub enum DeferredResult { Message(AssistantMessage), Deferred(DeferredHandle) }
pub trait Models {
    fn resolve(&self,r#ref:&ModelRef)->Option<Model>;
    fn stream(&self,model:&Model,request:RequestOptions,ctx:&Context)->AssistantStream;
    fn fetch_deferred<'a>(&'a self,model:&'a Model,handle:&'a DeferredHandle,ctx:&'a Context)->LocalBoxFuture<'a,Result<DeferredResult>>;
    fn cancel_deferred<'a>(&'a self,model:&'a Model,handle:&'a DeferredHandle,ctx:&'a Context)->LocalBoxFuture<'a,Result<()>>;
}
pub type CommitCallback<'a,Tx: ?Sized> = Box<dyn for<'b> FnOnce(&'b mut Tx,&'b Task,&'b Context)->LocalBoxFuture<'b,Result<JsonValue>>+'a>;
pub trait Runtime<H,Tx:TaskTx+?Sized> {
    fn task_id(&self)->Id;
    fn conversation_id(&self)->Id;
    fn kind(&self)->&KindToken;
    fn commit<'a>(&'a self,callback:CommitCallback<'a,Tx>,ctx:&'a Context)->LocalBoxFuture<'a,Result<JsonValue>>;
    fn hooks(&self)->&super::hooks::HookRunner<H>;
    fn models(&self)->&dyn Models;
    fn tools(&self)->&BTreeMap<String,Rc<dyn AnyToolDeclaration>>;
    fn kinds(&self)->&BTreeMap<String,KindToken>;
    fn registries(&self)->RuntimeRegistries<'_>;
    fn process_host(&self)->Option<&dyn ProcessHost>;
    fn plugins(&self)->&BTreeMap<String,Rc<dyn PluginHandler>>;
    fn now(&self)->u64;
    fn sleep<'a>(&'a self,until_ms:u64,ctx:&'a Context)->LocalBoxFuture<'a,Result<()>>;
    fn wait_for_input<'a>(&'a self,id:Id,ctx:&'a Context)->LocalBoxFuture<'a,Result<Input>>;
    fn wait_for_task<'a>(&'a self,id:Id,ctx:&'a Context)->LocalBoxFuture<'a,Result<Task>>;
    fn abort_task<'a>(&'a self,id:Id,ctx:&'a Context)->LocalBoxFuture<'a,Result<AbortTaskResult>>;
    fn abort_conversation<'a>(&'a self,id:Id,ctx:&'a Context)->LocalBoxFuture<'a,Result<()>>;
    fn create_owned_conversation<'a>(&'a self,spec:OwnedConversationSpec,ctx:&'a Context)->LocalBoxFuture<'a,Result<Id>>;
    fn send_owned<'a>(&'a self,conversation_id:Id,input:SendInput,ctx:&'a Context)->LocalBoxFuture<'a,Result<Id>>;
    fn context<'a>(&'a self,conversation_id:Id,at:Option<Id>,ctx:&'a Context)->LocalBoxFuture<'a,Result<ContextView>>;
    fn newest_entry<'a>(&'a self,conversation_id:Id,opts:NewestEntryOptions,ctx:&'a Context)->LocalBoxFuture<'a,Result<Option<Entry>>>;
    fn rewindable<'a>(&'a self,conversation_id:Id,ctx:&'a Context)->LocalBoxFuture<'a,Result<RewindableState>>;
    fn sticky<'a>(&'a self,conversation_id:Id,ctx:&'a Context)->LocalBoxFuture<'a,Result<StickyState>>;
    fn rewindable_as_of<'a>(&'a self,conversation_id:Id,at:Id,ctx:&'a Context)->LocalBoxFuture<'a,Result<Option<RewindableState>>>;
}
pub struct RuntimeRegistries<'a> {
    pub sections:&'a super::system::SectionRegistry,
    pub tools:&'a super::system::ToolRegistry,
}
#[derive(Clone, Copy, Debug)]
pub enum AbortTaskResult { Marked, Terminal }
#[derive(Clone, Debug, Default)]
pub struct NewestEntryOptions { pub kind:Option<String>, pub with_head:Option<bool> }
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all="lowercase")]
pub enum Severity { Info, Warn, Error }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolDiagnostic {
    pub severity:Severity, pub message:String,
    #[serde(default,skip_serializing_if="Option::is_none")] pub code:Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag="type",rename_all="lowercase")]
pub enum ToolContent { Text { text:String }, Image { data:String, #[serde(rename="mimeType")] mime_type:String } }
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ToolResult {
    #[serde(default,skip_serializing_if="Option::is_none")] pub content:Option<Vec<ToolContent>>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub is_error:Option<bool>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub details:Option<JsonValue>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub diagnostics:Option<Vec<ToolDiagnostic>>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub control:Option<ToolControl>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all="lowercase")]
pub enum Replay { Safe, Unsafe }
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct OutputLimits {
    #[serde(default,skip_serializing_if="Option::is_none")] pub max_bytes:Option<usize>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub max_lines:Option<usize>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub retain:Option<super::bounded::Retain>,
}
pub trait ToolDeclaration {
    type Args:DeserializeOwned;
    fn name(&self)->&str;
    fn description(&self)->&str;
    fn parameters(&self)->&JsonObject;
    fn replay(&self)->Replay;
    fn output(&self)->OutputLimits;
    fn execute<'a>(&'a self,args:Self::Args,api:&'a dyn ToolApi,ctx:&'a Context)->LocalBoxFuture<'a,Result<ToolResult>>;
}
pub trait AnyToolDeclaration {
    fn name(&self)->&str;
    fn description(&self)->&str;
    fn parameters(&self)->&JsonObject;
    fn replay(&self)->Replay;
    fn output(&self)->OutputLimits;
    fn execute<'a>(&'a self,args:JsonValue,api:&'a dyn ToolApi,ctx:&'a Context)->LocalBoxFuture<'a,Result<ToolResult>>;
}
impl<T:ToolDeclaration> AnyToolDeclaration for T {
    fn name(&self)->&str { ToolDeclaration::name(self) }
    fn description(&self)->&str { ToolDeclaration::description(self) }
    fn parameters(&self)->&JsonObject { ToolDeclaration::parameters(self) }
    fn replay(&self)->Replay { ToolDeclaration::replay(self) }
    fn output(&self)->OutputLimits { ToolDeclaration::output(self) }
    fn execute<'a>(&'a self,args:JsonValue,api:&'a dyn ToolApi,ctx:&'a Context)->LocalBoxFuture<'a,Result<ToolResult>> {
        Box::pin(async move {
            let args=serde_json::from_str(&serde_json::to_string(&args)?)?;
            ToolDeclaration::execute(self,args,api,ctx).await
        })
    }
}
pub trait OwnedConversation {
    fn id(&self)->Id;
    fn send<'a>(&'a self,input:SendInput,ctx:&'a Context)->LocalBoxFuture<'a,Result<Box<dyn InputHandle+'a>>>;
    fn abort<'a>(&'a self,ctx:&'a Context)->LocalBoxFuture<'a,Result<()>>;
}
pub trait InputHandle {
    fn id(&self)->Id;
    fn wait<'a>(&'a self,ctx:&'a Context)->LocalBoxFuture<'a,Result<Input>>;
    fn result<'a>(&'a self,ctx:&'a Context)->LocalBoxFuture<'a,Result<Option<Input>>>;
}
pub trait BeforeToolApi {
    fn info(&self)->&HookInfo;
    fn call_id(&self)->&str;
    fn waiting<'a>(&'a self,ctx:&'a Context)->LocalBoxFuture<'a,Result<()>>;
    fn memo<'a>(&'a self,name:&'a str,candidate:Option<JsonValue>,ctx:&'a Context)->LocalBoxFuture<'a,Result<Option<JsonValue>>>;
    fn emit<'a>(&'a self,name:&'a str,data:JsonValue,ctx:&'a Context)->LocalBoxFuture<'a,Result<()>>;
}
#[derive(Clone, Debug, Default)]
pub struct ToolProgress { pub progress:Option<String>, pub details:Option<JsonValue>, pub continued_by:Option<Id> }
pub trait ToolApi {
    fn task_id(&self)->Id;
    fn conversation_id(&self)->Id;
    fn call_id(&self)->&str;
    fn stream(&self,chunk:&[u8])->Result<()>;
    fn progress<'a>(&'a self,update:Box<dyn FnOnce(&mut ToolProgress)+'a>,ctx:&'a Context)->LocalBoxFuture<'a,Result<()>>;
    fn memo<'a>(&'a self,name:&'a str,candidate:Option<JsonValue>,ctx:&'a Context)->LocalBoxFuture<'a,Result<Option<JsonValue>>>;
    fn conversation<'a>(&'a self,spec:OwnedConversationSpec,ctx:&'a Context)->LocalBoxFuture<'a,Result<Box<dyn OwnedConversation+'a>>>;
    fn task<'a>(&'a self,kind:&'a KindToken,input:JsonValue,opts:TaskOptions,ctx:&'a Context)->LocalBoxFuture<'a,Result<TaskRef>>;
    fn get_task<'a>(&'a self,r#ref:&'a TaskRef,ctx:&'a Context)->LocalBoxFuture<'a,Result<Option<Task>>>;
    fn wait_for_task<'a>(&'a self,r#ref:&'a TaskRef,ctx:&'a Context)->LocalBoxFuture<'a,Result<Task>>;
    fn slot<'a>(&'a self,r#ref:&'a TaskRef,ctx:&'a Context)->LocalBoxFuture<'a,Result<Option<JsonObject>>>;
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessSpec {
    pub command:String, pub args:Vec<String>, pub cwd:String,
    #[serde(default,skip_serializing_if="Option::is_none")] pub env:Option<BTreeMap<String,String>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag="status",rename_all="lowercase",rename_all_fields="camelCase")]
pub enum ProcessStatus {
    Running { stdout:String, stderr:String, dropped_stdout:u64, dropped_stderr:u64 },
    Exited { exit_code:i32, stdout:String, stderr:String, dropped_stdout:u64, dropped_stderr:u64 }, Unknown,
}
#[derive(Clone, Copy, Debug)]
pub enum ProcessSignal { Sigterm, Sigkill }
pub trait ProcessHost {
    fn start<'a>(&'a self,key:&'a str,spec:ProcessSpec,ctx:&'a Context)->LocalBoxFuture<'a,Result<()>>;
    fn status<'a>(&'a self,key:&'a str,ctx:&'a Context)->LocalBoxFuture<'a,Result<ProcessStatus>>;
    fn kill<'a>(&'a self,key:&'a str,signal:ProcessSignal,ctx:&'a Context)->LocalBoxFuture<'a,Result<()>>;
}
pub trait PluginHandler {
    fn call<'a>(&'a self,input:JsonValue,api:&'a dyn ToolApi,ctx:&'a Context)->LocalBoxFuture<'a,Result<JsonValue>>;
}

#[cfg(test)]
#[path="types_tests.rs"]
mod tests;
