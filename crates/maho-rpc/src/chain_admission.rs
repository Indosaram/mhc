//! Port of senpi `packages/coding-agent/src/modes/rpc/chain-admission.ts`.
//!
//! Admits RPC input that may be a thread call from another session (decision K8) against the
//! user's `threads.chainCountLimit`, read at each admission.

use maho_agent::{CHAIN_COUNT_LIMIT, admit_chain_call};
use serde_json::{Value, json};

pub const INVALID_CHAIN_COUNT: &str = "invalid_chain_count";

#[derive(Debug, Clone, PartialEq)]
pub struct ChainRefusal {
    pub message: String,
    pub error_code: &'static str,
    pub error_data: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChainAdmission {
    Admitted { chain_count: Option<u64> },
    Refused(ChainRefusal),
}

pub trait ThreadChainLimitSource {
    fn thread_chain_count_limit(&self) -> Option<u64>;
}

impl ThreadChainLimitSource for maho_core::settings_manager::SettingsManager {
    fn thread_chain_count_limit(&self) -> Option<u64> {
        self.get_thread_chain_count_limit()
    }
}

/// Admits raw JSON `chainCount` field (validated to be a positive safe integer >= 1) against the
/// settings' thread chain count limit.
pub fn admit_input_chain(
    chain_count: Option<&Value>,
    settings: &dyn ThreadChainLimitSource,
) -> ChainAdmission {
    let Some(raw) = chain_count else {
        return ChainAdmission::Admitted { chain_count: None };
    };
    let valid_u64 = raw
        .as_u64()
        .filter(|n| (1..=9_007_199_254_740_991).contains(n))
        .or_else(|| {
            raw.as_f64().and_then(|f| {
                if f.is_finite() && f.fract() == 0.0 && (1.0..=9_007_199_254_740_991.0).contains(&f)
                {
                    Some(f as u64)
                } else {
                    None
                }
            })
        });
    let Some(count) = valid_u64 else {
        return ChainAdmission::Refused(ChainRefusal {
            message: format!(
                "chainCount must be a positive integer (1 for a chain's first hop), not {raw}"
            ),
            error_code: INVALID_CHAIN_COUNT,
            error_data: json!({ "chainCount": raw }),
        });
    };
    match admit_chain_call(count, || settings.thread_chain_count_limit()) {
        Ok(admitted) => ChainAdmission::Admitted {
            chain_count: Some(admitted),
        },
        Err(err) => ChainAdmission::Refused(ChainRefusal {
            message: err.to_string(),
            error_code: CHAIN_COUNT_LIMIT,
            error_data: json!({ "count": err.count, "limit": err.limit }),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockSettings(Option<u64>);
    impl ThreadChainLimitSource for MockSettings {
        fn thread_chain_count_limit(&self) -> Option<u64> {
            self.0
        }
    }

    #[test]
    fn admits_absent_and_within_limit_and_refuses_malformed_or_excessive() {
        let settings = MockSettings(Some(3));
        assert_eq!(
            admit_input_chain(None, &settings),
            ChainAdmission::Admitted { chain_count: None }
        );
        assert_eq!(
            admit_input_chain(Some(&json!(2)), &settings),
            ChainAdmission::Admitted {
                chain_count: Some(2)
            }
        );
        match admit_input_chain(Some(&json!(0)), &settings) {
            ChainAdmission::Refused(r) => assert_eq!(r.error_code, INVALID_CHAIN_COUNT),
            other => panic!("unexpected: {other:?}"),
        }
        match admit_input_chain(Some(&json!(4)), &settings) {
            ChainAdmission::Refused(r) => assert_eq!(r.error_code, CHAIN_COUNT_LIMIT),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
