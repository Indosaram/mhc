use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RpcSessionLaunchProfile {
    pub runtime: maho_core::agent_session_runtime::AgentSessionLaunchProfile,
    pub session_path: Option<String>,
    pub durable_session_id: Option<String>,
    pub session_kind: Option<maho_ext_api::SessionKind>,
    pub session_context: Option<BTreeMap<String, String>>,
    /// `Some(false)` only: an observational open whose attach never releases an existing hold (senpi `attended?: false`).
    pub attended: Option<bool>,
    /// `terminal`, `app` or `chat`: where this session's replies render (senpi `promptSurface`).
    pub prompt_surface: Option<String>,
    /// `connected`, `builtin` or `none`: which browser this session's skills drive (senpi `browserEngine`).
    pub browser_engine: Option<String>,
    /// This session's own `{ modelFallback, fallbackChains }` policy (senpi `retryFallback`).
    pub retry_fallback: Option<serde_json::Value>,
    /// The host endpoint this session's holder record names (senpi `holderEndpoint`).
    pub holder_endpoint: Option<maho_core::session_holders::SessionHolderEndpoint>,
    /// The validated ancestry of a child session, merged into the launch profile (senpi `spawnedBy`) for the
    /// runtime factory to write into the new session's header.
    pub spawned_by: Option<maho_core::session_lifecycle_types::SpawnedBy>,
    /// The `session_start` reason the open's runtime starts with (senpi `buildOpeningEntry` `sessionStartEvent`):
    /// `Resume` when the open names an existing file, `Startup` for an unattended create, `None` otherwise.
    pub session_start_reason: Option<maho_ext_api::SessionReason>,
}
pub struct SessionIdentity {
    pub kind: maho_ext_api::SessionKind,
    pub context: Arc<BTreeMap<String, String>>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcSessionState {
    Opening,
    Open,
    Closing,
    Quarantined,
    Closed,
}
pub struct SessionCloseState {
    pub state: RpcSessionState,
    pub attachments: i64,
    pub retain_on_disconnect: bool,
    pub reservation_key: Option<String>,
    pub writing: bool,
}
pub struct DurableSessionIdentity {
    pub state: RpcSessionState,
    pub durable_session_id: Option<String>,
    pub reservation_key: Option<String>,
}
pub struct RpcSessionEntry {
    pub close: SessionCloseState,
    pub profile: RpcSessionLaunchProfile,
    pub identity: SessionIdentity,
    pub scope: maho_ai::node::provider_scope::ProviderScope,
    pub runtime: Option<Arc<maho_core::agent_session_runtime::AgentSessionRuntime>>,
    pub durable_session_id: Option<String>,
    pub session_path: Option<String>,
    pub last_command_at: f64,
    /// Patches `set_session_context` applied over the frozen open-time context (senpi `contextOverlay`).
    pub context_overlay: Option<crate::session_context_patch::SessionContextOverlay>,
    /// The in-process session's ordered acceptance lane, present only while the compact-updates flag is on.
    pub acceptance: Option<SharedAcceptanceLane>,
}
pub type SharedAcceptanceLane =
    Arc<std::sync::Mutex<crate::session_acceptance_lane::SessionAcceptanceLane>>;
#[derive(Default)]
pub struct RpcSessionRegistry {
    entries: BTreeMap<String, RpcSessionEntry>,
    /// Session paths an entry holds, by canonical key (senpi `reservations`).
    reservations: std::collections::HashSet<String>,
    /// Opens between their reservation and their entry (senpi `pendingClaims`). The native claim store is
    /// synchronous, so an open settles its pending claim before `admit_open` returns.
    pending_claims: crate::session_registry_claims::PendingClaims,
    /// The cross-generation claims this host publishes (senpi `pathReservations`), when it has a daemon directory.
    path_reservations: Option<crate::host_reservations::BoundPathReservations>,
    /// Each still-opening entry's open completion and its settle half (senpi `openCompletion`).
    open_completions: BTreeMap<
        String,
        (
            crate::session_registry_opening::SettleOpen,
            crate::session_registry_opening::OpenCompletion,
        ),
    >,
    /// What each admitted open's runtime is built from (a resume drops the create-only choices).
    runtime_profiles: BTreeMap<String, RpcSessionLaunchProfile>,
    next_handle: u64,
    worker_refusal: Option<(u64, u64)>,
    /// A session worker's own registry: its host owns the acceptance lane, so the isolate opens none.
    pub worker_isolate: bool,
    /// The compact-updates flag as this registry sees it; `None` reads the process environment.
    pub compact_updates: Option<bool>,
    /// senpi `onSizeChange`: told the entry count after every open admits or a teardown removes an entry.
    pub on_size_change: Option<SizeChangeHook>,
}
/// senpi `RpcSessionRegistryOptions.onSizeChange`.
pub type SizeChangeHook = Arc<dyn Fn(usize) + Send + Sync>;
pub enum OpenAdmission {
    Create(String),
    Attached(String),
    /// The path is still opening: wait on the in-flight open
    /// ([`crate::session_registry_attach::wait_open_completion`]) with no registry lock held, then admit again.
    Wait {
        completion: crate::session_registry_attach::OpenCompletionHandle,
        session_path: String,
    },
}
/// senpi `ClientAdmissions.sealQueuedOnReplace` + `EffectLedger.sealOnReplace` for the ledgers published under
/// `route`: the before-replace listener a host session (and the stdio session) installs.
pub fn seal_ledgers_on_replace(route: String) -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(move || {
        crate::client_admissions::ClientAdmissions::seal_queued_on_replace(&route);
        crate::effect_ledger::EffectLedger::seal_on_replace(&route);
    })
}
impl RpcSessionRegistry {
    pub fn size(&self) -> usize {
        self.entries.len()
    }
    fn size_changed(&self) {
        if let Some(hook) = &self.on_size_change {
            hook(self.entries.len());
        }
    }
    pub fn get(&self, handle: &str) -> Option<&RpcSessionEntry> {
        self.entries.get(handle)
    }
    pub fn get_mut(&mut self, handle: &str) -> Option<&mut RpcSessionEntry> {
        self.entries.get_mut(handle)
    }
    pub fn get_for_command(
        &mut self,
        handle: &str,
        command: &str,
        now: f64,
    ) -> Result<&mut RpcSessionEntry, RpcSessionRegistryError> {
        let entry = self
            .entries
            .get_mut(handle)
            .ok_or_else(|| RpcSessionRegistryError::new("unknown_session", None, None))?;
        if entry.close.state == RpcSessionState::Closing
            && !matches!(
                command,
                "abort"
                    | "abort_bash"
                    | "extension_ui_response"
                    | "extension_ui_progress"
                    | "host_response"
            )
        {
            return Err(RpcSessionRegistryError::new("session_closing", None, None));
        }
        if !matches!(
            entry.close.state,
            RpcSessionState::Open | RpcSessionState::Closing
        ) {
            return Err(RpcSessionRegistryError::new("unknown_session", None, None));
        }
        // Polling a session nobody holds is observation, not work that needs its runtime; an attached
        // client's polling keeps its session alive exactly as before.
        if entry.close.attachments > 0
            || crate::session_command_activity::refreshes_session_activity(command)
        {
            entry.last_command_at = now;
        }
        Ok(entry)
    }
    /// senpi `attachSession` (`attachLiveSession`): a synchronous refcount change on an open entry; never opens a
    /// path and never applies a launch profile. Returns the runtime when the attach ended a retained session's
    /// parked window, so the caller emits `session_resumed` through it.
    pub fn attach_session(
        &mut self,
        handle: &str,
        now: f64,
    ) -> Result<
        Option<Arc<maho_core::agent_session_runtime::AgentSessionRuntime>>,
        RpcSessionRegistryError,
    > {
        let entry = self
            .entries
            .get_mut(handle)
            .filter(|entry| entry.close.state == RpcSessionState::Open)
            .ok_or_else(|| RpcSessionRegistryError::new("unknown_session", None, None))?;
        let was_parked = entry.close.retain_on_disconnect && entry.close.attachments == 0;
        entry.close.attachments += 1;
        entry.last_command_at = now;
        // senpi `markPathAttached`: a path this host serves a client on is never reclaimable from it.
        if let Some(key) = &entry.close.reservation_key
            && let Some(claims) = self.path_reservations.as_mut()
        {
            claims.set_attached(key, true);
        }
        Ok(if was_parked {
            entry.runtime.clone()
        } else {
            None
        })
    }
    pub fn set_worker_admission(&mut self, refusal: Option<(u64, u64)>) {
        self.worker_refusal = refusal;
    }
    pub async fn dispatch_command(
        &mut self,
        handle: &str,
        command: &crate::rpc_types::RpcCommand,
        kind: &str,
        now: f64,
        activity: &crate::session_attribution::SessionActivityRegistry,
    ) -> Result<Option<crate::rpc_types::RpcResponse>, RpcSessionRegistryError> {
        let entry = self.get_for_command(handle, kind, now)?;
        let runtime = entry
            .runtime
            .as_ref()
            .ok_or_else(|| RpcSessionRegistryError::new("unknown_session", None, None))?;
        crate::session_attribution::run_with_session_attribution(
            activity,
            crate::session_attribution::SessionAttribution {
                session_id: Some(handle.into()),
                tool: None,
                job: None,
            },
            async {
                maho_ai::node::provider_scope::run_with_provider_scope_async(
                    &entry.scope,
                    crate::connection_handler::handle_session_command(runtime.session(), command),
                )
                .await
                .map_err(|error| {
                    RpcSessionRegistryError::new("session_closed", Some(&error.to_string()), None)
                })
                .map(|response| {
                    response.map(|mut response| {
                        response.session_id = Some(handle.into());
                        response
                    })
                })
            },
        )
        .await
    }
    /// The claims this host publishes for its sessions' files (senpi `createEndpointReservations`).
    pub fn set_path_reservations(
        &mut self,
        reservations: Option<crate::host_reservations::BoundPathReservations>,
    ) {
        self.path_reservations = reservations;
    }
    /// What the runtime of the admitted open `handle` is built from (senpi `runtimeProfile`); taken once.
    pub fn take_runtime_profile(&mut self, handle: &str) -> Option<RpcSessionLaunchProfile> {
        self.runtime_profiles.remove(handle)
    }
    /// senpi `syncRuntimeMetadata`: every entry's reservation and claim follow the file its runtime writes.
    pub fn sync_runtime_metadata(&mut self) {
        for entry in self.entries.values_mut() {
            let Some(runtime) = &entry.runtime else {
                continue;
            };
            let (current_path, current_id) = (
                runtime.session().session_file(),
                runtime.session().session_id(),
            );
            crate::session_registry_claims::sync_runtime_metadata(
                crate::session_registry_claims::RuntimeMetadata {
                    reservation_key: &mut entry.close.reservation_key,
                    session_path: &mut entry.session_path,
                    durable_session_id: &mut entry.durable_session_id,
                    attachments: entry.close.attachments,
                },
                current_path,
                current_id,
                &mut self.reservations,
                self.path_reservations.as_mut(),
            );
        }
    }
    /// senpi `openRegistrySession`'s synchronous decision: attach to the entry holding the path
    /// (`attachToOpenSession`), hand back the in-flight open to wait on, or reserve and claim the path and record
    /// the opening entry (`takeCrossGenerationClaim` + `buildOpeningEntry`).
    pub fn admit_open(
        &mut self,
        profile: RpcSessionLaunchProfile,
        retain: bool,
        now: f64,
    ) -> Result<OpenAdmission, RpcSessionRegistryError> {
        self.admit_open_with_policy(profile, retain, now, false)
    }
    pub fn admit_open_with_policy(
        &mut self,
        mut profile: RpcSessionLaunchProfile,
        retain: bool,
        now: f64,
        attach_only: bool,
    ) -> Result<OpenAdmission, RpcSessionRegistryError> {
        validate_profile_paths(&profile)?;
        if let Some(path) = &profile.session_path {
            profile.session_path = Some(
                canonical_path(Path::new(path))
                    .map_err(|error| {
                        RpcSessionRegistryError::new("invalid_path", Some(&error.to_string()), None)
                    })?
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        self.sync_runtime_metadata();
        let entries = self
            .entries
            .values()
            .map(|entry| DurableSessionIdentity {
                state: entry.close.state,
                durable_session_id: entry.durable_session_id.clone(),
                reservation_key: entry.close.reservation_key.clone(),
            })
            .collect::<Vec<_>>();
        check_durable_session_collision(
            profile.durable_session_id.as_deref(),
            profile.session_path.as_deref(),
            &entries,
        )?;
        if let Some(path) = profile.session_path.clone()
            && (self.reservations.contains(&path)
                || self
                    .entries
                    .values()
                    .any(|entry| entry.close.reservation_key.as_ref() == Some(&path)))
        {
            let completions = &self.open_completions;
            let candidates = self
                .entries
                .iter_mut()
                .map(
                    |(handle, entry)| crate::session_registry_attach::AttachCandidate {
                        handle,
                        entry,
                        open_completion: completions
                            .get(handle)
                            .map(|(_, completion)| completion.clone()),
                    },
                )
                .collect();
            return match crate::session_registry_attach::attach_to_open_session(
                candidates,
                &path,
                &profile,
                retain,
                self.path_reservations.as_mut(),
                now,
            )? {
                crate::session_registry_attach::AttachDecision::Attached { session_id, .. } => {
                    Ok(OpenAdmission::Attached(session_id))
                }
                crate::session_registry_attach::AttachDecision::Wait(completion) => {
                    Ok(OpenAdmission::Wait {
                        completion,
                        session_path: path,
                    })
                }
            };
        }
        if attach_only {
            return Err(RpcSessionRegistryError::new(
                crate::rpc_types::RPC_ERROR_SESSION_NOT_HELD,
                None,
                Some(
                    serde_json::json!({"reason": "session_not_held", "action": "rediscover_holder"}),
                ),
            ));
        }
        if profile.session_kind == Some(maho_ext_api::SessionKind::Worker)
            && let Some((rss, retry)) = self.worker_refusal
        {
            return Err(RpcSessionRegistryError::new(
                "host_memory_pressure",
                None,
                Some(serde_json::json!({"rssMb":rss,"retry_after_ms":retry})),
            ));
        }
        // Taken before the entry exists, with no await between: a concurrent open for the same path finds the
        // reservation held. A live claim another generation holds gives the reservation back and refuses.
        let mut settle_claim = None;
        if let Some(path) = &profile.session_path {
            self.reservations.insert(path.clone());
            if let Some(claims) = self.path_reservations.as_mut() {
                settle_claim = Some(crate::session_registry_claims::take_cross_generation_claim(
                    &mut self.reservations,
                    &self.pending_claims,
                    claims,
                    path,
                )?);
            }
        }
        self.next_handle += 1;
        let handle = format!("rpc-{}", self.next_handle);
        let opening = crate::session_registry_opening::build_opening_entry(
            crate::session_registry_opening::OpeningInput {
                profile: &profile,
                session_path: profile.session_path.clone(),
                requested_durable_id: profile.durable_session_id.clone(),
                retain_on_disconnect: retain,
                now,
            },
        );
        let mut runtime_profile = opening.runtime_profile;
        runtime_profile.session_start_reason = opening
            .session_start_event
            .as_ref()
            .map(|event| event.reason);
        self.entries.insert(handle.clone(), opening.entry);
        self.size_changed();
        self.open_completions.insert(
            handle.clone(),
            (opening.settle_open, opening.open_completion),
        );
        self.runtime_profiles
            .insert(handle.clone(), runtime_profile);
        if let Some(settle) = settle_claim {
            settle();
        }
        Ok(OpenAdmission::Create(handle))
    }
    /// Settles an opening entry's open completion: waiters attach (`true`) or retry (`false`).
    fn settle_open(&mut self, handle: &str, opened: bool) {
        if let Some((settle, _)) = self.open_completions.remove(handle) {
            settle.send_replace(Some(opened));
        }
        self.runtime_profiles.remove(handle);
    }
    /// Gives an entry's path reservation and cross-generation claim back.
    fn release_reservation(&mut self, key: Option<&str>) {
        let Some(key) = key else {
            return;
        };
        self.reservations.remove(key);
        if let Some(claims) = self.path_reservations.as_mut() {
            claims.release(key);
        }
    }
    pub fn install_runtime(
        &mut self,
        handle: &str,
        runtime: Arc<maho_core::agent_session_runtime::AgentSessionRuntime>,
    ) -> Result<(), RpcSessionRegistryError> {
        let entry = self
            .entries
            .get_mut(handle)
            .filter(|entry| entry.close.state == RpcSessionState::Opening)
            .ok_or_else(|| RpcSessionRegistryError::new("unknown_session", None, None))?;
        entry.durable_session_id = Some(runtime.session().session_id());
        entry.session_path = runtime.session().session_file();
        // senpi `openSession`: the lane bootstraps from the manager's journal before admission.
        let compact_updates = self
            .compact_updates
            .unwrap_or_else(maho_core::engine_socket_flag::compact_updates_enabled);
        if compact_updates && !self.worker_isolate {
            let mut lane = crate::session_acceptance_lane::SessionAcceptanceLane::default();
            let transcript = runtime
                .session()
                .with_session_manager_mut(|manager| manager.begin_transcript_journal());
            lane.bootstrap(
                crate::session_acceptance_lane::SessionBootstrap { transcript },
                None,
            );
            runtime
                .session()
                .set_session_recovery_epoch(lane.recovery_epoch());
            entry.acceptance = Some(Arc::new(std::sync::Mutex::new(lane)));
        }
        // senpi `openRegistrySession`/`createEntrySwitchSession`: a replacement in place seals the outgoing
        // session's queued client input and held model switches while its transcript can still be written. The
        // ledgers are keyed by the host handle, which survives the replacement.
        runtime
            .session()
            .on_before_session_replace(seal_ledgers_on_replace(handle.to_owned()));
        entry.close.writing = true;
        entry.close.state = RpcSessionState::Open;
        entry.runtime = Some(runtime);
        self.settle_open(handle, true);
        Ok(())
    }
    pub async fn create_admitted_runtime(
        &mut self,
        handle: &str,
        create: impl std::future::Future<
            Output = Result<maho_core::agent_session_runtime::AgentSessionRuntime, String>,
        >,
    ) -> Result<(), RpcSessionRegistryError> {
        let scope = self
            .entries
            .get(handle)
            .filter(|entry| entry.close.state == RpcSessionState::Opening)
            .ok_or_else(|| RpcSessionRegistryError::new("unknown_session", None, None))?
            .scope
            .clone();
        let created =
            maho_ai::node::provider_scope::run_with_provider_scope_async(&scope, create).await;
        match created {
            Ok(Ok(runtime)) => self.install_runtime(handle, Arc::new(runtime)),
            result => {
                self.rollback_open(handle).await;
                let reason = match result {
                    Ok(Err(reason)) => reason,
                    Err(reason) => reason.to_string(),
                    Ok(Ok(_)) => unreachable!(),
                };
                Err(RpcSessionRegistryError::new(
                    "open_failed",
                    Some(&reason),
                    None,
                ))
            }
        }
    }
    pub async fn rollback_open(&mut self, handle: &str) {
        if let Some(entry) = self.entries.get(handle) {
            if let Some(runtime) = &entry.runtime {
                runtime.dispose().await;
            }
            entry.scope.close();
        }
        if let Some(entry) = self.entries.remove(handle) {
            self.release_reservation(entry.close.reservation_key.as_deref());
            self.size_changed();
        }
        self.settle_open(handle, false);
    }
    pub fn list_sessions(&self, include_workers: bool) -> Vec<serde_json::Value> {
        self.entries.iter().filter(|(_,entry)|entry.close.state!=RpcSessionState::Quarantined&&(include_workers||entry.identity.kind!=maho_ext_api::SessionKind::Worker)).map(|(handle,entry)|{
            let status=match entry.close.state{RpcSessionState::Opening=>"opening",RpcSessionState::Open=>"open",RpcSessionState::Closing=>"closing",RpcSessionState::Quarantined=>"quarantined",RpcSessionState::Closed=>"closed"};
            let mut row=serde_json::json!({"sessionId":handle,"cwd":entry.runtime.as_ref().map_or_else(||entry.profile.runtime.cwd.clone(),|runtime|runtime.session().cwd()),"status":status,"attachments":entry.close.attachments,"kind":if entry.identity.kind==maho_ext_api::SessionKind::Worker{"worker"}else{"interactive"},"tool_child":entry.runtime.as_ref().and_then(|runtime|serde_json::to_value(runtime.session().tool_child_row()).ok()).unwrap_or_else(||serde_json::json!({"state":"none"}))});
            if let Some(id)=&entry.durable_session_id{row["durableSessionId"]=id.clone().into();}
            if let Some(path)=&entry.session_path{row["sessionPath"]=path.clone().into();}
            if let Some(name)=entry.runtime.as_ref().and_then(|runtime|runtime.session().session_name()){row["name"]=name.into();}
            if include_workers{row["context"]=serde_json::Value::Object(crate::session_context_patch::published_session_context(&entry.identity.context,entry.context_overlay.as_ref()));}
            row
        }).collect()
    }
    pub fn mark_close(
        &mut self,
        handle: &str,
        detach: bool,
    ) -> Result<crate::session_teardown::CloseClaim, RpcSessionRegistryError> {
        let entry = self
            .entries
            .get_mut(handle)
            .ok_or_else(|| RpcSessionRegistryError::new("unknown_session", None, None))?;
        // A session mid-turn keeps its claim until it parks: reclaiming it would put two writers on one transcript.
        entry.close.writing =
            crate::session_teardown::session_is_writing(None, entry.runtime.as_deref());
        let claim = crate::session_teardown::begin_session_close(&mut entry.close, detach)?;
        // A retained session parked at zero attachments is reclaimable by another generation again.
        if let Some(key) = &claim.mark_detached
            && let Some(claims) = self.path_reservations.as_mut()
        {
            claims.set_attached(key, false);
        }
        Ok(claim)
    }
    pub async fn close_marked(
        &mut self,
        handle: &str,
        grace_ms: u64,
    ) -> Result<Option<crate::session_teardown::RuntimeCloseCompletion>, RpcSessionRegistryError>
    {
        let entry = self
            .entries
            .get(handle)
            .filter(|entry| entry.close.state == RpcSessionState::Closing)
            .ok_or_else(|| RpcSessionRegistryError::new("unknown_session", None, None))?;
        let completion = if let Some(runtime) = &entry.runtime {
            Some(
                crate::session_teardown::close_runtime_with_grace(
                    Arc::clone(runtime),
                    entry.scope.clone(),
                    grace_ms,
                )
                .await,
            )
        } else {
            entry.scope.close();
            None
        };
        if let Some(entry) = self.entries.remove(handle) {
            self.release_reservation(entry.close.reservation_key.as_deref());
            self.size_changed();
        }
        Ok(completion)
    }
}
pub fn validate_profile_paths(
    profile: &RpcSessionLaunchProfile,
) -> Result<(), RpcSessionRegistryError> {
    // senpi `validateProfile` uses node `path.isAbsolute`, which on win32 also accepts a rooted path without a
    // drive ("/tmp"); `Path::has_root` is that check on every platform, `Path::is_absolute` is not.
    if !Path::new(&profile.runtime.cwd).has_root()
        || profile
            .session_path
            .as_ref()
            .is_some_and(|path| !Path::new(path).has_root())
    {
        return Err(RpcSessionRegistryError::new("invalid_path", None, None));
    }
    // senpi `validateProfile`: a durable id must be one the session manager accepts (`assertValidSessionId`, checked
    // here instead of asserted: the native assertion panics).
    if let Some(id) = &profile.durable_session_id {
        let bytes = id.as_bytes();
        let valid = !bytes.is_empty()
            && bytes[0].is_ascii_alphanumeric()
            && bytes[bytes.len() - 1].is_ascii_alphanumeric()
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
        if !valid {
            return Err(RpcSessionRegistryError::new(
                "invalid_session_id",
                Some(
                    "Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character",
                ),
                None,
            ));
        }
    }
    Ok(())
}
pub fn check_durable_session_collision(
    requested_id: Option<&str>,
    session_path: Option<&str>,
    entries: &[DurableSessionIdentity],
) -> Result<(), RpcSessionRegistryError> {
    if let Some(requested_id) = requested_id {
        for entry in entries {
            if entry.state == RpcSessionState::Closed
                || entry.durable_session_id.as_deref() != Some(requested_id)
            {
                continue;
            }
            if session_path.is_some() && entry.reservation_key.as_deref() == session_path {
                continue;
            }
            return Err(RpcSessionRegistryError::new(
                "session_id_in_use",
                None,
                None,
            ));
        }
    }
    Ok(())
}
pub fn session_identity(profile: &RpcSessionLaunchProfile) -> SessionIdentity {
    SessionIdentity {
        kind: profile.session_kind.unwrap_or_default(),
        context: Arc::new(profile.session_context.clone().unwrap_or_default()),
    }
}
/// The launch inputs an open froze (senpi `frozenProfile`). Rust's ownership already replaces
/// `Object.freeze`; the value is cloned so the entry and the runtime share no aliasing.
pub fn frozen_profile(profile: &RpcSessionLaunchProfile) -> RpcSessionLaunchProfile {
    profile.clone()
}
/// node `realpathSync` spelling: on Windows without the `\\?\` verbatim prefix `canonicalize` adds.
pub fn canonical_path(path: &Path) -> std::io::Result<PathBuf> {
    use maho_core::paths::strip_verbatim_prefix;
    if path.exists() {
        path.canonicalize().map(strip_verbatim_prefix)
    } else {
        let parent = path.parent().unwrap_or(Path::new("."));
        Ok(
            strip_verbatim_prefix(parent.canonicalize()?)
                .join(path.file_name().unwrap_or_default()),
        )
    }
}
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("{code}{suffix}")]
pub struct RpcSessionRegistryError {
    pub code: String,
    pub detail: Option<serde_json::Value>,
    suffix: String,
}
impl RpcSessionRegistryError {
    pub fn new(code: &str, reason: Option<&str>, detail: Option<serde_json::Value>) -> Self {
        Self {
            code: code.into(),
            detail,
            suffix: if code == "open_failed" {
                reason
                    .filter(|reason| !reason.is_empty())
                    .map_or(String::new(), |reason| format!(": {reason}"))
            } else {
                String::new()
            },
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attach_only_refuses_without_allocating_entries_or_reservations() {
        let root = tempfile::tempdir().unwrap();
        let mut profile = RpcSessionLaunchProfile::default();
        profile.runtime.cwd = root.path().to_string_lossy().into_owned();
        profile.session_path = Some(
            root.path()
                .join("unheld.jsonl")
                .to_string_lossy()
                .into_owned(),
        );
        let mut registry = RpcSessionRegistry::default();
        let error = registry
            .admit_open_with_policy(profile, false, 0., true)
            .err()
            .unwrap();
        assert_eq!(error.code, "session_not_held");
        assert!(registry.entries.is_empty());
        assert!(registry.reservations.is_empty());
        assert!(registry.open_completions.is_empty());
        assert_eq!(registry.next_handle, 0);
    }
    #[test]
    fn frozen_profile_isolates_the_caller_copy() {
        let mut profile = RpcSessionLaunchProfile {
            runtime: Default::default(),
            session_path: None,
            durable_session_id: None,
            session_kind: None,
            session_context: Some(BTreeMap::from([("owner".into(), "a".into())])),
            ..Default::default()
        };
        let frozen = frozen_profile(&profile);
        profile
            .session_context
            .as_mut()
            .unwrap()
            .insert("owner".into(), "b".into());
        assert_eq!(frozen.session_context.unwrap()["owner"], "a");
    }
    #[test]
    fn launch_paths_must_be_absolute_before_creation() {
        let mut profile = RpcSessionLaunchProfile {
            runtime: Default::default(),
            session_path: None,
            durable_session_id: None,
            session_kind: None,
            session_context: None,
            ..Default::default()
        };
        profile.runtime.cwd = "relative".into();
        assert_eq!(
            validate_profile_paths(&profile).unwrap_err().code,
            "invalid_path"
        );
        let abs_cwd = if cfg!(windows) {
            "C:\\absolute"
        } else {
            "/absolute"
        };
        let abs_session = if cfg!(windows) {
            "C:\\session"
        } else {
            "/session"
        };
        profile.runtime.cwd = abs_cwd.into();
        assert!(validate_profile_paths(&profile).is_ok());
        profile.session_path = Some("relative".into());
        assert!(validate_profile_paths(&profile).is_err());
        profile.session_path = Some(abs_session.into());
        assert!(validate_profile_paths(&profile).is_ok());
    }
    fn registry_with(state: RpcSessionState, attachments: i64) -> RpcSessionRegistry {
        let mut registry = RpcSessionRegistry::default();
        let mut profile = RpcSessionLaunchProfile::default();
        profile.runtime.cwd = if cfg!(windows) { "C:\\cwd" } else { "/cwd" }.into();
        let crate::session_registry::OpenAdmission::Create(handle) =
            registry.admit_open(profile, true, 0.).unwrap()
        else {
            panic!("a fresh open creates");
        };
        let entry = registry.get_mut(&handle).unwrap();
        entry.close.state = state;
        entry.close.attachments = attachments;
        registry
    }
    /// senpi `onSizeChange`: an admitted open and a rolled-back one each tell the hook the entry count.
    #[tokio::test]
    async fn every_admit_and_removal_reports_the_entry_count() {
        let sizes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut registry = RpcSessionRegistry::default();
        let seen = Arc::clone(&sizes);
        registry.on_size_change = Some(Arc::new(move |size| seen.lock().unwrap().push(size)));
        let mut profile = RpcSessionLaunchProfile::default();
        profile.runtime.cwd = if cfg!(windows) { "C:\\cwd" } else { "/cwd" }.into();
        let OpenAdmission::Create(handle) = registry.admit_open(profile, true, 0.).unwrap() else {
            panic!("a fresh open creates");
        };
        registry.rollback_open(&handle).await;
        assert_eq!(*sizes.lock().unwrap(), vec![1, 0]);
    }
    // senpi `getForCommand`: a closing session still takes control commands, `host_response` included.
    #[test]
    fn a_closing_session_accepts_only_control_commands() {
        let mut registry = registry_with(RpcSessionState::Closing, 0);
        for command in [
            "abort",
            "abort_bash",
            "extension_ui_response",
            "extension_ui_progress",
            "host_response",
        ] {
            assert!(
                registry.get_for_command("rpc-1", command, 1.).is_ok(),
                "{command}"
            );
        }
        assert_eq!(
            registry
                .get_for_command("rpc-1", "prompt", 1.)
                .err()
                .unwrap()
                .code,
            "session_closing"
        );
        let mut opening = registry_with(RpcSessionState::Opening, 1);
        assert_eq!(
            opening
                .get_for_command("rpc-1", "abort", 1.)
                .err()
                .unwrap()
                .code,
            "unknown_session"
        );
    }
    // senpi `getForCommand`: polling a session nobody holds is observation and leaves its idle clock alone.
    #[test]
    fn polling_an_unattached_session_does_not_refresh_its_activity() {
        let mut registry = registry_with(RpcSessionState::Open, 0);
        registry.get_for_command("rpc-1", "get_state", 5.).unwrap();
        assert_eq!(registry.get("rpc-1").unwrap().last_command_at, 0.);
        registry.get_for_command("rpc-1", "prompt", 6.).unwrap();
        assert_eq!(registry.get("rpc-1").unwrap().last_command_at, 6.);
        let mut attached = registry_with(RpcSessionState::Open, 1);
        attached.get_for_command("rpc-1", "get_state", 7.).unwrap();
        assert_eq!(attached.get("rpc-1").unwrap().last_command_at, 7.);
    }
    // senpi `attachSession`: only an open entry is joined; a parked one reports its runtime for the resume.
    #[test]
    fn attach_session_counts_only_open_entries() {
        let mut registry = registry_with(RpcSessionState::Open, 0);
        assert!(registry.attach_session("rpc-1", 3.).unwrap().is_none());
        assert_eq!(registry.get("rpc-1").unwrap().close.attachments, 1);
        assert_eq!(registry.get("rpc-1").unwrap().last_command_at, 3.);
        let mut closing = registry_with(RpcSessionState::Closing, 1);
        assert_eq!(
            closing
                .attach_session("rpc-1", 3.)
                .err()
                .map(|error| error.code),
            Some("unknown_session".to_owned())
        );
    }
    #[test]
    fn an_invalid_durable_id_is_refused_with_its_own_code() {
        let mut profile = RpcSessionLaunchProfile::default();
        profile.runtime.cwd = if cfg!(windows) { "C:\\cwd" } else { "/cwd" }.into();
        profile.durable_session_id = Some("../escape".into());
        assert_eq!(
            validate_profile_paths(&profile).unwrap_err().code,
            "invalid_session_id"
        );
        profile.durable_session_id = Some("good-id".into());
        assert!(validate_profile_paths(&profile).is_ok());
    }
    #[test]
    fn durable_collision_is_live_only_and_same_reserved_path_may_attach() {
        let entries = [DurableSessionIdentity {
            state: RpcSessionState::Opening,
            durable_session_id: Some("durable".into()),
            reservation_key: Some("/same".into()),
        }];
        assert!(check_durable_session_collision(Some("durable"), Some("/same"), &entries).is_ok());
        assert_eq!(
            check_durable_session_collision(Some("durable"), Some("/other"), &entries)
                .unwrap_err()
                .code,
            "session_id_in_use"
        );
        assert!(check_durable_session_collision(None, None, &entries).is_ok());
        let entries = [DurableSessionIdentity {
            state: RpcSessionState::Closed,
            durable_session_id: Some("durable".into()),
            reservation_key: None,
        }];
        assert!(check_durable_session_collision(Some("durable"), None, &entries).is_ok());
    }
    #[test]
    fn identity_defaults_and_context_do_not_alias_client_inputs() {
        let mut profile = RpcSessionLaunchProfile {
            runtime: Default::default(),
            session_path: None,
            durable_session_id: None,
            session_kind: None,
            session_context: Some(BTreeMap::from([("owner".into(), "a".into())])),
            ..Default::default()
        };
        let identity = session_identity(&profile);
        profile
            .session_context
            .as_mut()
            .unwrap()
            .insert("owner".into(), "b".into());
        assert_eq!(identity.kind, maho_ext_api::SessionKind::Interactive);
        assert_eq!(identity.context["owner"], "a");
    }
    #[test]
    fn canonical_missing_leaf_uses_real_parent() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let parent = root.join("real");
        std::fs::create_dir(&parent).unwrap();
        #[cfg(unix)]
        {
            let alias = root.join("alias");
            std::os::unix::fs::symlink(&parent, &alias).unwrap();
            assert_eq!(
                canonical_path(&alias.join("missing.jsonl")).unwrap(),
                parent.join("missing.jsonl")
            );
        }
    }
    fn opening_profile(dir: &Path, name: &str) -> RpcSessionLaunchProfile {
        let mut profile = RpcSessionLaunchProfile::default();
        profile.runtime.cwd = dir.to_string_lossy().into_owned();
        profile.session_path = Some(dir.join(name).to_string_lossy().into_owned());
        profile.runtime.creation_model = Some(("p".into(), "m".into()));
        profile
    }

    fn created(admission: Result<OpenAdmission, RpcSessionRegistryError>) -> String {
        match admission {
            Ok(OpenAdmission::Create(handle)) => handle,
            _ => panic!("expected a create admission"),
        }
    }

    // senpi `openRegistrySession`: an open reserves and claims its path before the entry exists, and a rollback gives
    // both back and tells the waiters the open failed (session-registry-open.ts).
    #[tokio::test]
    async fn an_open_claims_its_path_and_a_rollback_releases_it_and_fails_its_waiters() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let daemon = root.join("daemon");
        let mut registry = RpcSessionRegistry::default();
        registry.set_path_reservations(Some(
            crate::host_reservations::create_session_path_reservations(
                &daemon,
                "this-host".into(),
                None,
                |_| {},
            ),
        ));
        let profile = opening_profile(&root, "s.jsonl");
        let handle = created(registry.admit_open(profile.clone(), false, 0.));
        let key = registry.get(&handle).unwrap().close.reservation_key.clone();
        let claims = || {
            crate::host_reservations::read_session_path_claims(
                &crate::host_daemon_paths::host_daemon_directory_paths(&daemon).reservations_dir,
            )
        };
        assert_eq!(claims().len(), 1);
        assert_eq!(claims()[0].owner.session_path, key.clone().unwrap());
        let Ok(OpenAdmission::Wait {
            completion,
            session_path,
        }) = registry.admit_open(profile.clone(), false, 0.)
        else {
            panic!("a reopen of an opening path waits");
        };
        registry.rollback_open(&handle).await;
        assert!(claims().is_empty());
        let refused =
            crate::session_registry_attach::wait_open_completion(completion, &session_path)
                .await
                .unwrap_err();
        assert_eq!(refused.code, "session_path_in_use");
        assert_eq!(refused.detail.unwrap()["reason"], "open_failed");
        // The path is free again: the retry creates.
        created(registry.admit_open(profile, false, 0.));
    }

