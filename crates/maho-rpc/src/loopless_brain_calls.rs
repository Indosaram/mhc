//! Port of senpi `packages/coding-agent/src/modes/rpc/loopless-brain-calls.ts`.
//!
//! Tracks the calls a loopless runtime runs for the brain while they run, along with the facts
//! the brain sent with each (`senpi#6921`, `senpi#6919`).

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Mutex, PoisonError};

use maho_ext_api::PromptSurface;

/// What the session that issued a call says about itself, sent with the call (`senpi#6921`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LooplessCallFacts {
    /// Its prompt surface: what its host does with the result, which the call's tool reports.
    pub prompt_surface: Option<PromptSurface>,
}

#[derive(Default)]
pub struct LooplessBrainCalls {
    running: Mutex<HashMap<String, LooplessCallFacts>>,
}

struct RunningCallGuard<'a> {
    running: &'a Mutex<HashMap<String, LooplessCallFacts>>,
    tool_call_id: String,
}

impl Drop for RunningCallGuard<'_> {
    fn drop(&mut self) {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.tool_call_id);
    }
}

impl LooplessBrainCalls {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_model_issued_tool_call(&self, tool_call_id: &str) -> bool {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(tool_call_id)
    }

    pub fn prompt_surface_of(&self, tool_call_id: &str) -> Option<PromptSurface> {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(tool_call_id)
            .and_then(|facts| facts.prompt_surface)
    }

    pub async fn run<T, F>(
        &self,
        tool_call_id: &str,
        facts: Option<LooplessCallFacts>,
        call: F,
    ) -> T
    where
        F: Future<Output = T>,
    {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(tool_call_id.to_owned(), facts.unwrap_or_default());
        let _guard = RunningCallGuard {
            running: &self.running,
            tool_call_id: tool_call_id.to_owned(),
        };
        call.await
    }
}

pub fn create_brain_calls() -> LooplessBrainCalls {
    LooplessBrainCalls::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tracks_running_call_facts_and_clears_on_completion() {
        let calls = create_brain_calls();
        assert!(!calls.is_model_issued_tool_call("tc-1"));

        let inside = calls
            .run(
                "tc-1",
                Some(LooplessCallFacts {
                    prompt_surface: Some(PromptSurface::App),
                }),
                async {
                    (
                        calls.is_model_issued_tool_call("tc-1"),
                        calls.prompt_surface_of("tc-1"),
                    )
                },
            )
            .await;

        assert_eq!(inside, (true, Some(PromptSurface::App)));
        assert!(!calls.is_model_issued_tool_call("tc-1"));
        assert_eq!(calls.prompt_surface_of("tc-1"), None);
    }
}
