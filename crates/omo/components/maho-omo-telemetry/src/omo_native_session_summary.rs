use crate::{
    omo_native_consent::NativeConsent,
    omo_native_parallel_summary::SummaryCapture,
    session_summary_store::{SummaryStore, rollup},
    session_summary_window::{add_turn, is_session_sampled, open_window, summary_properties},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex, PoisonError},
};

pub const RAW_TURN_SAMPLE_RATE: f64 = 0.05;
pub struct SessionSummaryRecorder {
    store: SummaryStore,
    live: BTreeMap<String, Value>,
    retired: BTreeMap<String, u64>,
    recovered: bool,
    consent: Arc<NativeConsent>,
    capture: SummaryCapture,
    now: Arc<dyn Fn() -> f64 + Send + Sync>,
}
impl SessionSummaryRecorder {
    pub fn new(
        state: &Path,
        consent: Arc<NativeConsent>,
        capture: SummaryCapture,
        now: Arc<dyn Fn() -> f64 + Send + Sync>,
    ) -> Self {
        Self {
            store: SummaryStore::new(state),
            live: BTreeMap::new(),
            retired: BTreeMap::new(),
            recovered: false,
            consent,
            capture,
            now,
        }
    }
    fn recover(&mut self) -> std::io::Result<()> {
        if self.recovered {
            return Ok(());
        }
        self.recovered = true;
        for mut orphan in self.store.claim_orphans((self.now)())? {
            if !self.still_consented(&orphan) {
                continue;
            }
            let hash = orphan["sessionHash"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let seq = orphan["nextSeq"].as_u64().unwrap_or(0);
            let window = orphan["window"].take();
            orphan["nextSeq"] = json!(seq + u64::from(!window.is_null()));
            orphan["pid"] = json!(std::process::id());
            if orphan["nextSeq"].as_u64().unwrap_or(0) > 0 {
                self.store.write(&orphan)?;
            }
            if !window.is_null() {
                (self.capture)(
                    "session_summary",
                    summary_properties(
                        &hash,
                        &window,
                        seq,
                        "recovered",
                        orphan["sessionRole"].as_str().unwrap_or("main"),
                        RAW_TURN_SAMPLE_RATE,
                    ),
                );
            }
            self.live.insert(hash, orphan);
        }
        Ok(())
    }
    fn still_consented(&self, value: &Value) -> bool {
        value["consent"] == "enabled"
            && value["scope"]["cwd"]
                .as_str()
                .is_some_and(|cwd| self.consent.allowed_for(Path::new(cwd)))
    }
    fn load(&mut self, hash: &str, cwd: &Path, role: &str) -> std::io::Result<Value> {
        if let Some(value) = self.live.remove(hash) {
            if self.still_consented(&value) {
                return Ok(value);
            }
            self.retired
                .entry(hash.into())
                .and_modify(|seq| *seq = (*seq).max(value["nextSeq"].as_u64().unwrap_or(0)))
                .or_insert(value["nextSeq"].as_u64().unwrap_or(0));
        }
        let stored = self
            .store
            .read(hash)?
            .filter(|value| self.still_consented(value));
        let mut value =
            stored.unwrap_or_else(|| rollup(hash, 0, Value::Null, cwd, (self.now)(), role));
        let seq = value["nextSeq"]
            .as_u64()
            .unwrap_or(0)
            .max(self.retired.get(hash).copied().unwrap_or(0));
        value["nextSeq"] = json!(seq);
        Ok(value)
    }
    fn flush(&self, hash: &str, value: &mut Value, reason: &str) -> std::io::Result<()> {
        let window = value["window"].take();
        let window = if window.is_null() {
            open_window((self.now)(), &json!(0))
        } else {
            window
        };
        let seq = value["nextSeq"].as_u64().unwrap_or(0);
        value["nextSeq"] = json!(seq + 1);
        if reason == "shutdown" {
            self.store.remove(hash)?;
        } else {
            self.store.write(value)?;
        }
        (self.capture)(
            "session_summary",
            summary_properties(
                hash,
                &window,
                seq,
                reason,
                value["sessionRole"].as_str().unwrap_or("main"),
                RAW_TURN_SAMPLE_RATE,
            ),
        );
        Ok(())
    }
    pub fn record(
        &mut self,
        hash: &str,
        event: &Value,
        cwd: &Path,
        role: &str,
    ) -> std::io::Result<()> {
        if !self.consent.allowed() {
            return self.discard(hash);
        }
        let Some((mut properties, invalid)) =
            crate::omo_native_turns::turn_completed_properties(event, hash)
        else {
            return Ok(());
        };
        self.recover()?;
        let mut value = self.load(hash, cwd, role)?;
        let at = (self.now)();
        if value["window"].is_null() {
            value["window"] = open_window(at, &properties["turn_index"]);
        }
        add_turn(&mut value["window"], &properties, event, invalid, at);
        value["updatedAt"] = json!(at);
        value["scope"] = json!({"cwd":cwd});
        if value["window"]["totals"]
            .as_array()
            .is_some_and(|v| v.len() >= 50)
            || at - value["window"]["startedAt"].as_f64().unwrap_or(at) >= 30.0 * 60_000.0
        {
            self.flush(hash, &mut value, "checkpoint")?;
        } else {
            self.store.write(&value)?;
        }
        self.live.insert(hash.into(), value);
        if is_session_sampled(hash, RAW_TURN_SAMPLE_RATE) && self.consent.allowed() {
            properties["sample_rate"] = json!(RAW_TURN_SAMPLE_RATE);
            let provider = event["message"]["provider"].as_str().unwrap_or_default();
            let model = event["message"]["model"].as_str().unwrap_or_default();
            if !crate::product_identity::KNOWN_MODELS
                .iter()
                .any(|(p, models)| *p == provider && models.contains(&model))
            {
                properties["model_id"] = json!("custom");
            }
            (self.capture)("turn_completed", properties);
        }
        Ok(())
    }
    pub fn finish(
        &mut self,
        hash: &str,
        cwd: &Path,
        reason: &str,
        role: &str,
    ) -> std::io::Result<()> {
        if !self.consent.allowed() {
            return self.discard(hash);
        }
        self.recover()?;
        let mut value = self.load(hash, cwd, role)?;
        if !value["window"].is_null()
            || (reason != "reload" && value["nextSeq"].as_u64().unwrap_or(0) > 0)
        {
            self.flush(hash, &mut value, reason)?;
        } else if reason != "reload" {
            self.store.remove(hash)?;
        }
        if reason == "reload" {
            self.live.insert(hash.into(), value);
        }
        Ok(())
    }
    fn discard(&mut self, hash: &str) -> std::io::Result<()> {
        if let Some(value) = self.live.remove(hash).or(self.store.read(hash)?) {
            self.retired
                .entry(hash.into())
                .and_modify(|seq| *seq = (*seq).max(value["nextSeq"].as_u64().unwrap_or(0)))
                .or_insert(value["nextSeq"].as_u64().unwrap_or(0));
        }
        self.store.remove(hash)
    }
    pub fn purge(&mut self) -> std::io::Result<()> {
        let allowed = self.consent.allowed();
        let hashes: Vec<_> = self
            .live
            .iter()
            .filter(|(_, value)| !allowed || !self.still_consented(value))
            .map(|(hash, _)| hash.clone())
            .collect();
        for hash in hashes {
            self.discard(&hash)?;
        }
        Ok(())
    }
}

impl Drop for SessionSummaryRecorder {
    fn drop(&mut self) {
        if let Err(error) = self.purge() {
            maho_ext_api::diagnostic(&format!(
                "omo-native summary disposal purge failed: {error}"
            ));
        }
    }
}

pub fn register_session_summary(
    api: &mut maho_ext_api::ExtensionApi,
    recorder: Arc<Mutex<SessionSummaryRecorder>>,
    hash: Arc<dyn Fn(&str) -> String + Send + Sync>,
    role: &'static str,
) {
    use maho_ext_api::{EventKind, EventResult, ExtensionEvent, SessionReason};
    let periodic = Arc::downgrade(&recorder);
    let task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>> = Arc::default();
    let start_task = Arc::clone(&task);
    api.on(
        EventKind::SessionStart,
        Arc::new(move |_, _| {
            let mut task = start_task.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(previous) = task.take() {
                previous.abort();
            }
            let periodic = periodic.clone();
            *task = Some(tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
                loop {
                    interval.tick().await;
                    let Some(periodic) = periodic.upgrade() else {
                        break;
                    };
                    if let Err(error) = periodic
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .purge()
                    {
                        maho_ext_api::diagnostic(&format!(
                            "omo-native summary consent purge failed: {error}"
                        ));
                    }
                }
            }));
            Box::pin(async { Ok(EventResult::None) })
        }),
    );
    api.on(
        EventKind::SessionShutdown,
        Arc::new(move |event, ctx| {
            if let Some(task) = task.lock().unwrap_or_else(PoisonError::into_inner).take() {
                task.abort();
            }
            let reason = match event {
                ExtensionEvent::SessionShutdown(event) if event.reason == SessionReason::Reload => {
                    "reload"
                }
                _ => "shutdown",
            };
            if let Err(error) = recorder
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .finish(
                    &hash(ctx.session_manager.session_id()),
                    &ctx.cwd,
                    reason,
                    role,
                )
            {
                crate::omo_native_component::warn(
                    ctx,
                    &format!("omo-native session summary failed: {error}"),
                );
            }
            Box::pin(async { Ok(EventResult::None) })
        }),
    );
}
pub fn session_role(
    profile: &maho_ext_api::ExtensionSessionProfile,
    env: &telemetry_core::TelemetryEnv,
) -> &'static str {
    match profile
        .session_context
        .get("role")
        .map(String::as_str)
        .filter(|s| !s.is_empty())
    {
        Some("dag_child") => "dag_child",
        Some("member") => "member",
        Some(_) => "child",
        None if env.get("SENPI_TASK_MEMBER").is_some_and(|s| !s.is_empty()) => "member",
        None if env
            .get("OMO_SENPI_TASK_RPC_CHILD")
            .is_some_and(|s| s == "1") =>
        {
            "child"
        }
        None => "main",
    }
}