    // senpi `buildOpeningEntry`: re-opening an existing file is a resume that drops the create-only choices.
    #[test]
    fn an_open_of_an_existing_file_builds_its_runtime_as_a_resume() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::write(root.join("old.jsonl"), "").unwrap();
        let mut registry = RpcSessionRegistry::default();
        let resumed = created(registry.admit_open(opening_profile(&root, "old.jsonl"), false, 0.));
        let runtime = registry.take_runtime_profile(&resumed).unwrap();
        assert_eq!(
            runtime.session_start_reason,
            Some(maho_ext_api::SessionReason::Resume)
        );
        assert_eq!(runtime.runtime.creation_model, None);
        assert_eq!(
            registry
                .get(&resumed)
                .unwrap()
                .profile
                .runtime
                .creation_model,
            Some(("p".into(), "m".into()))
        );
        let fresh = created(registry.admit_open(opening_profile(&root, "new.jsonl"), false, 0.));
        let runtime = registry.take_runtime_profile(&fresh).unwrap();
        assert_eq!(runtime.session_start_reason, None);
        assert_eq!(
            runtime.runtime.creation_model,
            Some(("p".into(), "m".into()))
        );
    }

    // senpi `takeCrossGenerationClaim`: a live claim another generation holds refuses with its owner and leaves no
    // reservation behind.
    #[cfg(unix)]
    #[test]
    fn a_path_another_generation_holds_is_refused_with_its_owner() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let daemon = root.join("daemon");
        let parent = std::os::unix::process::parent_id();
        let profile = opening_profile(&root, "s.jsonl");
        let mut foreign = crate::host_reservations::create_session_path_reservations(
            &daemon,
            "other-host".into(),
            Some(parent),
            |_| {},
        );
        let key = canonical_path(Path::new(profile.session_path.as_deref().unwrap()))
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(foreign.claim(&key, true).is_none());
        let mut registry = RpcSessionRegistry::default();
        registry.set_path_reservations(Some(
            crate::host_reservations::create_session_path_reservations(
                &daemon,
                "this-host".into(),
                None,
                |_| {},
            ),
        ));
        let refused = registry
            .admit_open(profile.clone(), false, 0.)
            .err()
            .expect("a live foreign claim refuses");
        assert_eq!(refused.code, "session_path_in_use");
        assert_eq!(refused.detail.unwrap()["owner"]["pid"], parent);
        assert_eq!(registry.size(), 0);
        foreign.release(&key);
        created(registry.admit_open(profile, false, 0.));
    }

    #[test]
    fn only_open_failed_appends_reason() {
        assert_eq!(
            RpcSessionRegistryError::new("open_failed", Some("bad"), None).to_string(),
            "open_failed: bad"
        );
        assert_eq!(
            RpcSessionRegistryError::new("session_closing", Some("bad"), None).to_string(),
            "session_closing"
        );
    }
}
