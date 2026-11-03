//! Port of senpi `anthropic-subscription/session-checkpoint-recovery.ts` (pin 903b8336).
//!
//! deviation: upstream reaches the SDK boundary, `reattachSession` and the continuity logger through
//! module singletons. The native registry has no such singletons, so the transcript reader, the
//! reattach step and the recovery observer are passed in; the decision logic is unchanged.
use crate::session_continuity::Snapshot;
use regex::Regex;
use serde_json::Value;
use std::future::Future;
use std::sync::OnceLock;

/// Each recovery spawns a Claude Code subprocess; a lineage this damaged cold-seeds instead.
pub const MAX_CHECKPOINT_RECOVERIES: usize = 3;

/// `RESUME_MESSAGE_MISSING` (session-turn-attempt.ts): Claude Code rejected the fork point.
pub fn resume_message_missing() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"No message found with message\.uuid").expect("resume missing pattern")
    })
}

pub struct CheckpointLookup<'a> {
    pub binding: &'a Snapshot,
    pub current_hashes: &'a [String],
    pub cwd: Option<&'a str>,
    pub auth_lane: &'a str,
}

/// The newest mapped boundary strictly before `binding.sent_count` that lies inside the hash-proven
/// prefix AND exists in the SDK transcript of the same session as a top-level assistant. The
/// config-dir lane and an unreadable or foreign transcript fail closed (senpi#1973).
pub async fn earlier_verified_checkpoint<F, Fut>(
    input: CheckpointLookup<'_>,
    get_session_messages: F,
) -> Option<Snapshot>
where
    F: FnOnce(String, String) -> Fut,
    Fut: Future<Output = anyhow::Result<Vec<Value>>>,
{
    let CheckpointLookup {
        binding,
        current_hashes,
        cwd,
        auth_lane,
    } = input;
    let cwd = cwd?;
    if auth_lane == "config-dir" {
        return None;
    }
    let mut candidates: Vec<(usize, &String)> = binding
        .assistant_uuid_by_index
        .iter()
        .map(|(index, uuid)| (*index, uuid))
        .filter(|(index, _)| {
            *index >= 1
                && *index < binding.sent_count
                && *index <= binding.sent_hashes.len()
                && *index <= current_hashes.len()
                && binding.sent_hashes[..*index] == current_hashes[..*index]
        })
        .collect();
    candidates.sort_by_key(|right| std::cmp::Reverse(right.0));
    if candidates.is_empty() {
        return None;
    }
    let messages = get_session_messages(binding.sdk_session_id.clone(), cwd.to_owned())
        .await
        .ok()?;
    if messages
        .iter()
        .any(|message| message["session_id"] != binding.sdk_session_id.as_str())
    {
        return None;
    }
    let available: Vec<&str> = messages
        .iter()
        .filter(|message| {
            message["type"] == "assistant"
                && message
                    .get("parent_tool_use_id")
                    .is_some_and(Value::is_null)
        })
        .filter_map(|message| message["uuid"].as_str())
        .collect();
    let (sent_count, last_assistant_uuid) = candidates
        .into_iter()
        .find(|(_, uuid)| available.contains(&uuid.as_str()))?;
    let mut earlier = binding.clone();
    earlier.sent_count = sent_count;
    earlier.sent_hashes = current_hashes[..sent_count].to_vec();
    earlier.last_assistant_uuid = Some(last_assistant_uuid.clone());
    earlier
        .assistant_uuid_by_index
        .retain(|index, _| *index <= sent_count);
    Some(earlier)
}

pub struct RecoveringReattachInput<'a> {
    pub binding: Snapshot,
    pub at_uuid: Option<String>,
    pub cwd: Option<&'a str>,
    pub aborted: &'a (dyn Fn() -> bool + Send + Sync),
    pub current_hashes: &'a [String],
    pub auth_lane: &'a str,
}

