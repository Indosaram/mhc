use crate::{
    bridge::protocol::{KernelMemoryReport, KernelMemoryThresholds},
    kernels::shared::kernel_memory::KernelMemoryPolicy,
};
use serde_json::Value;

pub struct KernelMemoryBridge {
    policy: KernelMemoryPolicy,
    process: Option<super::process_memory_host::JavaScriptProcessMemoryHost>,
    cached: std::sync::Arc<std::sync::Mutex<Option<u64>>>,
}

impl KernelMemoryBridge {
    pub fn new(thresholds: KernelMemoryThresholds) -> Self {
        Self {
            policy: KernelMemoryPolicy::new(thresholds),
            process: None,
            cached: Default::default(),
        }
    }

    pub fn process(thresholds: KernelMemoryThresholds) -> Self {
        Self {
            policy: KernelMemoryPolicy::new(thresholds.clone()),
            process: Some(super::process_memory_host::JavaScriptProcessMemoryHost::new(thresholds)),
            cached: Default::default(),
        }
    }

    pub fn with_cache(mut self, cached: std::sync::Arc<std::sync::Mutex<Option<u64>>>) -> Self {
        self.cached = cached;
        self
    }

    pub fn observe_query(&self, bytes: u64) {
        *self.cached.lock().expect("JS memory cache lock") = Some(bytes);
    }

    pub fn mark_restarted(&mut self, reason: &str) {
        if let Some(process) = &mut self.process {
            process.mark_restarted(reason);
        }
    }

    pub fn consume(&mut self, message: &Value) -> bool {
        let reading = if message["type"] == "kernel-memory-collected" {
            Some(&message["liveBytes"])
        } else if message["type"] == "status" && message["event"]["op"] == "kernel-memory-collected"
        {
            Some(&message["event"]["liveBytes"])
        } else {
            None
        };
        let Some(reading) = reading else {
            return false;
        };
        if let Some(bytes) = reading.as_u64() {
            if self.process.is_none() {
                self.observe_query(bytes);
            }
            self.policy.observe_live(bytes);
        }
        true
    }

    pub fn settled(&mut self, mut message: Value) -> Value {
        if let Some(bytes) = message["memory"]["liveBytes"].as_u64() {
            self.observe_query(bytes);
        }
        if let Some(process) = &mut self.process {
            return process.settled(message);
        }
        if let Some(memory) = message.get("memory") {
            // PERF-DIVERGENCE(senpi kernel-memory-bridge): upstream mutates the report object in place; this port deserializes from a borrowed Value instead of cloning it first. Result is identical.
            match <KernelMemoryReport as serde::Deserialize>::deserialize(memory) {
                Ok(report) => match serde_json::to_value(self.policy.annotate(report)) {
                    Ok(report) => message["memory"] = report,
                    Err(error) => maho_ext_api::diagnostic(&format!(
                        "JS memory report serialization failed: {error}"
                    )),
                },
                Err(error) => {
                    maho_ext_api::diagnostic(&format!("Invalid JS memory report: {error}"))
                }
            }
        }
        message
    }

    pub fn claim_recycle(&mut self, idle: bool) -> bool {
        if let Some(process) = &mut self.process {
            return process.claim_recycle(idle);
        }
        self.policy.claim_recycle(idle)
    }
    pub fn worker_lost(&mut self) {
        *self.cached.lock().expect("JS memory cache lock") = None;
        if let Some(process) = &mut self.process {
            process.worker_lost();
        }
        self.policy.kernel_retired();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn idle_collection_updates_cache_and_worker_loss_forgets_it() {
        let cache = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut bridge = KernelMemoryBridge::new(KernelMemoryThresholds {
            gc_watermark_bytes: 50,
            notice_bytes: 100,
            ceiling_bytes: 200,
        })
        .with_cache(cache.clone());
        bridge.consume(
            &json!({"type":"status","event":{"op":"kernel-memory-collected","liveBytes":42}}),
        );
        assert_eq!(*cache.lock().expect("cache"), Some(42));
        bridge.worker_lost();
        assert_eq!(*cache.lock().expect("cache"), None);
    }

    #[test]
    fn result_annotation_survives_idle_recycle_and_announces_once() {
        let mut bridge = KernelMemoryBridge::new(KernelMemoryThresholds {
            gc_watermark_bytes: 50,
            notice_bytes: 100,
            ceiling_bytes: 200,
        });
        let breached = bridge.settled(
            json!({"type":"result","memory":{"liveBytes":200,"measure":"heap","gcRan":true}}),
        );
        assert_eq!(breached["memory"]["overCeiling"], true);
        assert!(!bridge.claim_recycle(false));
        assert!(bridge.claim_recycle(true));
        bridge.worker_lost();
        let fresh =
            bridge.settled(json!({"type":"result","memory":{"liveBytes":10,"measure":"heap"}}));
        assert_eq!(fresh["memory"]["recycled"], true);
        let next =
            bridge.settled(json!({"type":"result","memory":{"liveBytes":10,"measure":"heap"}}));
        assert!(next["memory"].get("recycled").is_none());
    }

    #[test]
    fn idle_collection_rearms_notice() {
        let mut bridge = KernelMemoryBridge::new(KernelMemoryThresholds {
            gc_watermark_bytes: 50,
            notice_bytes: 100,
            ceiling_bytes: 200,
        });
        let result =
            || json!({"type":"result","memory":{"liveBytes":100,"measure":"heap","gcRan":true}});
        assert!(bridge.settled(result())["memory"]["notice"].is_string());
        assert!(bridge.settled(result())["memory"].get("notice").is_none());
        assert!(bridge.consume(
            &json!({"type":"status","event":{"op":"kernel-memory-collected","liveBytes":40}})
        ));
        assert!(bridge.settled(result())["memory"]["notice"].is_string());
    }
}
