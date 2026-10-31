//! Preview-safe prefix composition with opt-in sizes-only accounting.
use crate::prompt_size_report::{DynamicSystemPromptPart, PromptSizeAccounting, PromptSizeReport};
use crate::prompt_size_startup::PromptSizeStartupNotes;
use maho_ai::{
    types::{BoxFuture, Model, Tool},
    utils::abort::AbortSignal,
};
use std::{collections::HashMap, sync::Arc};
pub const PROMPT_STARTED_REASON: &str =
    "a prompt started composing its turn before the prefix was built";
pub struct PromptCachePrefixBuilds {
    in_flight: std::sync::Mutex<Vec<(u64, maho_ai::utils::abort::AbortController)>>,
    next_id: std::sync::atomic::AtomicU64,
}
impl Default for PromptCachePrefixBuilds {
    fn default() -> Self {
        Self {
            in_flight: Default::default(),
            next_id: std::sync::atomic::AtomicU64::new(0),
        }
    }
}
impl PromptCachePrefixBuilds {
    pub async fn build(
        &self,
        sources: Arc<dyn PromptCachePrefixSources>,
        signal: Option<AbortSignal>,
        sizes_only: bool,
    ) -> PromptCachePrefixResult {
        self.run(signal, |signal| {
            build_prompt_cache_prefix_request(sources, signal, sizes_only)
        })
        .await
    }
    /// Track one in-flight build (senpi `build`'s `AbortSignal.any([options.signal, controller.signal])`):
    /// `work` receives a signal that aborts with [`PROMPT_STARTED_REASON`] on [`Self::cancel_all`]
    /// and with `prefix build cancelled` when the caller's `signal` aborts.
    pub async fn run<T, F, Fut>(&self, signal: Option<AbortSignal>, work: F) -> T
    where
        F: FnOnce(AbortSignal) -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let controller = maho_ai::utils::abort::AbortController::new();
        let linked = signal.and_then(|external| {
            if external.aborted() {
                controller.abort(Some(caller_cancelled()));
                return None;
            }
            let linked = controller.clone();
            let listener =
                external.add_abort_listener(move |_| linked.abort(Some(caller_cancelled())));
            Some((external, listener))
        });
        self.in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((id, controller.clone()));
        let _tracked = InFlightBuild {
            builds: self,
            id,
            linked,
        };
        work(controller.signal()).await
    }
    pub fn cancel_all(&self) {
        let controllers = std::mem::take(
            &mut *self
                .in_flight
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for (_, controller) in controllers {
            controller.abort(Some(maho_ai::utils::abort::AbortReason::new(
                "AbortError",
                PROMPT_STARTED_REASON,
            )));
        }
    }
}
const PREFIX_BUILD_CANCELLED: &str = "prefix build cancelled";
fn caller_cancelled() -> maho_ai::utils::abort::AbortReason {
    maho_ai::utils::abort::AbortReason::new("AbortError", PREFIX_BUILD_CANCELLED)
}
/// senpi `cancelled(signal)`: the abort reason of a tracked build's signal.
pub fn cancelled_reason(signal: &AbortSignal) -> String {
    signal.reason().map_or_else(
        || PREFIX_BUILD_CANCELLED.to_owned(),
        |reason| reason.message,
    )
}
/// Untracks a build when it settles or its future is dropped.
struct InFlightBuild<'a> {
    builds: &'a PromptCachePrefixBuilds,
    id: u64,
    linked: Option<(AbortSignal, maho_ai::utils::abort::ListenerId)>,
}
impl Drop for InFlightBuild<'_> {
    fn drop(&mut self) {
        self.builds
            .in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(key, _)| *key != self.id);
        if let Some((external, listener)) = self.linked.take() {
            external.remove_abort_listener(listener);
        }
    }
}
pub struct PromptContribution {
    pub path: String,
    pub before: String,
    pub after: String,
    pub parts: Option<Vec<DynamicSystemPromptPart>>,
}
pub struct ComposedPrompt {
    pub system_prompt: String,
    pub contributions: Vec<PromptContribution>,
}
pub trait PromptCachePrefixSources: Send + Sync {
    fn ready(&self) -> BoxFuture<'_, Result<(), String>>;
    fn preview_unsafe_paths(&self) -> Vec<String>;
    fn base_system_prompt(&self) -> String;
    fn base_system_prompt_parts(&self) -> Vec<DynamicSystemPromptPart>;
    fn skill_count(&self) -> usize;
    fn extension_names(&self) -> HashMap<String, String>;
    fn compose(
        &self,
        base: String,
        signal: AbortSignal,
    ) -> BoxFuture<'_, Result<ComposedPrompt, String>>;
    fn model(&self) -> Option<Model>;
    fn tools(&self) -> Vec<Tool>;
    fn options(&self) -> serde_json::Value;
    fn prepare_request(
        &self,
        model: Model,
        options: serde_json::Value,
    ) -> BoxFuture<'_, Result<(Model, serde_json::Value), String>>;
    fn startup_notes(&self) -> PromptSizeStartupNotes;
}
pub struct PromptCachePrefixRequest {
    pub model: Model,
    pub system_prompt: String,
    pub tools: Vec<Tool>,
    pub options: serde_json::Value,
    pub size_report: Option<PromptSizeReport>,
    pub startup_notes: Option<PromptSizeStartupNotes>,
}
pub enum PromptCachePrefixResult {
    Ready(Box<PromptCachePrefixRequest>),
    Skipped(String),
}
pub async fn compose_turn_system_prompt(
    sources: &dyn PromptCachePrefixSources,
    signal: &AbortSignal,
    sizes_only: bool,
) -> Result<(String, Option<PromptSizeAccounting>), String> {
    let mut system_prompt = sources.base_system_prompt();
    let mut accounting = None;
    for _ in 0..2 {
        if signal.aborted() {
            return Err("prefix build cancelled".into());
        }
        let base = sources.base_system_prompt();
        accounting = sizes_only.then(|| {
            PromptSizeAccounting::new(
                &base,
                sources.skill_count(),
                &sources.base_system_prompt_parts(),
                sources.extension_names(),
            )
        });
        let composed = sources.compose(base.clone(), signal.clone()).await?;
        if let Some(accounting) = &mut accounting {
            for contribution in composed.contributions {
                accounting.observe(
                    &contribution.path,
                    &contribution.before,
                    &contribution.after,
                    contribution.parts.as_deref(),
                );
            }
        }
        system_prompt = composed.system_prompt;
        if sources.base_system_prompt() == base {
            break;
        }
    }
    Ok((system_prompt, accounting))
}
pub async fn build_prompt_cache_prefix_request(
    sources: Arc<dyn PromptCachePrefixSources>,
    signal: AbortSignal,
    sizes_only: bool,
) -> PromptCachePrefixResult {
    let work = async {
        sources.ready().await?;
        let unsafe_paths = sources.preview_unsafe_paths();
        if !unsafe_paths.is_empty() {
            return Err(format!(
                "before_agent_start handlers not registered previewSafe: {}",
                unsafe_paths.join(", ")
            ));
        }
        let (system_prompt, accounting) =
            compose_turn_system_prompt(&*sources, &signal, sizes_only).await?;
        let model = sources.model().ok_or("no model selected")?;
        let options = sources.options();
        let (model, options) = if sizes_only {
            (model, options)
        } else {
            sources.prepare_request(model, options).await?
        };
        let tools = sources.tools();
        let size_report = accounting.map(|accounting| accounting.report(&tools));
        Ok(PromptCachePrefixRequest {
            model,
            system_prompt,
            tools,
            options,
            size_report,
            startup_notes: sizes_only.then(|| sources.startup_notes()),
        })
    };
    tokio::select! { biased; () = signal.cancelled() => PromptCachePrefixResult::Skipped(cancelled_reason(&signal)), result = work => match result { Ok(request) => PromptCachePrefixResult::Ready(Box::new(request)), Err(reason) => PromptCachePrefixResult::Skipped(reason) } }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poll_once<F: std::future::Future>(
        future: std::pin::Pin<&mut F>,
    ) -> std::task::Poll<F::Output> {
        future.poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    }

    #[test]
    fn cancel_all_aborts_tracked_builds_with_the_prompt_started_reason() {
        let builds = PromptCachePrefixBuilds::default();
        let mut build = std::pin::pin!(builds.run(None, |signal| async move {
            signal.cancelled().await;
            cancelled_reason(&signal)
        }));
        assert!(poll_once(build.as_mut()).is_pending());
        builds.cancel_all();
        assert_eq!(
            poll_once(build.as_mut()),
            std::task::Poll::Ready(PROMPT_STARTED_REASON.to_owned())
        );
        assert!(
            builds
                .in_flight
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
    }

    #[test]
    fn caller_signal_cancels_its_build_and_settled_builds_untrack() {
        let builds = PromptCachePrefixBuilds::default();
        let caller = maho_ai::utils::abort::AbortController::new();
        let mut build = std::pin::pin!(builds.run(Some(caller.signal()), |signal| async move {
            signal.cancelled().await;
            cancelled_reason(&signal)
        }));
        assert!(poll_once(build.as_mut()).is_pending());
        caller.abort(None);
        assert_eq!(
            poll_once(build.as_mut()),
            std::task::Poll::Ready(PREFIX_BUILD_CANCELLED.to_owned())
        );
        let mut settled = std::pin::pin!(builds.run(None, |_| async { 7 }));
        assert_eq!(poll_once(settled.as_mut()), std::task::Poll::Ready(7));
        assert!(
            builds
                .in_flight
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
    }
}
