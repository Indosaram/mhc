//! senpi `modes/rpc/compact-reset-stream.ts`: `reset_stream` operates on existing admission
//! authority, freezing the next generation and installing a fresh admission baseline.

use crate::compact_admission_baseline::{
    BaselineIdentity, GeneratedFramesCursor, admission_baseline,
};
use crate::compact_admission_storage::AdmissionState;
use crate::compact_admissions::CompactAdmissions;
use crate::compact_recovery_state::{RecoveryCheckpoints, RecoveryLane, RecoveryRoute};
use crate::compact_recovery_types::{RecoveryCaller, RecoveryRouteKind};
use serde_json::{Value, json};
use std::sync::Arc;

pub trait ResetStreamHost: Send + Sync {
    fn current_connection(&self) -> Option<String>;
    fn socket_epoch_of(&self, peer: &str) -> Option<String>;
    fn admissions(&self) -> Option<Arc<CompactAdmissions>>;
    fn recovery_checkpoints(&self) -> RecoveryCheckpoints;
    fn recovery_lane(&self, session_id: &str) -> Option<Arc<dyn RecoveryLane>>;
    fn is_compact_subscription(&self, peer: &str, admission_id: &str, session_id: &str) -> bool;
    fn install_admission_baseline(
        &self,
        peer: &str,
        baseline: GeneratedFramesCursor,
        still_same_epoch: Box<dyn Fn() -> bool + Send + Sync>,
    );
}

#[derive(Debug, Clone)]
pub struct ResetStreamCommandInput {
    pub id: Option<String>,
    pub session_id: Option<String>,
    pub admission_id: String,
}

fn error_response(id: Option<&str>, code: &str, data: Value) -> Value {
    let mut resp = json!({
        "type": "response",
        "command": "reset_stream",
        "success": false,
        "error": code,
        "errorCode": code,
        "errorData": data,
    });
    if let Some(id) = id {
        resp["id"] = Value::String(id.to_owned());
    }
    resp
}

fn refused_unknown(id: Option<&str>) -> Value {
    error_response(
        id,
        "recovery_unknown_token",
        json!({"kind":"repair_needed","reason":"unknown_token"}),
    )
}

