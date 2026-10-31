use crate::{
    session_continuity::{ContinuityInput, Decision},
    session_reattach::BindingStore,
    session_registry::{CreateEntry, SESSION_REGISTRY_MAX_ENTRIES, SessionEntry},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

#[derive(Default)]
pub struct SessionRegistry {
    pub entries: BTreeMap<String, SessionEntry>,
    pub bindings: BindingStore,
    pub last_decisions: BTreeMap<String, crate::session_observability::Observation>,
    pub reapers: BTreeMap<String, tokio::task::JoinHandle<()>>,
    /// senpi `onDispatchShape`: per session, whether the last attempt re-sent the whole history and that re-send's
    /// bytes/4 estimate, so a failed turn can mark a cold-seed overflow.
    pub dispatch_shapes: BTreeMap<String, DispatchShape>,
    /// `claude_sdk_oauth_checkpoint_recovered`: the rejected and the recovered boundary of the last recovery.
    pub checkpoint_recoveries: BTreeMap<String, (usize, usize)>,
    generation: u64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DispatchShape {
    pub cold_seed: bool,
    pub estimated_tokens: Option<u64>,
}
pub struct ResidentInput<'a> {
    pub session: &'a str,
    pub account: &'a str,
    pub model: &'a str,
    pub context: &'a Value,
    pub options: &'a Value,
    pub executable: &'a Path,
    pub environment: &'a BTreeMap<String, String>,
    pub auth_lane: &'a str,
    pub custom: &'a BTreeMap<String, String>,
    pub tool_note: Option<&'a str>,
    pub now: u64,
    pub transcript_available: bool,
    pub signal: Option<&'a maho_ai::utils::abort::AbortSignal>,
    /// Digest of the access token this attempt authenticates with: a resident process spawned with another token
    /// must not serve the turn (a refresh revokes the previous token).
    pub credential_digest: Option<&'a str>,
    /// The model's window; a cold-seed re-send that cannot fit it is refused before dispatch. 0 disables the check.
    pub context_window: u64,
}
impl SessionRegistry {
    pub async fn close(&mut self, session: &str) -> anyhow::Result<()> {
        if let Some(reaper) = self.reapers.remove(session) {
            reaper.abort();
            let _ = reaper.await;
        }
        if let Some(mut entry) = self.entries.remove(session) {
            self.bindings.remember(
                session,
                &crate::session_turn_attempt::binding_from_entry(&entry, &entry.sent_hashes),
            );
            entry.close().await?;
        }
        Ok(())
    }
    pub async fn close_all(&mut self) -> anyhow::Result<()> {
        let sessions: Vec<_> = self.entries.keys().cloned().collect();
        for session in sessions {
            self.close(&session).await?;
        }
        Ok(())
    }
    pub async fn turn(
        &mut self,
        input: ResidentInput<'_>,
        deliver: impl FnMut(Value),
    ) -> anyhow::Result<crate::session_registry_pump::TurnResult> {
        // senpi resets the dispatch shape per stream call: a turn that fails before dispatch has none.
        self.dispatch_shapes.remove(input.session);
        if let Some(reaper) = self.reapers.remove(input.session) {
            reaper.abort();
            let _ = reaper.await;
        }
        let messages = crate::session_sync::sent_messages(input.context);
        let hashes = crate::session_sync::sent_message_hashes(&messages);
        let fingerprint = crate::session_sync::config_fingerprint(
            input.options,
            input.context,
            input.auth_lane,
            input.account,
        );
        let snapshot = self.entries.get(input.session).map(|entry| {
            crate::session_turn_attempt::binding_from_entry(entry, &entry.sent_hashes)
        });
        let pending_diverged_path = self
            .entries
            .get(input.session)
            .and_then(|entry| entry.pending_fork_diverged_path.clone());
        let admission = crate::session_restored_admission::admit_restored_binding(
            &mut self.bindings,
            input.session,
            input.options["cwd"].as_str().map(std::path::Path::new),
            input.auth_lane,
            |binding, cwd, lane| {
                let cwd = cwd.to_owned();
                let lane = lane.to_owned();
                async move {
                    if lane == "config-dir" {
                        return Ok(false);
                    }
                    let messages = crate::sdk_boundary::get_session_messages(
                        &binding.sdk_session_id,
                        &cwd,
                        input.environment,
                    )
                    .await?;
                    Ok(crate::session_reattach::verify_transcript(
                        &binding, &messages, &lane,
                    ))
                }
            },
        )
        .await?;
        // senpi `servedLineage`: the binding this process held for the session before this attempt.
        let retained_binding = self.bindings.get(input.session);
        let binding = admission.binding;
        let lineage_seat = snapshot
            .as_ref()
            .or(retained_binding.as_ref())
            .or(binding.as_ref())
            .map(|source| source.account_name.as_str());
        let seat_changed = lineage_seat.map(|previous| previous != input.account);
        let seat = crate::seat_digest::seat_digest(input.account, None);
        let decision = crate::session_continuity::decide(&ContinuityInput {
            entry: snapshot.as_ref(),
            binding: binding.as_ref(),
            current_hashes: &hashes,
            account_name: input.account,
            model_id: input.model,
            system_prompt_hash: &fingerprint.system_prompt_hash,
            toolset_hash: &fingerprint.toolset_hash,
            transcript_available: input.transcript_available && admission.transcript_available,
            cross_account_resume_supported: input.auth_lane != "config-dir",
            idle_expired: self
                .entries
                .get(input.session)
                .is_some_and(|entry| entry.idle_expired(input.now)),
            invalidation_reason: self.bindings.invalidation_reason(input.session),
            credential_digest: input.credential_digest,
        });
        let lineage_source = snapshot.clone().or_else(|| binding.clone());
        let (mut from, resume, sdk_id) = match &decision {
            Decision::Delta { from } => (
                *from,
                None,
                snapshot
                    .as_ref()
                    .expect("live entry")
                    .sdk_session_id
                    .clone(),
            ),
            Decision::Reattach {
                from,
                sdk_session_id,
                ..
            } => (*from, Some(None), sdk_session_id.clone()),
            Decision::Fork {
                from,
                sdk_session_id,
                at_uuid,
                ..
            } => (*from, Some(Some(at_uuid.as_str())), sdk_session_id.clone()),
            _ => (0, None, maho_ai::utils::uuid::uuidv7(None)?),
        };
        if !matches!(decision, Decision::Delta { .. }) {
            self.close(input.session).await?;
            if self.entries.len() >= SESSION_REGISTRY_MAX_ENTRIES {
                let oldest = self
                    .entries
                    .iter()
                    .filter(|(_, entry)| entry.evictable())
                    .min_by_key(|(_, entry)| entry.last_used_at)
                    .map(|(session, _)| session.clone())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Anthropic Subscription registry capacity has no idle entry"
                        )
                    })?;
                self.close(&oldest).await?;
            }
            self.generation += 1;
            let generation = self.generation;
            let recovered = std::sync::Mutex::new(None);
            let recovered = &recovered;
            let create = async {
                let Some(resume_at) = resume else {
                    return SessionEntry::create(CreateEntry {
                        session_id: input.session,
                        sdk_session_id: &sdk_id,
                        account_name: input.account,
                        model_id: input.model,
                        system_prompt_hash: &fingerprint.system_prompt_hash,
                        toolset_hash: &fingerprint.toolset_hash,
                        executable: input.executable,
                        options: input.options,
                        environment: input.environment,
                        generation,
                        now: input.now,
                        resume_at: None,
                    })
                    .await
                    .map(|entry| (entry, from));
                };
                // senpi `reattachRecoveringCheckpoint`: a fork point Claude Code reports missing from its transcript
                // forks at the next earlier verified boundary instead of re-sending the whole conversation.
                let mut lineage = lineage_source.clone().unwrap_or_default();
                lineage.sdk_session_id = sdk_id.clone();
                lineage.sent_count = from;
                lineage.sent_hashes = hashes[..from.min(hashes.len())].to_vec();
                lineage
                    .assistant_uuid_by_index
                    .retain(|index, _| *index <= from);
                if let Some(at) = resume_at {
                    lineage.last_assistant_uuid = Some(at.to_owned());
                }
                let aborted = || {
                    input
                        .signal
                        .is_some_and(maho_ai::utils::abort::AbortSignal::aborted)
                };
                crate::session_checkpoint_recovery::reattach_recovering_checkpoint(
                    crate::session_checkpoint_recovery::RecoveringReattachInput {
                        binding: lineage,
                        at_uuid: resume_at.map(str::to_owned),
                        cwd: input.options["cwd"].as_str(),
                        aborted: &aborted,
                        current_hashes: &hashes,
                        auth_lane: input.auth_lane,
                    },
                    |binding, at_uuid| {
                        let sdk_id = binding.sdk_session_id;
                        let (session, account, model, executable, options, environment) = (
                            input.session,
                            input.account,
                            input.model,
                            input.executable,
                            input.options,
                            input.environment,
                        );
                        let (system_prompt_hash, toolset_hash) =
                            (&fingerprint.system_prompt_hash, &fingerprint.toolset_hash);
                        let now = input.now;
                        async move {
                            SessionEntry::create(CreateEntry {
                                session_id: session,
                                sdk_session_id: &sdk_id,
                                account_name: account,
                                model_id: model,
                                system_prompt_hash,
                                toolset_hash,
                                executable,
                                options,
                                environment,
                                generation,
                                now,
                                resume_at: Some(at_uuid.as_deref()),
                            })
                            .await
                        }
                    },
                    |sdk_session_id, cwd| {
                        let environment = input.environment;
                        async move {
                            crate::sdk_boundary::get_session_messages(
                                &sdk_session_id,
                                Path::new(&cwd),
                                environment,
                            )
                            .await
                        }
                    },
                    |rejected, recovered_at| {
                        *recovered
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            Some((rejected, recovered_at))
                    },
                )
                .await
            };
            let created = match input.signal {
                Some(signal) => maho_ai::utils::abort::race_with_abort_signal(create, signal)
                    .await
                    .map_err(anyhow::Error::from)
                    .and_then(|result| result),
                None => create.await,
            };
            if let Some(recovery) = recovered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                self.checkpoint_recoveries
                    .insert(input.session.into(), recovery);
            }
            let mut entry = match created {
                Ok((entry, recovered_from)) => {
                    from = recovered_from;
                    entry
                }
                Err(error) => {
                    self.last_decisions.insert(
                        input.session.into(),
                        crate::session_observability::Observation {
                            kind: "bootstrap".into(),
                            reason: if input
                                .signal
                                .is_some_and(maho_ai::utils::abort::AbortSignal::aborted)
                            {
                                "resume_initialization_aborted"
                            } else {
                                "resume_initialization_failed"
                            }
                            .into(),
                            delta_messages: hashes.len() - from,
                            payload_bytes: None,
                            collapsed_directives: None,
                            session_id: Some(input.session.into()),
                            settings_source: None,
                            seat: Some(seat.clone()),
                            seat_changed,
                            diverged_path: None,
                        },
                    );
                    return Err(error);
                }
            };
            if let Some(source) = lineage_source.as_ref() {
                entry.assistant_uuid_by_index = source
                    .assistant_uuid_by_index
                    .iter()
                    .filter(|(index, _)| **index <= from)
                    .map(|(index, uuid)| (*index, uuid.clone()))
                    .collect();
                entry.sent_hashes = hashes[..from].to_vec();
            }
            self.entries.insert(input.session.into(), entry);
        }
        // Every branch above either reuses a process whose token matched (delta) or spawned one with this attempt's
        // options, so the entry now runs on this token.
        if let Some(entry) = self.entries.get_mut(input.session) {
            entry.credential_digest = input.credential_digest.map(str::to_owned);
        }
        let cold = matches!(
            decision,
            Decision::Bootstrap { .. } | Decision::Flatten { .. }
        );
        let blocks = if cold {
            let blocks = crate::prompt_bridge::build_prompt_blocks(
                input.context["messages"].as_array().expect("messages"),
                input.custom,
                input.tool_note,
            );
            crate::prompt_directive_dedupe::dedupe_registered_directive_blocks(&blocks).blocks
        } else {
            crate::session_sync::build_delta_prompt_blocks(&messages[from..], input.custom)
        };
        let estimated_tokens = cold.then(|| {
            crate::cold_seed_budget::estimate_cold_seed_tokens(
                input.context["systemPrompt"].as_str(),
                input.context.get("tools").filter(|tools| !tools.is_null()),
                &blocks,
            )
        });
        self.dispatch_shapes.insert(
            input.session.into(),
            DispatchShape {
                cold_seed: cold,
                estimated_tokens,
            },
        );
        if let Some(overflow) = estimated_tokens.and_then(|estimated| {
            crate::cold_seed_budget::cold_seed_overflow(
                input.context_window,
                estimated,
                crate::cold_seed_budget::cold_seed_calibration(Some(input.session)),
            )
        }) {
            // Never dispatch a re-send that cannot fit: the SDK cannot compact a single exchange, so the overflow
            // must reach senpi's own compaction instead.
            self.close(input.session).await?;
            self.last_decisions.insert(
                input.session.into(),
                crate::session_observability::Observation {
                    kind: "failed".into(),
                    reason: crate::session_continuity::sanitize_reason(&overflow.to_string()),
                    delta_messages: hashes.len(),
                    payload_bytes: None,
                    collapsed_directives: None,
                    session_id: Some(input.session.into()),
                    settings_source: None,
                    seat: Some(seat.clone()),
                    seat_changed,
                    diverged_path: None,
                },
            );
            return Err(overflow.into());
        }
        let entry = self.entries.get_mut(input.session).expect("admitted entry");
        let result = crate::session_turn_attempt::run(
            entry,
            maho_ai::utils::uuid::uuidv7(None)?,
            json!({"role":"user","content":blocks}),
            &hashes,
            (input.now, input.signal),
            &mut self.bindings,
            deliver,
        )
        .await;
        let (kind, reason, delta) = match &decision {
            Decision::Delta { from } => ("delta", "prefix_matched", hashes.len() - from),
            Decision::Reattach { from, reason, .. } | Decision::Fork { from, reason, .. } => {
                ("fork", reason.as_str(), hashes.len() - from)
            }
            Decision::Bootstrap { reason } => (
                "bootstrap",
                reason.as_deref().unwrap_or("registry_miss"),
                hashes.len(),
            ),
            Decision::Flatten { reason } => ("flatten", reason.as_str(), hashes.len()),
        };
        // senpi#1975: an assistant_rewritten fork/flatten names the path the commit boundary recorded.
        let diverged_path = (result.is_ok() && kind != "delta" && reason == "assistant_rewritten")
            .then_some(pending_diverged_path)
            .flatten();
        self.last_decisions.insert(
            input.session.into(),
            crate::session_observability::Observation {
                // Every attempt failed: the terminal observation is `failed`, not `flatten`; nothing was re-sent and the
                // retry checkpoint still resumes the lineage.
                kind: if result.is_err() { "failed" } else { kind }.into(),
                reason: result.as_ref().err().map_or_else(
                    || reason.into(),
                    |error| crate::session_continuity::sanitize_reason(&error.to_string()),
                ),
                delta_messages: delta,
                payload_bytes: cold
                    .then(|| crate::prompt_directive_dedupe::serialized_payload_bytes(&blocks)),
                collapsed_directives: None,
                session_id: Some(input.session.into()),
                settings_source: None,
                seat: Some(seat.clone()),
                seat_changed,
                diverged_path,
            },
        );
        if result.is_err()
            || self.entries.get(input.session).is_some_and(|entry| {
                entry.pump.state == crate::session_registry_state::SessionState::Closed
            })
        {
            let checkpoint = self.entries.get(input.session).map(|entry| {
                let mut binding =
                    crate::session_turn_attempt::binding_from_entry(entry, &entry.sent_hashes);
                binding.unanswered_turn_digest = Some(
                    crate::session_sync::sent_hash_prefix_digest(&hashes, hashes.len()),
                );
                binding
            });
            if let Some(mut entry) = self.entries.remove(input.session) {
                entry.close().await?;
            }
            let failure = result.as_ref().err().map(ToString::to_string);
            if failure.as_ref().is_some_and(|error| {
                error
                    .to_lowercase()
                    .contains("no conversation found with session id")
            }) {
                self.bindings.forget(input.session);
            } else if let Some(checkpoint) = checkpoint {
                // The attempt's process is gone. An unanswered attempt keeps the served lineage when that names another
                // account (senpi#4304), with a resume uuid Claude Code reported missing dropped from both (senpi#1958).
                let answered = result
                    .as_ref()
                    .is_ok_and(crate::session_turn_attempt::successful_turn);
                let binding = if answered {
                    checkpoint
                } else {
                    let drop_missing = |binding| match &failure {
                        Some(error) => crate::session_turn_attempt::without_missing_assistant_uuid(
                            binding, error,
                        ),
                        None => binding,
                    };
                    crate::session_turn_attempt::unanswered_attempt_binding(
                        drop_missing(checkpoint),
                        retained_binding.clone().map(drop_missing),
                    )
                };
                self.bindings.remember(input.session, &binding);
            }
        }
        result
    }
}
impl Drop for SessionRegistry {
    fn drop(&mut self) {
        for reaper in self.reapers.values() {
            reaper.abort();
        }
    }
}
