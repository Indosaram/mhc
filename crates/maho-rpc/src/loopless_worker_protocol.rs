//! Port of senpi `packages/coding-agent/src/modes/rpc/loopless-worker-protocol.ts`.

use serde::{Deserialize, Serialize};

use crate::loopless_runtime_settings::LooplessCliConfiguration;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
#[allow(clippy::large_enum_variant)]
pub enum HostToLooplessWorker {
    Start {
        configuration: LooplessCliConfiguration,
        cwd: String,
    },
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum LooplessWorkerToHost {
    Ready { tools: Vec<String> },
    Failure { error: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn roundtrips_worker_protocol_messages() {
        let start = HostToLooplessWorker::Start {
            configuration: LooplessCliConfiguration {
                cwd: "/repo".into(),
                agent_dir: "/agent".into(),
                ..Default::default()
            },
            cwd: "/repo".into(),
        };
        let encoded = serde_json::to_value(&start).expect("serialize");
        assert_eq!(encoded["type"], "start");
        let decoded: HostToLooplessWorker = serde_json::from_value(encoded).expect("deserialize");
        assert_eq!(decoded, start);

        let ready: LooplessWorkerToHost =
            serde_json::from_value(json!({ "type": "ready", "tools": ["read", "bash"] }))
                .expect("ready");
        assert_eq!(
            ready,
            LooplessWorkerToHost::Ready {
                tools: vec!["read".into(), "bash".into()]
            }
        );
    }
}
