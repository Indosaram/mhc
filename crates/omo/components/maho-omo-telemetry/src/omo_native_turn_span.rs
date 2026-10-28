//! Port of `packages/omo-senpi/src/components/telemetry/omo-native-turn-span.ts`.
//!
//! One registration's (one session's) turn spans. State is per registration, never per component: one
//! extension instance serves every session of a shared host, and two sessions' turns may overlap.

use std::sync::{Arc, Mutex, PoisonError};

use telemetry_core::{
    TurnSpanAttribute, TurnSpanAttributes, TurnSpanHandle, TurnSpanStart, TurnSpanStatus, TurnTracing,
    clear_turn_traceparent, is_traceparent, publish_turn_traceparent,
};

use crate::product_identity::mask_provider_and_model;

/// The `gen_ai.provider.name` well-known values for our provider ids; an id with no well-known value keeps
/// our id, and `custom` stays `custom`.
pub fn gen_ai_provider_name(provider: &str) -> String {
    match provider {
        "anthropic" => "anthropic".into(),
        "openai" => "openai".into(),
        "google" => "gcp.gemini".into(),
        "xai" => "x_ai".into(),
        other => other.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnModel {
    pub provider: String,
    pub model_id: String,
}

fn model_attributes(model: &TurnModel) -> TurnSpanAttributes {
    let (provider, model_id) = mask_provider_and_model(&model.provider, &model.model_id);
    TurnSpanAttributes::from([
        (
            "gen_ai.provider.name".to_string(),
            TurnSpanAttribute::String(gen_ai_provider_name(&provider)),
        ),
        (
            "omo.provider".to_string(),
            TurnSpanAttribute::String(provider),
        ),
        (
            "omo.model_id".to_string(),
            TurnSpanAttribute::String(model_id),
        ),
    ])
}

pub struct OmoNativeTurnSpansOptions {
    /// The live transport's span sink; `None` on the PostHog path, which has none.
    pub traces: Arc<dyn Fn() -> Option<Arc<dyn TurnTracing>> + Send + Sync>,
    pub is_consented: Arc<dyn Fn() -> bool + Send + Sync>,
    /// The fresh send-time consent read, taken at the moment a span would end and so be exported.
    pub is_send_allowed: Arc<dyn Fn() -> bool + Send + Sync>,
    pub session_role: String,
    pub inherited_traceparent: Option<String>,
}

struct OpenSpan {
    handle: TurnSpanHandle,
    request_model_set: bool,
}

struct SpanState {
    open: Option<OpenSpan>,
    last_published: Option<(String, String)>,
}

pub struct OmoNativeTurnSpans {
    options: OmoNativeTurnSpansOptions,
    parent: Option<String>,
    state: Mutex<SpanState>,
}

pub fn create_omo_native_turn_spans(options: OmoNativeTurnSpansOptions) -> Arc<OmoNativeTurnSpans> {
    let parent = options
        .inherited_traceparent
        .clone()
        .filter(|parent| is_traceparent(parent));
    Arc::new(OmoNativeTurnSpans {
        options,
        parent,
        state: Mutex::new(SpanState {
            open: None,
            last_published: None,
        }),
    })
}

impl OmoNativeTurnSpans {
    /// The published traceparent lives exactly as long as its turn, so no later launch carries a stale trace
    /// id: not after the turn ends, not after an opt-out stops the next one from opening.
    fn unpublish(state: &mut SpanState) {
        if let Some((raw_session_id, traceparent)) = state.last_published.take() {
            clear_turn_traceparent(&raw_session_id, Some(&traceparent));
        }
    }

    /// Ending a span exports it, so every ending (turn end, abandon, a replaced turn, shutdown) reads consent
    /// fresh at that moment; after a withdrawal the span is dropped unended, never exported.
    fn close(&self, status: TurnSpanStatus) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(open) = state.open.take()
            && (self.options.is_send_allowed)()
        {
            open.handle.end(status);
        }
        Self::unpublish(&mut state);
    }

    /// Opens this session's `omo.turn` span; a span still open from a turn that never ended closes as `error`.
    pub fn start(
        &self,
        turn_index: f64,
        session_hash: &str,
        raw_session_id: Option<&str>,
        model: Option<&TurnModel>,
    ) {
        self.close(TurnSpanStatus::Error);
        if !(self.options.is_consented)() {
            return;
        }
        let mut attributes = TurnSpanAttributes::from([
            (
                "gen_ai.operation.name".to_string(),
                TurnSpanAttribute::String("chat".into()),
            ),
            (
                "session.id".to_string(),
                TurnSpanAttribute::String(session_hash.to_string()),
            ),
            (
                "omo.session.role".to_string(),
                TurnSpanAttribute::String(self.options.session_role.clone()),
            ),
            (
                "omo.turn.index".to_string(),
                TurnSpanAttribute::Number(turn_index),
            ),
        ]);
        if let Some(model) = model {
            attributes.extend(model_attributes(model));
            let (_, model_id) = mask_provider_and_model(&model.provider, &model.model_id);
            attributes.insert(
                "gen_ai.request.model".to_string(),
                TurnSpanAttribute::String(model_id),
            );
        }
        let Some(handle) = (self.options.traces)().and_then(|traces| {
            traces.start_turn(TurnSpanStart {
                attributes,
                parent_traceparent: self.parent.clone(),
            })
        }) else {
            return;
        };
        if let Some(raw_session_id) = raw_session_id {
            // A child the task engine launches during this turn continues this trace.
            publish_turn_traceparent(raw_session_id, &handle.traceparent);
        }
        let traceparent = handle.traceparent.clone();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.open = Some(OpenSpan {
            handle,
            request_model_set: model.is_some(),
        });
        state.last_published = raw_session_id.map(|raw| (raw.to_string(), traceparent));
    }

    pub fn end(&self, response: Option<&TurnModel>, response_model_id: Option<&str>, failed: bool) {
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(open) = state.open.as_mut() else {
                return;
            };
            if let Some(response) = response {
                let mut attributes = model_attributes(response);
                if !open.request_model_set {
                    let (_, model_id) =
                        mask_provider_and_model(&response.provider, &response.model_id);
                    attributes.insert(
                        "gen_ai.request.model".to_string(),
                        TurnSpanAttribute::String(model_id),
                    );
                }
                let (_, response_model) = mask_provider_and_model(
                    &response.provider,
                    response_model_id.unwrap_or(&response.model_id),
                );
                attributes.insert(
                    "gen_ai.response.model".to_string(),
                    TurnSpanAttribute::String(response_model),
                );
                open.handle.set_attributes(attributes);
            }
        }
        self.close(if failed {
            TurnSpanStatus::Error
        } else {
            TurnSpanStatus::Ok
        });
    }

    /// Ends an open span that will see no `turn_end` (the run ended).
    pub fn abandon(&self) {
        self.close(TurnSpanStatus::Error);
    }

    pub fn run<T>(&self, callback: impl FnOnce() -> T) -> T {
        let handle = {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.open.as_ref().map(|open| open.handle.clone())
        };
        match handle {
            Some(handle) => handle.run(callback),
            None => callback(),
        }
    }

    /// The async counterpart of [`Self::run`]: the task-local scope survives awaits.
    pub async fn run_async<F: std::future::Future>(&self, future: F) -> F::Output {
        let handle = {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.open.as_ref().map(|open| open.handle.clone())
        };
        match handle {
            Some(handle) => handle.run_async(future).await,
            None => future.await,
        }
    }

    /// Session shutdown, before the transport shuts down: a span still open is ended as `error` with
    /// `omo.turn.end_reason = shutdown` while the send-time consent read holds, else dropped unended.
    pub fn shutdown(&self) {
        {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(open) = state.open.as_ref() {
                open.handle.set_attributes(TurnSpanAttributes::from([(
                    "omo.turn.end_reason".to_string(),
                    TurnSpanAttribute::String("shutdown".into()),
                )]));
            }
        }
        self.close(TurnSpanStatus::Error);
    }

    pub fn is_open(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .open
            .is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use telemetry_core::create_turn_tracing;

    struct Fixture {
        records: Arc<Mutex<Vec<telemetry_core::TurnSpanRecord>>>,
        consented: Arc<AtomicBool>,
        allowed: Arc<AtomicBool>,
        spans: Arc<OmoNativeTurnSpans>,
    }

    fn fixture() -> Fixture {
        let records: Arc<Mutex<Vec<telemetry_core::TurnSpanRecord>>> = Arc::default();
        let sink = Arc::clone(&records);
        let tracing: Arc<dyn TurnTracing> = Arc::new(create_turn_tracing(
            Arc::new(|| true),
            Arc::new(move |record| sink.lock().unwrap().push(record)),
        ));
        let consented = Arc::new(AtomicBool::new(true));
        let allowed = Arc::new(AtomicBool::new(true));
        let consented_flag = Arc::clone(&consented);
        let allowed_flag = Arc::clone(&allowed);
        let spans = create_omo_native_turn_spans(OmoNativeTurnSpansOptions {
            traces: Arc::new(move || Some(Arc::clone(&tracing))),
            is_consented: Arc::new(move || consented_flag.load(Ordering::SeqCst)),
            is_send_allowed: Arc::new(move || allowed_flag.load(Ordering::SeqCst)),
            session_role: "main".into(),
            inherited_traceparent: None,
        });
        Fixture {
            records,
            consented,
            allowed,
            spans,
        }
    }

    fn model(provider: &str, model_id: &str) -> TurnModel {
        TurnModel {
            provider: provider.into(),
            model_id: model_id.into(),
        }
    }

    #[test]
    fn three_turns_produce_three_exact_pairs() {
        let fixture = fixture();
        for index in 1..=3 {
            fixture.spans.start(
                f64::from(index),
                "hashed-session",
                Some("raw-session"),
                Some(&model("openai", "gpt-5.6-sol")),
            );
            assert!(fixture.spans.is_open());
            fixture.spans.end(
                Some(&model("openai", "gpt-5.6-sol")),
                None,
                false,
            );
            assert!(!fixture.spans.is_open());
        }
        let records = fixture.records.lock().unwrap();
        assert_eq!(records.len(), 3);
        for (index, record) in records.iter().enumerate() {
            assert_eq!(record.name, telemetry_core::TURN_SPAN_NAME);
            assert_eq!(record.status, Some(TurnSpanStatus::Ok));
            assert_eq!(
                record.attributes["omo.turn.index"],
                TurnSpanAttribute::Number(f64::from(index as u32 + 1))
            );
            assert_eq!(
                record.attributes["gen_ai.operation.name"],
                TurnSpanAttribute::String("chat".into())
            );
            assert_eq!(
                record.attributes["omo.session.role"],
                TurnSpanAttribute::String("main".into())
            );
            assert_eq!(
                record.attributes["gen_ai.request.model"],
                TurnSpanAttribute::String("gpt-5.6-sol".into())
            );
        }
        assert_ne!(records[0].trace_id, records[1].trace_id);
    }

    #[test]
    fn model_attributes_mask_private_gateways() {
        let fixture = fixture();
        fixture.spans.start(
            1.0,
            "hashed",
            None,
            Some(&model("private-gateway", "acme-internal-ft")),
        );
        fixture.spans.end(
            Some(&model("private-gateway", "acme-internal-ft")),
            None,
            false,
        );
        let records = fixture.records.lock().unwrap();
        assert_eq!(
            records[0].attributes["gen_ai.provider.name"],
            TurnSpanAttribute::String("custom".into())
        );
        assert_eq!(
            records[0].attributes["omo.provider"],
            TurnSpanAttribute::String("custom".into())
        );
        assert_eq!(
            records[0].attributes["omo.model_id"],
            TurnSpanAttribute::String("custom".into())
        );
        assert_eq!(
            records[0].attributes["gen_ai.response.model"],
            TurnSpanAttribute::String("custom".into())
        );
    }

    #[test]
    fn well_known_provider_names_are_mapped() {
        assert_eq!(gen_ai_provider_name("google"), "gcp.gemini");
        assert_eq!(gen_ai_provider_name("xai"), "x_ai");
        assert_eq!(gen_ai_provider_name("anthropic"), "anthropic");
        assert_eq!(gen_ai_provider_name("openai"), "openai");
        assert_eq!(gen_ai_provider_name("custom"), "custom");
        assert_eq!(gen_ai_provider_name("sionic-openrouter"), "sionic-openrouter");
    }

    #[test]
    fn an_opted_out_turn_opens_nothing() {
        let fixture = fixture();
        fixture.consented.store(false, Ordering::SeqCst);
        fixture
            .spans
            .start(1.0, "hashed", None, Some(&model("openai", "gpt-5.6-sol")));
        assert!(!fixture.spans.is_open());
        fixture.spans.end(None, None, false);
        assert!(fixture.records.lock().unwrap().is_empty());
    }

    #[test]
    fn a_withdrawal_drops_the_span_unended() {
        let fixture = fixture();
        fixture
            .spans
            .start(1.0, "hashed", None, Some(&model("openai", "gpt-5.6-sol")));
        fixture.allowed.store(false, Ordering::SeqCst);
        fixture.spans.end(None, None, false);
        assert!(fixture.records.lock().unwrap().is_empty());
    }

    #[test]
    fn a_replaced_turn_ends_the_previous_one_as_error() {
        let fixture = fixture();
        fixture
            .spans
            .start(1.0, "hashed", None, Some(&model("openai", "gpt-5.6-sol")));
        fixture
            .spans
            .start(2.0, "hashed", None, Some(&model("openai", "gpt-5.6-sol")));
        let records = fixture.records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, Some(TurnSpanStatus::Error));
    }

    #[test]
    fn failures_and_abandonment_end_as_error() {
        let fixture = fixture();
        fixture.spans.start(1.0, "hashed", None, None);
        fixture.spans.end(None, None, true);
        fixture.spans.start(2.0, "hashed", None, None);
        fixture.spans.abandon();
        let records = fixture.records.lock().unwrap();
        assert_eq!(records.len(), 2);
        assert!(records.iter().all(|record| record.status == Some(TurnSpanStatus::Error)));
    }

    #[test]
    fn shutdown_marks_the_reason_and_ends_the_span() {
        let fixture = fixture();
        fixture.spans.start(1.0, "hashed", None, None);
        fixture.spans.shutdown();
        let records = fixture.records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].attributes["omo.turn.end_reason"],
            TurnSpanAttribute::String("shutdown".into())
        );
        assert_eq!(records[0].status, Some(TurnSpanStatus::Error));
        assert!(!fixture.spans.is_open());
    }

    #[test]
    fn a_published_traceparent_lives_only_as_long_as_its_turn() {
        let fixture = fixture();
        fixture.spans.start(1.0, "hashed", Some("raw-session"), None);
        let published = telemetry_core::read_turn_traceparent("raw-session").expect("published");
        assert!(telemetry_core::is_traceparent(&published));
        fixture.spans.end(None, None, false);
        assert_eq!(telemetry_core::read_turn_traceparent("raw-session"), None);
    }

    #[test]
    fn children_inherit_the_published_trace() {
        let fixture = fixture();
        fixture.spans.start(1.0, "hashed", Some("raw-parent"), None);
        let parent = telemetry_core::read_turn_traceparent("raw-parent").unwrap();
        let child = fixture
            .spans
            .run(telemetry_core::active_turn_traceparent);
        assert_eq!(child.as_deref(), Some(parent.as_str()));
        fixture.spans.end(None, None, false);
        assert_eq!(
            fixture.spans.run(telemetry_core::active_turn_traceparent),
            None
        );
    }

    #[test]
    fn an_inherited_parent_is_adopted_and_malformed_ones_are_ignored() {
        let records: Arc<Mutex<Vec<telemetry_core::TurnSpanRecord>>> = Arc::default();
        let sink = Arc::clone(&records);
        let tracing: Arc<dyn TurnTracing> = Arc::new(create_turn_tracing(
            Arc::new(|| true),
            Arc::new(move |record| sink.lock().unwrap().push(record)),
        ));
        let parent = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";
        let spans = create_omo_native_turn_spans(OmoNativeTurnSpansOptions {
            traces: Arc::new(move || Some(Arc::clone(&tracing))),
            is_consented: Arc::new(|| true),
            is_send_allowed: Arc::new(|| true),
            session_role: "child".into(),
            inherited_traceparent: Some(parent.into()),
        });
        spans.start(1.0, "hashed", None, None);
        spans.end(None, None, false);
        let records = records.lock().unwrap();
        assert_eq!(records[0].trace_id, &parent[3..35]);
        assert_eq!(records[0].parent_span_id.as_deref(), Some(&parent[36..52]));
    }

    #[test]
    fn no_tracing_transport_means_no_spans() {
        let spans = create_omo_native_turn_spans(OmoNativeTurnSpansOptions {
            traces: Arc::new(|| None),
            is_consented: Arc::new(|| true),
            is_send_allowed: Arc::new(|| true),
            session_role: "main".into(),
            inherited_traceparent: Some("invalid".into()),
        });
        spans.start(1.0, "hashed", Some("raw"), None);
        assert!(!spans.is_open());
        spans.end(None, None, false);
        assert_eq!(telemetry_core::read_turn_traceparent("raw"), None);
    }
}
