//! Port of senpi `packages/agent/src/harness/hooks.ts`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use maho_ai::model::Model;
use maho_ai::types::{BoxFuture, Usage};
use serde_json::{Map, Value};

use crate::harness::context::{Context, with_abort_signal};
use crate::harness::execution::effect_gate::{Gate, GateRefusal};
use crate::harness::session::types::{OperationKind, SettledAssistantMessage};
use crate::harness::telemetry::start_harness_span;
use crate::harness::types::{
    AgentHarnessResources, AgentHarnessStreamOptions, AgentHarnessStreamOptionsPatch,
};
use crate::types::AgentMessage;
use maho_ai::types::ContentBlock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HookName {
    BeforeRun,
    BeforeDrive,
    BeforeRunEnd,
    TransformContext,
    BeforeRequest,
    BeforePayload,
    AfterResponse,
    BeforeTool,
    AfterTool,
    BeforeCompaction,
    BeforeNavigation,
}

impl HookName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BeforeRun => "before_run",
            Self::BeforeDrive => "before_drive",
            Self::BeforeRunEnd => "before_run_end",
            Self::TransformContext => "transform_context",
            Self::BeforeRequest => "before_request",
            Self::BeforePayload => "before_payload",
            Self::AfterResponse => "after_response",
            Self::BeforeTool => "before_tool",
            Self::AfterTool => "after_tool",
            Self::BeforeCompaction => "before_compaction",
            Self::BeforeNavigation => "before_navigation",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlockDecision {
    pub reason: String,
    pub terminate: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HookInvocation {
    pub lane: String,
    pub run_id: String,
    pub prompt: Vec<AgentMessage>,
    pub resources: AgentHarnessResources,
    pub operation: Option<OperationKind>,
    pub messages: Vec<AgentMessage>,
    pub system_prompt: String,
    pub model: Option<Model>,
    pub step: Option<String>,
    pub attempt: Option<u32>,
    pub stream_options: AgentHarnessStreamOptions,
    pub payload: Option<Value>,
    pub message: Option<SettledAssistantMessage>,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub args: Map<String, Value>,
    pub content: Vec<ContentBlock>,
    pub details: Option<Value>,
    pub is_error: bool,
    pub usage: Option<Usage>,
    pub target_id: Option<String>,
    pub preparation: Option<Value>,
}

impl HookInvocation {
    pub fn new(lane: impl Into<String>, run_id: impl Into<String>) -> Self {
        Self {
            lane: lane.into(),
            run_id: run_id.into(),
            prompt: Vec::new(),
            resources: AgentHarnessResources::default(),
            operation: None,
            messages: Vec::new(),
            system_prompt: String::new(),
            model: None,
            step: None,
            attempt: None,
            stream_options: AgentHarnessStreamOptions::default(),
            payload: None,
            message: None,
            tool_call_id: None,
            tool_name: None,
            args: Map::new(),
            content: Vec::new(),
            details: None,
            is_error: false,
            usage: None,
            target_id: None,
            preparation: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AfterToolPatch {
    pub content: Option<Vec<ContentBlock>>,
    pub details: Option<Value>,
    pub is_error: Option<bool>,
    pub usage: Option<Usage>,
    pub terminate: Option<bool>,
}

impl AfterToolPatch {
    pub fn is_empty(&self) -> bool {
        self.content.is_none()
            && self.details.is_none()
            && self.is_error.is_none()
            && self.usage.is_none()
            && self.terminate.is_none()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum HookResult {
    None,
    BeforeRun { messages: Vec<AgentMessage> },
    BeforeRunEnd { follow_up: String },
    TransformContext { messages: Option<Vec<AgentMessage>>, system_prompt: Option<String> },
    BeforeRequest { stream_options: AgentHarnessStreamOptionsPatch },
    BeforePayload { payload: Value },
    AfterResponse { message: Box<SettledAssistantMessage> },
    BeforeTool { args: Option<Map<String, Value>>, block: Option<BlockDecision> },
    AfterTool(AfterToolPatch),
    Structural { decline: Option<bool>, value: Option<Value> },
}

pub type HookHandler =
    Arc<dyn Fn(HookInvocation, Context) -> BoxFuture<'static, Result<HookResult, String>> + Send + Sync>;

#[derive(Debug, Clone)]
pub enum HookRunError {
    Gate(GateRefusal),
    Handler(String),
}

impl std::fmt::Display for HookRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gate(error) => write!(f, "{error}"),
            Self::Handler(message) => f.write_str(message),
        }
    }
}
pub type HookErrorReporter = Arc<dyn Fn(String, HookName, String, Context) -> BoxFuture<'static, ()> + Send + Sync>;

struct HookRegistration {
    id: Option<String>,
    handler: HookHandler,
}

pub struct HookRegistry {
    registrations: Arc<Mutex<BTreeMap<HookName, Vec<Arc<HookRegistration>>>>>,
    report_error: HookErrorReporter,
    closed_error: Mutex<Option<String>>,
}

impl HookRegistry {
    pub fn new(report_error: HookErrorReporter) -> Self {
        Self {
            registrations: Arc::new(Mutex::new(BTreeMap::new())),
            report_error,
            closed_error: Mutex::new(None),
        }
    }

    pub fn on(&self, name: HookName, handler: HookHandler, id: Option<String>) -> Unsubscribe {
        if let Some(error) = self.closed() {
            panic!("{error}");
        }
        let registration = Arc::new(HookRegistration { id, handler });
        let mut registrations = self.registrations.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        registrations.entry(name).or_default().push(registration.clone());
        let registry = HookRegistryHandle::new(self.registrations.clone());
        Unsubscribe::new(move || {
            let mut registrations =
                registry.registrations.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(entry) = registrations.get_mut(&name) {
                entry.retain(|candidate| !Arc::ptr_eq(candidate, &registration));
            }
        })
    }

    pub fn has(&self, name: HookName) -> bool {
        self.registrations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&name)
            .is_some_and(|entries| !entries.is_empty())
    }

    pub fn close(&self, error: impl Into<String>) {
        let mut closed = self.closed_error.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if closed.is_none() {
            *closed = Some(error.into());
        }
    }

    pub async fn run_with_gate(
        &self,
        name: HookName,
        event: HookInvocation,
        gate: &Gate,
        context: &Context,
    ) -> Result<HookResult, HookRunError> {
        gate.admit(|| ()).map_err(HookRunError::Gate)?;
        let admitted_context = with_abort_signal(gate.signal(), context);
        if let Some(signal) = admitted_context.abort_signal().filter(|signal| signal.aborted()) {
            let message = signal.reason().map_or_else(|| "The operation was aborted".to_owned(), |reason| reason.message);
            return Err(HookRunError::Gate(GateRefusal::Closed(message)));
        }
        self.run_admitted(name, event, &admitted_context).await
    }

    pub async fn run_tool_with_gate(
        &self,
        name: HookName,
        event: HookInvocation,
        gate: &Gate,
        context: &Context,
    ) -> Result<HookResult, HookRunError> {
        gate.admit(|| ()).map_err(HookRunError::Gate)?;
        let admitted_context = with_abort_signal(gate.signal(), context);
        if let Some(signal) = admitted_context.abort_signal().filter(|signal| signal.aborted()) {
            let message = signal.reason().map_or_else(|| "The operation was aborted".to_owned(), |reason| reason.message);
            return Err(HookRunError::Gate(GateRefusal::Closed(message)));
        }
        match name {
            HookName::BeforeTool => self.before_tool(event, &admitted_context).await,
            HookName::AfterTool => self.after_tool(event, &admitted_context).await,
            _ => Ok(HookResult::None),
        }
    }

    fn closed(&self) -> Option<String> {
        self.closed_error.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    async fn run_admitted(
        &self,
        name: HookName,
        event: HookInvocation,
        context: &Context,
    ) -> Result<HookResult, HookRunError> {
        if let Some(error) = self.closed() {
            panic!("{error}");
        }
        self.aggregate(name, event, context).await
    }

    async fn aggregate(
        &self,
        name: HookName,
        event: HookInvocation,
        context: &Context,
    ) -> Result<HookResult, HookRunError> {
        match name {
            HookName::BeforeRun => self.before_run(event, context).await,
            HookName::BeforeDrive => {
                self.invoke_all_fail_closed(name, &event, context).await?;
                Ok(HookResult::None)
            }
            HookName::BeforeRunEnd => {
                let follow_up = Arc::new(Mutex::new(None));
                let sink = follow_up.clone();
                self.invoke_all(name, &event, context, Arc::new(move |value| {
                    if let HookResult::BeforeRunEnd { follow_up: value } = value {
                        *sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(value);
                    }
                }))
                .await;
                let follow_up = follow_up.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
                Ok(match follow_up {
                    Some(follow_up) => HookResult::BeforeRunEnd { follow_up },
                    None => HookResult::None,
                })
            }
            HookName::TransformContext => self.transform_context(event, context).await,
            HookName::BeforeRequest => self.before_request(event, context).await,
            HookName::BeforePayload => self.before_payload(event, context).await,
            HookName::AfterResponse => self.after_response(event, context).await,
            HookName::BeforeTool => self.before_tool(event, context).await,
            HookName::AfterTool => self.after_tool(event, context).await,
            HookName::BeforeCompaction => self.first_structural(name, event, "compaction", context).await,
            HookName::BeforeNavigation => self.first_structural(name, event, "summary", context).await,
        }
    }

    fn registrations_for(&self, name: HookName) -> Vec<Arc<HookRegistration>> {
        self.registrations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&name)
            .cloned()
            .unwrap_or_default()
    }

    async fn before_run(&self, event: HookInvocation, context: &Context) -> Result<HookResult, HookRunError> {
        let mut prompt = event.prompt.clone();
        let mut injected: Vec<AgentMessage> = Vec::new();
        for registration in self.registrations_for(HookName::BeforeRun) {
            let mut current = event.clone();
            current.prompt = prompt.clone();
            match (registration.handler)(current, context.clone()).await {
                Ok(HookResult::BeforeRun { messages }) => {
                    injected.extend(messages.iter().cloned());
                    prompt.extend(messages);
                }
                Ok(_) => {}
                Err(message) => {
                    (self.report_error)(message, HookName::BeforeRun, event.lane.clone(), context.clone()).await;
                }
            }
        }
        Ok(if injected.is_empty() {
            HookResult::None
        } else {
            HookResult::BeforeRun { messages: injected }
        })
    }

    async fn before_tool(&self, event: HookInvocation, context: &Context) -> Result<HookResult, HookRunError> {
        let mut args = event.args.clone();
        let mut block: Option<BlockDecision> = None;
        for registration in self.registrations_for(HookName::BeforeTool) {
            let mut current = event.clone();
            current.args = args.clone();
            match self.invoke_tool_registration(HookName::BeforeTool, &registration, current, context).await {
                Ok(HookResult::BeforeTool { args: next_args, block: next_block }) => {
                    if let Some(next_args) = next_args {
                        args = next_args;
                    }
                    if let Some(next_block) = next_block {
                        block = Some(next_block);
                        break;
                    }
                }
                Ok(_) => {}
                Err(message) => {
                    (self.report_error)(message.clone(), HookName::BeforeTool, event.lane.clone(), context.clone())
                        .await;
                    block = Some(BlockDecision { reason: message, terminate: None });
                    break;
                }
            }
        }
        Ok(HookResult::BeforeTool { args: if args == event.args { None } else { Some(args) }, block })
    }

    async fn transform_context(&self, event: HookInvocation, context: &Context) -> Result<HookResult, HookRunError> {
        let mut messages = event.messages.clone();
        let mut system_prompt = event.system_prompt.clone();
        for registration in self.registrations_for(HookName::TransformContext) {
            let mut current = event.clone();
            current.messages = messages.clone();
            current.system_prompt = system_prompt.clone();
            match (registration.handler)(current, context.clone()).await {
                Ok(HookResult::TransformContext { messages: next_messages, system_prompt: next_prompt }) => {
                    if let Some(next_messages) = next_messages {
                        messages = next_messages;
                    }
                    if let Some(next_prompt) = next_prompt {
                        system_prompt = next_prompt;
                    }
                }
                Ok(_) => {}
                Err(message) => {
                    (self.report_error)(message, HookName::TransformContext, event.lane.clone(), context.clone())
                        .await;
                }
            }
        }
        Ok(HookResult::TransformContext { messages: Some(messages), system_prompt: Some(system_prompt) })
    }

    async fn before_request(&self, event: HookInvocation, context: &Context) -> Result<HookResult, HookRunError> {
        let mut stream_options = event.stream_options.clone();
        let mut changed = false;
        for registration in self.registrations_for(HookName::BeforeRequest) {
            let mut current = event.clone();
            current.stream_options = stream_options.clone();
            match (registration.handler)(current, context.clone()).await {
                Ok(HookResult::BeforeRequest { stream_options: patch }) => {
                    stream_options = apply_stream_options_patch(&stream_options, &patch);
                    changed = true;
                }
                Ok(_) => {}
                Err(message) => {
                    (self.report_error)(message, HookName::BeforeRequest, event.lane.clone(), context.clone())
                        .await;
                }
            }
        }
        Ok(if changed {
            HookResult::BeforeRequest {
                stream_options: create_stream_options_patch(&event.stream_options, &stream_options),
            }
        } else {
            HookResult::None
        })
    }

    async fn before_payload(&self, event: HookInvocation, context: &Context) -> Result<HookResult, HookRunError> {
        let mut payload = event.payload.clone();
        for registration in self.registrations_for(HookName::BeforePayload) {
            let mut current = event.clone();
            current.payload = payload.clone();
            match (registration.handler)(current, context.clone()).await {
                Ok(HookResult::BeforePayload { payload: next }) => {
                    payload = Some(next);
                }
                Ok(_) => {}
                Err(message) => {
                    (self.report_error)(message, HookName::BeforePayload, event.lane.clone(), context.clone())
                        .await;
                }
            }
        }
        Ok(HookResult::BeforePayload { payload: payload.unwrap_or(Value::Null) })
    }

    async fn after_response(&self, event: HookInvocation, context: &Context) -> Result<HookResult, HookRunError> {
        let mut message = event.message.clone();
        for registration in self.registrations_for(HookName::AfterResponse) {
            let mut current = event.clone();
            current.message = message.clone();
            match (registration.handler)(current, context.clone()).await {
                Ok(HookResult::AfterResponse { message: next }) => {
                    message = Some(*next);
                }
                Ok(_) => {}
                Err(report) => {
                    (self.report_error)(report, HookName::AfterResponse, event.lane.clone(), context.clone()).await;
                }
            }
        }
        Ok(match message {
            Some(message) => HookResult::AfterResponse { message: Box::new(message) },
            None => HookResult::None,
        })
    }

    async fn after_tool(&self, event: HookInvocation, context: &Context) -> Result<HookResult, HookRunError> {
        let mut current_content = event.content.clone();
        let mut current_details = event.details.clone();
        let mut current_is_error = event.is_error;
        let mut current_usage = event.usage;
        let mut aggregate = AfterToolPatch::default();
        for registration in self.registrations_for(HookName::AfterTool) {
            let mut current = event.clone();
            current.content = current_content.clone();
            current.details = current_details.clone();
            current.is_error = current_is_error;
            current.usage = current_usage;
            match self.invoke_tool_registration(HookName::AfterTool, &registration, current, context).await {
                Ok(HookResult::AfterTool(patch)) => {
                    if patch.content.is_some() {
                        aggregate.content = patch.content.clone();
                    }
                    if patch.details.is_some() {
                        aggregate.details = patch.details.clone();
                    }
                    if patch.is_error.is_some() {
                        aggregate.is_error = patch.is_error;
                    }
                    if patch.usage.is_some() {
                        aggregate.usage = patch.usage;
                    }
                    if patch.terminate.is_some() {
                        aggregate.terminate = patch.terminate;
                    }
                    if let Some(content) = patch.content.clone() {
                        current_content = content;
                    }
                    if let Some(details) = patch.details.clone() {
                        current_details = Some(details);
                    }
                    if let Some(is_error) = patch.is_error {
                        current_is_error = is_error;
                    }
                    if let Some(usage) = patch.usage {
                        current_usage = Some(usage);
                    }
                }
                Ok(_) => {}
                Err(message) => {
                    (self.report_error)(message, HookName::AfterTool, event.lane.clone(), context.clone()).await;
                }
            }
        }
        Ok(if aggregate.is_empty() { HookResult::None } else { HookResult::AfterTool(aggregate) })
    }

    async fn first_structural(
        &self,
        name: HookName,
        event: HookInvocation,
        result_field: &str,
        context: &Context,
    ) -> Result<HookResult, HookRunError> {
        for registration in self.registrations_for(name) {
            let value = match (registration.handler)(event.clone(), context.clone()).await {
                Ok(HookResult::Structural { decline, value }) => {
                    let declined = decline == Some(true);
                    if declined && value.is_some() {
                        (self.report_error)(
                            format!("{} hook cannot return both decline and {result_field}", name.as_str()),
                            name,
                            event.lane.clone(),
                            context.clone(),
                        )
                        .await;
                        continue;
                    }
                    if declined || value.is_some() {
                        HookResult::Structural { decline, value }
                    } else {
                        continue;
                    }
                }
                Ok(_) => continue,
                Err(message) => {
                    (self.report_error)(message, name, event.lane.clone(), context.clone()).await;
                    continue;
                }
            };
            return Ok(value);
        }
        Ok(HookResult::None)
    }

    async fn invoke_tool_registration(
        &self,
        name: HookName,
        registration: &Arc<HookRegistration>,
        event: HookInvocation,
        context: &Context,
    ) -> Result<HookResult, String> {
        let mut attributes = Map::new();
        attributes.insert("pi.lane.name".to_owned(), Value::String(event.lane.clone()));
        attributes.insert("pi.operation.id".to_owned(), Value::String(event.run_id.clone()));
        attributes.insert("pi.hook.name".to_owned(), Value::String(name.as_str().to_owned()));
        if let Some(id) = &registration.id {
            attributes.insert("pi.hook.registration_id".to_owned(), Value::String(id.clone()));
        }
        let handler = registration.handler.clone();
        let handler_name = name;
        start_harness_span(
            "pi.harness.hook",
            attributes,
            move |span, span_context| {
                Box::pin(async move {
                    match handler(event, span_context).await {
                        Ok(value) => {
                            let blocked = handler_name == HookName::BeforeTool
                                && matches!(&value, HookResult::BeforeTool { block: Some(_), .. });
                            let mut outcome = Map::new();
                            outcome.insert(
                                "pi.hook.outcome".to_owned(),
                                Value::String(if blocked { "blocked" } else { "completed" }.to_owned()),
                            );
                            span.set_attributes(outcome);
                            Ok(value)
                        }
                        Err(error) => {
                            let mut outcome = Map::new();
                            outcome.insert("pi.hook.outcome".to_owned(), Value::String("failed".to_owned()));
                            span.set_attributes(outcome);
                            span.set_status(crate::harness::telemetry::SpanStatus::Error { error: None });
                            Err(error)
                        }
                    }
                })
            },
            context,
        )
        .await
    }

    async fn invoke_all_fail_closed(
        &self,
        name: HookName,
        event: &HookInvocation,
        context: &Context,
    ) -> Result<(), HookRunError> {
        for registration in self.registrations_for(name) {
            if let Err(message) = (registration.handler)(event.clone(), context.clone()).await {
                (self.report_error)(message.clone(), name, event.lane.clone(), context.clone()).await;
                return Err(HookRunError::Handler(message));
            }
        }
        Ok(())
    }

    async fn invoke_all(
        &self,
        name: HookName,
        event: &HookInvocation,
        context: &Context,
        apply: Arc<dyn Fn(HookResult) + Send + Sync>,
    ) {
        for registration in self.registrations_for(name) {
            match (registration.handler)(event.clone(), context.clone()).await {
                Ok(value) => apply(value),
                Err(message) => {
                    (self.report_error)(message, name, event.lane.clone(), context.clone()).await;
                }
            }
        }
    }
}

struct HookRegistryHandle {
    registrations: Arc<Mutex<BTreeMap<HookName, Vec<Arc<HookRegistration>>>>>,
}

impl HookRegistryHandle {
    fn new(registrations: Arc<Mutex<BTreeMap<HookName, Vec<Arc<HookRegistration>>>>>) -> Self {
        Self { registrations }
    }
}

pub struct Unsubscribe {
    callback: Arc<dyn Fn() + Send + Sync>,
}

impl Unsubscribe {
    fn new(callback: impl Fn() + Send + Sync + 'static) -> Self {
        Self { callback: Arc::new(callback) }
    }

    pub fn call(&self) {
        (self.callback)();
    }
}

pub fn apply_stream_options_patch(
    base: &AgentHarnessStreamOptions,
    patch: &AgentHarnessStreamOptionsPatch,
) -> AgentHarnessStreamOptions {
    let mut next = base.clone();
    if let Some(value) = patch.transport {
        next.transport = value;
    }
    if let Some(value) = patch.timeout_ms {
        next.timeout_ms = value;
    }
    if let Some(value) = patch.max_retries {
        next.max_retries = value;
    }
    if let Some(value) = patch.max_retry_delay_ms {
        next.max_retry_delay_ms = value;
    }
    if let Some(value) = patch.cache_retention {
        next.cache_retention = value;
    }
    if let Some(value) = patch.deferred {
        next.deferred = value;
    }
    if let Some(headers) = &patch.headers {
        match headers {
            None => next.headers = None,
            Some(patch_headers) => {
                let mut headers = next.headers.clone().unwrap_or_default();
                for (key, value) in patch_headers {
                    match value {
                        None => {
                            headers.remove(key);
                        }
                        Some(value) => {
                            headers.insert(key.clone(), Value::String(value.clone()));
                        }
                    }
                }
                next.headers = Some(headers);
            }
        }
    }
    if let Some(metadata) = &patch.metadata {
        match metadata {
            None => next.metadata = None,
            Some(patch_metadata) => {
                let mut metadata = next.metadata.clone().unwrap_or_default();
                for (key, value) in patch_metadata {
                    match value {
                        None => {
                            metadata.remove(key);
                        }
                        Some(value) => {
                            metadata.insert(key.clone(), value.clone());
                        }
                    }
                }
                next.metadata = Some(metadata);
            }
        }
    }
    next
}

pub fn create_stream_options_patch(
    base: &AgentHarnessStreamOptions,
    value: &AgentHarnessStreamOptions,
) -> AgentHarnessStreamOptionsPatch {
    let mut patch = AgentHarnessStreamOptionsPatch::default();
    if base.transport != value.transport {
        patch.transport = Some(value.transport);
    }
    if base.timeout_ms != value.timeout_ms {
        patch.timeout_ms = Some(value.timeout_ms);
    }
    if base.max_retries != value.max_retries {
        patch.max_retries = Some(value.max_retries);
    }
    if base.max_retry_delay_ms != value.max_retry_delay_ms {
        patch.max_retry_delay_ms = Some(value.max_retry_delay_ms);
    }
    if base.cache_retention != value.cache_retention {
        patch.cache_retention = Some(value.cache_retention);
    }
    if base.deferred != value.deferred {
        patch.deferred = Some(value.deferred);
    }
    if base.headers != value.headers {
        match &value.headers {
            None => patch.headers = Some(None),
            Some(value_headers) => {
                let mut headers: BTreeMap<String, Option<String>> = BTreeMap::new();
                for key in base.headers.as_ref().map_or_else(Vec::new, |headers| headers.keys().cloned().collect::<Vec<_>>()) { 
                    if !value_headers.contains_key(&key) {
                        headers.insert(key, None);
                    }
                }
                for (key, header) in value_headers {
                    let base_value = base.headers.as_ref().and_then(|headers| headers.get(key));
                    if base_value != Some(header) {
                        headers.insert(key.clone(), header.as_str().map(ToOwned::to_owned));
                    }
                }
                if base.headers.is_none() && headers.is_empty() {
                    patch.headers = Some(Some(BTreeMap::new()));
                } else if !headers.is_empty() {
                    patch.headers = Some(Some(headers));
                }
            }
        }
    }
    if base.metadata != value.metadata {
        match &value.metadata {
            None => patch.metadata = Some(None),
            Some(value_metadata) => {
                let mut metadata: BTreeMap<String, Option<Value>> = BTreeMap::new();
                for key in base.metadata.as_ref().map_or_else(Vec::new, |metadata| metadata.keys().cloned().collect::<Vec<_>>()) { 
                    if !value_metadata.contains_key(&key) {
                        metadata.insert(key, None);
                    }
                }
                for (key, metadata_value) in value_metadata {
                    let base_value = base.metadata.as_ref().and_then(|metadata| metadata.get(key));
                    if base_value != Some(metadata_value) {
                        metadata.insert(key.clone(), Some(metadata_value.clone()));
                    }
                }
                if base.metadata.is_none() && metadata.is_empty() {
                    patch.metadata = Some(Some(BTreeMap::new()));
                } else if !metadata.is_empty() {
                    patch.metadata = Some(Some(metadata));
                }
            }
        }
    }
    patch
}