/// A reset operates on existing admission authority, not a fresh open or an attachment.
pub async fn reset_stream(
    host: Arc<dyn ResetStreamHost>,
    command: ResetStreamCommandInput,
) -> Option<Value> {
    let peer = host.current_connection()?;
    let Some(socket_epoch) = host.socket_epoch_of(&peer) else {
        return Some(refused_unknown(command.id.as_deref()));
    };
    let (Some(admissions), Some(session_id)) = (host.admissions(), command.session_id.clone())
    else {
        return Some(refused_unknown(command.id.as_deref()));
    };
    let result = match admissions
        .result(&socket_epoch, &command.admission_id)
        .await
    {
        Ok(Some(res)) => res,
        _ => return Some(refused_unknown(command.id.as_deref())),
    };
    if result.state != AdmissionState::Complete {
        return Some(refused_unknown(command.id.as_deref()));
    }
    let matches_session = result.answer.get("sessionId").and_then(Value::as_str)
        == Some(session_id.as_str())
        || host.is_compact_subscription(&peer, &command.admission_id, &session_id);
    if !matches_session {
        return Some(refused_unknown(command.id.as_deref()));
    }

    let checkpoints = host.recovery_checkpoints();
    let caller = RecoveryCaller {
        socket_epoch: socket_epoch.clone(),
        session_id: session_id.clone(),
    };
    let stream = checkpoints.stream(&caller);
    let lane = host.recovery_lane(&session_id);
    let (Some(stream), Some(lane)) = (stream, lane) else {
        return Some(refused_unknown(command.id.as_deref()));
    };
    if !stream.routes.iter().any(|r| r == &command.admission_id) {
        return Some(refused_unknown(command.id.as_deref()));
    }

    let route_kind = if host.is_compact_subscription(&peer, &command.admission_id, &session_id) {
        RecoveryRouteKind::Mirror
    } else {
        RecoveryRouteKind::Ordinary
    };
    let route = RecoveryRoute {
        socket_epoch: socket_epoch.clone(),
        admission_id: command.admission_id.clone(),
        route_kind,
        peer: peer.clone(),
        session_id: session_id.clone(),
    };
    let frozen = match checkpoints.freeze(&route, lane, false) {
        Ok(cp) => cp,
        Err(failure) => {
            return Some(error_response(
                command.id.as_deref(),
                &format!("recovery_{}", failure.kind_str()),
                serde_json::to_value(&failure).unwrap_or(Value::Null),
            ));
        }
    };

    let token = frozen.token;
    let generation = token.admission.generation;
    let mut answer = serde_json::Map::new();
    if let Some(id) = command.id {
        answer.insert("id".into(), Value::String(id));
    }
    answer.insert("type".into(), json!("response"));
    answer.insert("command".into(), json!("reset_stream"));
    answer.insert("success".into(), json!(true));
    answer.insert("sessionId".into(), Value::String(session_id.clone()));
    answer.insert(
        "admissionId".into(),
        Value::String(command.admission_id.clone()),
    );
    answer.insert("generation".into(), json!(generation));
    answer.insert(
        "data".into(),
        json!({"recovery":{"token": token.id, "componentRef": "manifest"}}),
    );

    let baseline = admission_baseline(
        Box::pin(async move { Some(answer) }),
        BaselineIdentity {
            session_id,
            admission_id: command.admission_id,
            generation,
            token: Some(token.id),
            event_lease: None,
            component_ref: None,
        },
        Arc::new(|| true),
        Some(stream.routes),
    );
    let check_host = host.clone();
    let check_peer = peer.clone();
    host.install_admission_baseline(
        &peer,
        baseline,
        Box::new(move || {
            check_host.socket_epoch_of(&check_peer).as_deref() == Some(socket_epoch.as_str())
        }),
    );
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_admission_baseline::LegacyViewCursor;
    use crate::compact_admission_types::{AdmissionIdentity, Answer, Produced};
    use crate::compact_recovery_state::FrozenLaneCut;
    use crate::compact_settled_messages::SettledRead;
    use crate::session_acceptance_lane::{FrozenAcceptedRoot, RetireReason, SessionAcceptanceLane};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, PoisonError};

    struct TestLane {
        lane: Mutex<SessionAcceptanceLane>,
        next_key: AtomicU64,
        gen_counter: AtomicU64,
        roots: Mutex<HashMap<usize, FrozenAcceptedRoot>>,
    }

    impl TestLane {
        fn new() -> Self {
            let mut lane = SessionAcceptanceLane::default();
            lane.bootstrap(Default::default(), Some("n1".into()));
            Self {
                lane: Mutex::new(lane),
                next_key: AtomicU64::new(1),
                gen_counter: AtomicU64::new(1),
                roots: Mutex::new(HashMap::new()),
            }
        }
    }

    impl RecoveryLane for TestLane {
        fn session_epoch(&self) -> String {
            self.lane
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .session_epoch()
                .to_owned()
        }
        fn accepted_sequence(&self) -> u64 {
            0
        }
        fn retired(&self) -> bool {
            false
        }
        fn retire_reason(&self) -> Option<RetireReason> {
            None
        }
        fn next_generation(&self) -> Option<u64> {
            Some(self.gen_counter.fetch_add(1, Ordering::SeqCst) + 1)
        }
        fn freeze_cut(&self) -> Option<FrozenLaneCut> {
            let root = self
                .lane
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .freeze();
            let key = self.next_key.fetch_add(1, Ordering::SeqCst) as usize;
            self.roots
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key, root.clone());
            Some(FrozenLaneCut {
                root_key: key,
                root,
                store: None,
                has_current: true,
            })
        }
        fn release_cut(&self, key: usize) {
            if let Some(r) = self
                .roots
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&key)
            {
                self.lane
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .release(&r);
            }
        }
        fn read_settled(
            &self,
            _: usize,
        ) -> futures::future::BoxFuture<'static, Result<SettledRead, String>> {
            Box::pin(async {
                Ok(SettledRead {
                    settled_messages: vec![],
                    owners: vec![],
                })
            })
        }
        fn read_current(
            &self,
            _: usize,
        ) -> futures::future::BoxFuture<
            'static,
            Result<Value, crate::compact_recovery_bytes::RecoveryBytesError>,
        > {
            Box::pin(async { Ok(json!({})) })
        }
        fn read_components(
            &self,
            _: usize,
        ) -> futures::future::BoxFuture<
            'static,
            Result<Value, crate::compact_recovery_bytes::RecoveryBytesError>,
        > {
            Box::pin(async { Ok(json!({})) })
        }
        fn read_active(
            &self,
            _: usize,
        ) -> futures::future::BoxFuture<
            'static,
            Result<Value, crate::compact_recovery_bytes::RecoveryBytesError>,
        > {
            Box::pin(async { Ok(json!([])) })
        }
        fn read_calls(
            &self,
            _: usize,
        ) -> futures::future::BoxFuture<
            'static,
            Result<Value, crate::compact_recovery_bytes::RecoveryBytesError>,
        > {
            Box::pin(async { Ok(json!({"calls":[]})) })
        }
    }

    struct MockResetHost {
        admissions: Arc<CompactAdmissions>,
        checkpoints: RecoveryCheckpoints,
        lane: Arc<TestLane>,
        baselines: Mutex<Vec<GeneratedFramesCursor>>,
    }

    impl ResetStreamHost for MockResetHost {
        fn current_connection(&self) -> Option<String> {
            Some("p1".into())
        }
        fn socket_epoch_of(&self, _: &str) -> Option<String> {
            Some("se-1".into())
        }
        fn admissions(&self) -> Option<Arc<CompactAdmissions>> {
            Some(self.admissions.clone())
        }
        fn recovery_checkpoints(&self) -> RecoveryCheckpoints {
            self.checkpoints.clone()
        }
        fn recovery_lane(&self, _: &str) -> Option<Arc<dyn RecoveryLane>> {
            Some(self.lane.clone())
        }
        fn is_compact_subscription(&self, _: &str, _: &str, _: &str) -> bool {
            false
        }
        fn install_admission_baseline(
            &self,
            _: &str,
            baseline: GeneratedFramesCursor,
            _: Box<dyn Fn() -> bool + Send + Sync>,
        ) {
            self.baselines
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(baseline);
        }
    }

    #[tokio::test]
    async fn refuses_unknown_admission_and_resets_valid_stream() {
        let admissions = Arc::new(CompactAdmissions::default());
        let checkpoints = RecoveryCheckpoints::default();
        let lane = Arc::new(TestLane::new());
        let host = Arc::new(MockResetHost {
            admissions: admissions.clone(),
            checkpoints: checkpoints.clone(),
            lane: lane.clone(),
            baselines: Mutex::new(Vec::new()),
        });

        let err = reset_stream(
            host.clone(),
            ResetStreamCommandInput {
                id: Some("1".into()),
                session_id: Some("s1".into()),
                admission_id: "adm-missing".into(),
            },
        )
        .await
        .expect("refused");
        assert_eq!(err["errorCode"], "recovery_unknown_token");

        // Complete an admission and freeze initial route
        let fp = "a".repeat(64);
        admissions
            .run(
                AdmissionIdentity {
                    socket_epoch: "se-1".into(),
                    peer: "p1".into(),
                    admission_id: "adm-1".into(),
                    fingerprint: fp,
                    retry: false,
                },
                |_| {
                    Box::pin(async {
                        let mut map = Answer::new();
                        map.insert("sessionId".into(), json!("s1"));
                        Ok(Produced::simple(map))
                    })
                },
            )
            .await
            .expect("admit");
        checkpoints
            .freeze(
                &RecoveryRoute {
                    socket_epoch: "se-1".into(),
                    admission_id: "adm-1".into(),
                    route_kind: RecoveryRouteKind::Ordinary,
                    peer: "p1".into(),
                    session_id: "s1".into(),
                },
                lane,
                false,
            )
            .expect("freeze");

        let ok = reset_stream(
            host.clone(),
            ResetStreamCommandInput {
                id: Some("2".into()),
                session_id: Some("s1".into()),
                admission_id: "adm-1".into(),
            },
        )
        .await;
        assert!(ok.is_none());
        let mut bl = host
            .baselines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(0);
        let reply = bl.next_frame().await.expect("reply");
        assert!(reply.contains("\"command\":\"reset_stream\""));
        assert!(reply.contains("\"generation\":3"));
        admissions.dispose().await;
    }
}
