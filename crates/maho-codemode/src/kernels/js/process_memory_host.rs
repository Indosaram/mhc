use crate::{
    bridge::protocol::KernelMemoryThresholds, kernels::shared::kernel_memory::KernelMemoryPolicy,
};
use serde_json::Value;

pub async fn read_process_footprint(pid: Option<u32>) -> Option<u64> {
    let pid = pid?;
    #[cfg(target_os = "linux")]
    {
        let status = tokio::fs::read_to_string(format!("/proc/{pid}/status"))
            .await
            .ok()?;
        let row = status
            .lines()
            .find_map(|line| line.strip_prefix("RssAnon:"))?;
        let mut fields = row.split_whitespace();
        let kib = fields.next()?.parse::<u64>().ok()?;
        (fields.next()? == "kB").then_some(kib.checked_mul(1024)?)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

pub struct JavaScriptProcessMemoryHost {
    policy: KernelMemoryPolicy,
    restarted: Option<String>,
}

impl JavaScriptProcessMemoryHost {
    pub fn new(thresholds: KernelMemoryThresholds) -> Self {
        Self {
            policy: KernelMemoryPolicy::process(thresholds),
            restarted: None,
        }
    }

    pub fn settled(&mut self, mut message: Value) -> Value {
        if let Some(memory) = message.get("memory") {
            match serde_json::from_value(memory.clone())
                .and_then(|report| serde_json::to_value(self.policy.annotate(report)))
            {
                Ok(report) => message["memory"] = report,
                Err(error) => {
                    maho_ext_api::diagnostic(&format!("Invalid JS process footprint: {error}"))
                }
            }
        }
        if let Some(notice) = self.restarted.take() {
            message["notice"] = Value::String(notice);
            message["kernelState"] = Value::String("restarted".into());
        }
        message
    }

    pub fn claim_recycle(&mut self, idle: bool) -> bool {
        self.policy.claim_recycle(idle)
    }
    pub fn worker_lost(&mut self) {
        self.policy.kernel_retired();
    }
    pub fn mark_restarted(&mut self, reason: &str) {
        self.restarted = Some(format!(
            "[js kernel was restarted after {reason}; every global is lost]"
        ));
    }
}
