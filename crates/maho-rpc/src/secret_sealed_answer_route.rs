//! Port of senpi `packages/coding-agent/src/modes/rpc/secret-sealed-answer-route.ts`.
//!
//! An `mcp_presenter_secret_answer` from `connection`: step 1 the shape, step 2 the sender holds
//! an eligible grant in the request's snapshot, step 3 a settled request answers from its receipt,
//! steps 4 and 5 the authority's checks; the latch then decides (steps 3 and 6 again, at the claim).

use crate::secret_input_requests::{
    SealedAnswerError, SealedAnswerReply, SealedClaim, SealedLatch,
};
use crate::secret_sealed_answers::parse_sealed_answer;
use crate::secret_sealed_holders::{SealedHolder, SealedSecretSource};
use serde_json::Value;
use std::sync::Arc;

/// What one pending or settled request offers a sealed answer: its holder snapshot and its
/// session's latch.
pub struct SealedAnswerTarget {
    pub sealed: Vec<SealedHolder>,
    pub latch: Option<Arc<dyn SealedLatch>>,
}

struct SourceClaim {
    source: Arc<dyn SealedSecretSource>,
    holder: SealedHolder,
}

impl SealedClaim for SourceClaim {
    fn current(&self) -> bool {
        self.source.eligible(&self.holder)
    }

    fn admit_jti(&self, jti: &str, exp: i64) -> bool {
        self.source.admit_jti(jti, exp)
    }
}

pub async fn answer_sealed_frame<F>(
    connection: &str,
    value: &Value,
    source: Option<&Arc<dyn SealedSecretSource>>,
    target: F,
) -> SealedAnswerReply
where
    F: FnOnce(&str) -> Option<SealedAnswerTarget>,
{
    let Some(frame) = parse_sealed_answer(value) else {
        return SealedAnswerReply::Error(SealedAnswerError::InvalidSecretResponse);
    };
    let found = target(&frame.ui_request_id);
    let (Some(source), Some(found)) = (source, found) else {
        return SealedAnswerReply::Error(SealedAnswerError::SecretAnswerRefused);
    };
    let Some(holder) = found
        .sealed
        .into_iter()
        .find(|grant| grant.id == frame.grant_id && source.connection_of(grant) == connection)
    else {
        return SealedAnswerReply::Error(SealedAnswerError::SecretAnswerRefused);
    };
    let Some(latch) = found.latch else {
        return SealedAnswerReply::Error(SealedAnswerError::SecretAnswerRefused);
    };
    if !source.eligible(&holder) {
        return SealedAnswerReply::Error(SealedAnswerError::SecretAnswerRefused);
    }
    if let Some(principal) = source.principal_of(&holder)
        && let Some(settled) = latch.settled(&frame.ui_request_id, &frame.answer_id, &principal)
    {
        return settled;
    }
    let checked = match source.answer(&holder, &frame).await {
        Ok(answer) => answer,
        Err(reply) => return reply,
    };
    let claim = SourceClaim {
        source: source.clone(),
        holder,
    };
    latch.respond(checked, &claim).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret_input_requests::{SealedAnswer, SealedAnswerDisposition};
    use crate::secret_sealed::SecretInputOutcome;
    use futures::future::BoxFuture;
    use serde_json::json;

    struct DummySource {
        eligible: bool,
    }

    impl SealedSecretSource for DummySource {
        fn holders(&self, _: &str) -> Vec<SealedHolder> {
            Vec::new()
        }
        fn current(&self, _: &SealedHolder) -> bool {
            true
        }
        fn connection_of(&self, h: &SealedHolder) -> String {
            h.connection_id.clone()
        }
        fn offer(
            &self,
            _: &str,
            _: &[SealedHolder],
            _: crate::secret_sealed_holders::SealedSecretOffer,
        ) {
        }
        fn withdraw(&self, _: &str) {}
        fn retire(&self, _: &str) {}
        fn on_change(&self, _: Arc<dyn Fn() + Send + Sync>) -> Box<dyn FnOnce() + Send> {
            Box::new(|| {})
        }
        fn eligible(&self, _: &SealedHolder) -> bool {
            self.eligible
        }
        fn principal_of(&self, h: &SealedHolder) -> Option<String> {
            Some(h.principal_id.clone())
        }
        fn answer<'a>(
            &'a self,
            h: &'a SealedHolder,
            f: &'a crate::secret_sealed_answers::SealedAnswerFrame,
        ) -> BoxFuture<'a, Result<SealedAnswer, SealedAnswerReply>> {
            Box::pin(async move {
                Ok(SealedAnswer {
                    ui_request_id: f.ui_request_id.clone(),
                    principal: h.principal_id.clone(),
                    grant_id: f.grant_id.clone(),
                    answer_id: f.answer_id.clone(),
                    req: "req".into(),
                    value: None,
                })
            })
        }
        fn settle(&self, _: &[SealedHolder], _: &str, _: SecretInputOutcome, _: Option<&str>) {}
        fn admit_jti(&self, _: &str, _: i64) -> bool {
            true
        }
    }

    struct DummyLatch;
    impl SealedLatch for DummyLatch {
        fn settled(&self, _: &str, _: &str, _: &str) -> Option<SealedAnswerReply> {
            None
        }
        fn respond<'a>(
            &'a self,
            _: SealedAnswer,
            _: &'a dyn SealedClaim,
        ) -> BoxFuture<'a, SealedAnswerReply> {
            Box::pin(async { SealedAnswerReply::Applied })
        }
    }

    #[tokio::test]
    async fn route_checks_shape_holder_eligibility_and_forwards_to_latch() {
        let holder = SealedHolder {
            id: "g1".into(),
            binding_id: 1,
            durable_session_id: "d1".into(),
            connection_id: "conn-1".into(),
            principal_id: "p1".into(),
        };
        let frame = json!({
            "type": "mcp_presenter_secret_answer",
            "id": "l1",
            "v": 1,
            "grantId": "g1",
            "uiRequestId": "u1",
            "answerId": "a1",
            "cancelled": true,
            "sig": "a".repeat(128),
        });
        let source: Arc<dyn SealedSecretSource> = Arc::new(DummySource { eligible: true });
        let reply = answer_sealed_frame("conn-1", &frame, Some(&source), |_| {
            Some(SealedAnswerTarget {
                sealed: vec![holder.clone()],
                latch: Some(Arc::new(DummyLatch)),
            })
        })
        .await;
        assert_eq!(
            reply,
            SealedAnswerReply::Disposition(SealedAnswerDisposition::Applied)
        );
        // Wrong connection refused.
        let wrong_conn = answer_sealed_frame("conn-2", &frame, Some(&source), |_| {
            Some(SealedAnswerTarget {
                sealed: vec![holder.clone()],
                latch: Some(Arc::new(DummyLatch)),
            })
        })
        .await;
        assert_eq!(
            wrong_conn,
            SealedAnswerReply::Error(SealedAnswerError::SecretAnswerRefused)
        );
    }
}
