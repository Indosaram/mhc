//! Port of senpi `packages/agent/src/public-entry.ts`.
//!
//! The package entry's loop and `Agent`: the loop core builds no store (senpi#4996), so the entry
//! supplies a fresh in-memory store per batch unless the caller passes its own factory or `ToolHost`.

use maho_ai::utils::abort::AbortSignal;
use maho_ai::utils::event_stream::EventStream;

use crate::agent::Agent as LoopAgent;
use crate::agent_loop::{
    AgentEventSink, AgentLoopError, agent_loop as loop_agent_loop,
    agent_loop_continue as loop_agent_loop_continue, run_agent_loop as loop_run_agent_loop,
    run_agent_loop_continue as loop_run_agent_loop_continue,
};
use crate::execution::prepared::memory_batch_store::open_memory_batch_store;
use crate::execution::tool_host::no_preflight_carry_fn;
use crate::types::{AgentContext, AgentEvent, AgentLoopConfig, AgentMessage, StreamFn};

pub type Agent = LoopAgent;

fn with_batch_store(mut config: AgentLoopConfig) -> AgentLoopConfig {
    if config.batch_store_factory.is_none() {
        config.batch_store_factory = Some(open_memory_batch_store());
    }
    if config.after_tool_freeze.is_none() {
        config.after_tool_freeze = Some(no_preflight_carry_fn());
    }
    config
}

pub fn agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> EventStream<AgentEvent, Vec<AgentMessage>> {
    loop_agent_loop(
        prompts,
        context,
        with_batch_store(config),
        signal,
        stream_fn,
    )
}

pub fn agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> EventStream<AgentEvent, Vec<AgentMessage>> {
    loop_agent_loop_continue(context, with_batch_store(config), signal, stream_fn)
}

pub async fn run_agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentEventSink,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Result<Vec<AgentMessage>, AgentLoopError> {
    loop_run_agent_loop(
        prompts,
        context,
        with_batch_store(config),
        emit,
        signal,
        stream_fn,
    )
    .await
}

pub async fn run_agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentEventSink,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Result<Vec<AgentMessage>, AgentLoopError> {
    loop_run_agent_loop_continue(context, with_batch_store(config), emit, signal, stream_fn).await
}