/// Reattaches at the decided boundary. When Claude Code rejects the fork point as absent from its
/// transcript, forks at the next earlier verified boundary instead of re-sending the whole
/// conversation; every retry strictly lowers the boundary index. Any other failure, an abort, no
/// verified candidate, or the attempt cap rethrows. Returns the entry and the boundary it forked from.
/// `on_recovered(rejected_index, recovered_index)` reports `claude_sdk_oauth_checkpoint_recovered`.
pub async fn reattach_recovering_checkpoint<E, R, RFut, M, MFut, O>(
    input: RecoveringReattachInput<'_>,
    mut reattach: R,
    get_session_messages: M,
    mut on_recovered: O,
) -> anyhow::Result<(E, usize)>
where
    O: FnMut(usize, usize),
    R: FnMut(Snapshot, Option<String>) -> RFut,
    RFut: Future<Output = anyhow::Result<E>>,
    M: Fn(String, String) -> MFut,
    MFut: Future<Output = anyhow::Result<Vec<Value>>>,
{
    let mut binding = input.binding;
    let mut at_uuid = input.at_uuid;
    let mut recovered = 0;
    loop {
        match reattach(binding.clone(), at_uuid.clone()).await {
            Ok(entry) => return Ok((entry, binding.sent_count)),
            Err(error) => {
                if (input.aborted)() || recovered >= MAX_CHECKPOINT_RECOVERIES {
                    return Err(error);
                }
                if !resume_message_missing().is_match(&error.to_string()) {
                    return Err(error);
                }
                let earlier = earlier_verified_checkpoint(
                    CheckpointLookup {
                        binding: &binding,
                        current_hashes: input.current_hashes,
                        cwd: input.cwd,
                        auth_lane: input.auth_lane,
                    },
                    &get_session_messages,
                )
                .await;
                let Some(earlier) = earlier.filter(|earlier| earlier.last_assistant_uuid.is_some())
                else {
                    return Err(error);
                };
                on_recovered(binding.sent_count, earlier.sent_count);
                at_uuid = earlier.last_assistant_uuid.clone();
                binding = earlier;
                recovered += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;

    fn hashes(count: usize) -> Vec<String> {
        (1..=count).map(|index| format!("h{index}")).collect()
    }

    fn binding() -> Snapshot {
        Snapshot {
            sdk_session_id: "sdk".into(),
            sent_count: 4,
            sent_hashes: hashes(4),
            last_assistant_uuid: Some("a3".into()),
            assistant_uuid_by_index: [(1, "a1".into()), (2, "a2".into()), (3, "a3".into())].into(),
            ..Default::default()
        }
    }

    fn assistant(uuid: &str, parent: Value) -> Value {
        json!({"type":"assistant","uuid":uuid,"session_id":"sdk","parent_tool_use_id":parent})
    }

    async fn lookup(
        binding: &Snapshot,
        current: &[String],
        lane: &str,
        transcript: Vec<Value>,
    ) -> Option<Snapshot> {
        earlier_verified_checkpoint(
            CheckpointLookup {
                binding,
                current_hashes: current,
                cwd: Some("/work"),
                auth_lane: lane,
            },
            |_, _| async move { Ok::<_, anyhow::Error>(transcript) },
        )
        .await
    }

    #[tokio::test]
    async fn picks_newest_mapped_boundary_present_in_the_transcript() {
        let binding = binding();
        let earlier = lookup(
            &binding,
            &hashes(4),
            "oauth-slots",
            vec![assistant("a1", Value::Null), assistant("a2", Value::Null)],
        )
        .await
        .unwrap();
        assert_eq!(earlier.sent_count, 2);
        assert_eq!(earlier.last_assistant_uuid.as_deref(), Some("a2"));
        assert_eq!(earlier.sent_hashes, hashes(2));
        assert_eq!(
            earlier
                .assistant_uuid_by_index
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            [1, 2]
        );
    }

    #[tokio::test]
    async fn fails_closed_for_foreign_subagent_config_dir_and_drifted_prefix() {
        let binding = binding();
        let mut foreign = assistant("a2", Value::Null);
        foreign["session_id"] = json!("other");
        assert!(
            lookup(
                &binding,
                &hashes(4),
                "oauth-slots",
                vec![assistant("a1", Value::Null), foreign]
            )
            .await
            .is_none()
        );
        assert!(
            lookup(
                &binding,
                &hashes(4),
                "oauth-slots",
                vec![assistant("a2", json!("tool"))]
            )
            .await
            .is_none()
        );
        assert!(
            lookup(
                &binding,
                &hashes(4),
                "config-dir",
                vec![assistant("a2", Value::Null)]
            )
            .await
            .is_none()
        );
        let mut drifted = hashes(4);
        drifted[0] = "changed".into();
        assert!(
            lookup(
                &binding,
                &drifted,
                "oauth-slots",
                vec![assistant("a2", Value::Null)]
            )
            .await
            .is_none()
        );
        let unreadable = earlier_verified_checkpoint(
            CheckpointLookup {
                binding: &binding,
                current_hashes: &hashes(4),
                cwd: Some("/w"),
                auth_lane: "ambient",
            },
            |_, _| async { Err::<Vec<Value>, _>(anyhow::anyhow!("unreadable")) },
        )
        .await;
        assert!(unreadable.is_none());
        let no_cwd = earlier_verified_checkpoint(
            CheckpointLookup {
                binding: &binding,
                current_hashes: &hashes(4),
                cwd: None,
                auth_lane: "ambient",
            },
            |_, _| async { Ok::<_, anyhow::Error>(vec![assistant("a2", Value::Null)]) },
        )
        .await;
        assert!(no_cwd.is_none());
    }

    #[tokio::test]
    async fn reattach_recovers_at_earlier_boundary_after_missing_fork_point() {
        let attempts = RefCell::new(Vec::new());
        let recovered = RefCell::new(Vec::new());
        let current = hashes(4);
        let (entry, from) = reattach_recovering_checkpoint(
            RecoveringReattachInput {
                binding: binding(),
                at_uuid: Some("a3".into()),
                cwd: Some("/work"),
                aborted: &|| false,
                current_hashes: &current,
                auth_lane: "oauth-slots",
            },
            |binding: Snapshot, at: Option<String>| {
                attempts.borrow_mut().push(at.clone());
                async move {
                    if at.as_deref() == Some("a3") {
                        anyhow::bail!("No message found with message.uuid of: a3");
                    }
                    Ok(binding.sent_count)
                }
            },
            |_, _| async {
                Ok(vec![
                    assistant("a1", Value::Null),
                    assistant("a2", Value::Null),
                ])
            },
            |rejected, recovered_index| recovered.borrow_mut().push((rejected, recovered_index)),
        )
        .await
        .unwrap();
        assert_eq!((entry, from), (2, 2));
        assert_eq!(
            *attempts.borrow(),
            [Some("a3".to_string()), Some("a2".to_string())]
        );
        assert_eq!(*recovered.borrow(), [(4, 2)]);
    }

    #[tokio::test]
    async fn other_failures_aborts_and_missing_candidates_rethrow() {
        let current = hashes(4);
        let run = |message: &'static str, aborted: bool| {
            let current = current.clone();
            async move {
                let aborted_fn = move || aborted;
                reattach_recovering_checkpoint(
                    RecoveringReattachInput {
                        binding: binding(),
                        at_uuid: Some("a3".into()),
                        cwd: Some("/work"),
                        aborted: &aborted_fn,
                        current_hashes: &current,
                        auth_lane: "oauth-slots",
                    },
                    |_, _| async move { Err::<usize, anyhow::Error>(anyhow::anyhow!(message)) },
                    |_, _| async { Ok::<_, anyhow::Error>(vec![assistant("a2", Value::Null)]) },
                    |_, _| {},
                )
                .await
                .unwrap_err()
                .to_string()
            }
        };
        assert_eq!(run("spawn failed", false).await, "spawn failed");
        assert_eq!(
            run("No message found with message.uuid of: x", true).await,
            "No message found with message.uuid of: x"
        );
        // every boundary rejected: recovery stops once no earlier verified candidate remains
        assert_eq!(
            run("No message found with message.uuid of: y", false).await,
            "No message found with message.uuid of: y"
        );
    }
}
