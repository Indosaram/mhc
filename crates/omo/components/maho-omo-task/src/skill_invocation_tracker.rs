use crate::skill_invocation_handoff::{
    SessionHandoff, deposit_session_handoff, discard_session_handoff, shutdown_keeps_session,
    take_session_handoff,
};
use maho_ext_api::{
    EventKind, EventResult, ExtensionApi, ExtensionEvent, InputSource, SessionReason,
};
use regex::Regex;
use senpi_task::agents::{PlanArtifactReference, SkillInvocationState};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, PoisonError},
};
#[derive(Clone, Default)]
pub struct SessionSkills {
    invoked: BTreeSet<String>,
    requested: BTreeSet<String>,
    touches: BTreeMap<String, PlanArtifactReference>,
}
impl SkillInvocationState for SessionSkills {
    fn has_invoked(&self, skill: &str) -> bool {
        self.invoked.contains(skill)
    }
    fn has_user_requested(&self, skill: &str) -> bool {
        self.requested.contains(skill)
    }
    fn has_plan_artifact(&self) -> bool {
        !self.touches.is_empty()
    }
    fn plan_artifact_references(&self) -> Vec<PlanArtifactReference> {
        let mut references: Vec<_> = self.touches.values().cloned().collect();
        references.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then(b.last_touched_at.cmp(&a.last_touched_at))
        });
        references
    }
}
pub struct SkillInvocationTracker {
    sessions: Mutex<BTreeMap<String, SessionSkills>>,
    adopted: Mutex<BTreeSet<String>>,
    sequence: Mutex<u64>,
    expanded: Regex,
    injected: Vec<Regex>,
    requested: Regex,
    own_words: Vec<Regex>,
    skill_path: Regex,
    plan_path: Regex,
    patch_path: Regex,
}
impl senpi_task::tools::task::spawn_policy::SpawnPolicyDeps for SkillInvocationTracker {
    fn invocation_gate_denial(&self, agent: &str, session: &str) -> Option<String> {
        let state = self.state_for(session);
        let resolver = move |_: &str| Box::new(state.clone()) as Box<dyn SkillInvocationState>;
        senpi_task::tools::task::invocation_gate::invocation_gate_denial(
            Some(&resolver),
            agent,
            session,
        )
    }
    fn plan_review_contract_outcome(
        &self,
        agent: &str,
        prompt: &str,
        session: &str,
    ) -> Option<senpi_task::tools::task::spawn_policy::PlanReviewContractOutcome> {
        let state = self.state_for(session);
        let resolver = move |_: &str| Box::new(state.clone()) as Box<dyn SkillInvocationState>;
        senpi_task::tools::task::plan_review_contract::plan_review_contract_outcome(
            Some(&resolver),
            agent,
            prompt,
            session,
        )
    }
}
impl SkillInvocationTracker {
    pub fn new() -> Result<Arc<Self>, regex::Error> {
        Ok(Arc::new(Self { sessions: Mutex::new(BTreeMap::new()), adopted: Mutex::new(BTreeSet::new()), sequence: Mutex::new(0), expanded: Regex::new(r#"(?i)<skill\s+name="([^"]+)""#)?, injected: [r"(?is)<ultrawork-mode>.*?</ultrawork-mode>", r"(?is)<system-reminder>.*?</system-reminder>", r#"(?is)<skill\s+name="[^"]*".*?</skill>"#, r#"(?is)<skill\s+name="[^"]*".*$"#].into_iter().map(Regex::new).collect::<Result<_,_>>()?, requested: Regex::new(r"(?i)(?-u:\b)ulw[-_ ]?plan(?-u:\b)")?, own_words: [r"(?i)(?-u:\b)(?:make|write|create|draw|draft|build)\s+(?:me\s+)?(?:a|an|the)?\s*(?:work|implementation|action)?\s*plan(?-u:\b)",r"(?i)(?-u:\b)plan(?-u:\b)[^.!?\n]{0,40}(?-u:\b)before\s+(?:you\s+)?(?:cod(?:e|ing)|implement|start|work)",r"(?i)(?-u:\b)before\s+(?:you\s+)?(?:cod(?:e|ing)|implement|start)(?-u:\b)[^.!?\n]{0,40}(?-u:\b)plan(?-u:\b)",r"(?i)(?-u:\b)plan\s+(?:it|this|that|the\s+work)\s+(?:out\s+)?first(?-u:\b)",r"계획(?:서)?(?:부터|을|를|\s)*\s*(?:먼저\s*)?(?:세워|세우|짜|작성해|수립해)",r"(?:먼저|우선)\s*계획(?:서)?(?:을|를)?\s*(?:세워|세우|짜|작성해|수립해)"].into_iter().map(Regex::new).collect::<Result<_,_>>()?, skill_path: Regex::new(r"(?i)[\\/]skills[\\/]([^\\/]+)[\\/]SKILL\.md$")?, plan_path: Regex::new(r"(?i)(^|[\\/])\.omo[\\/]plans[\\/][^\\/]+\.md$")?, patch_path: Regex::new(r#"(?i)\.omo[\\/]plans[\\/][^\s"'`]+\.md"#)? }))
    }
    /// Adopt the session's handoff once per generation, before this generation records anything.
    fn adopt_handoff(&self, session: &str) {
        if session.is_empty()
            || !self
                .adopted
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(session.to_owned())
        {
            return;
        }
        let Some(handoff) = take_session_handoff(session) else {
            return;
        };
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let state = sessions.entry(session.to_owned()).or_default();
        state.invoked.extend(handoff.invoked);
        state.requested.extend(handoff.requested);
        for touch in handoff.plan_touches {
            state.touches.entry(touch.path.clone()).or_insert(touch);
        }
        let mut sequence = self.sequence.lock().unwrap_or_else(PoisonError::into_inner);
        *sequence = (*sequence).max(handoff.plan_touch_sequence);
    }
    pub fn state_for(&self, session: &str) -> SessionSkills {
        self.adopt_handoff(session);
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(session)
            .cloned()
            .unwrap_or_default()
    }
    pub fn input(&self, session: &str, text: &str, source: InputSource) {
        if session.is_empty() || source == InputSource::Extension {
            return;
        }
        self.adopt_handoff(session);
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let state = sessions.entry(session.to_owned()).or_default();
        if let Some(command) = text.strip_prefix("/skill:") {
            let skill = command.split(' ').next().unwrap_or_default().trim();
            if !skill.is_empty() {
                state.invoked.insert(skill.to_owned());
                state.requested.insert(skill.to_owned());
            }
            return;
        }
        for capture in self.expanded.captures_iter(text) {
            if let Some(skill) = capture
                .get(1)
                .map(|name| name.as_str().trim())
                .filter(|name| !name.is_empty())
            {
                state.invoked.insert(skill.to_owned());
                state.requested.insert(skill.to_owned());
            }
        }
        // PERF-DIVERGENCE(omo skill-invocation-tracker): upstream reduces String.replace over the injected patterns, copying per pattern; we borrow the input and allocate only when a pattern actually matches. The resulting text is identical.
        let mut visible: std::borrow::Cow<str> = std::borrow::Cow::Borrowed(text);
        for pattern in &self.injected {
            let stripped = match pattern.replace_all(&visible, "") {
                std::borrow::Cow::Owned(stripped) => Some(stripped),
                std::borrow::Cow::Borrowed(_) => None,
            };
            if let Some(stripped) = stripped {
                visible = std::borrow::Cow::Owned(stripped);
            }
        }
        if self.requested.is_match(&visible)
            || self
                .own_words
                .iter()
                .any(|pattern| pattern.is_match(&visible))
        {
            state.requested.insert("ulw-plan".into());
        }
    }
    pub fn tool_result(&self, session: &str, tool: &str, input: &Value, is_error: bool) {
        if session.is_empty() || is_error {
            return;
        }
        self.adopt_handoff(session);
        let path = input.get("path").and_then(Value::as_str);
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let state = sessions.entry(session.to_owned()).or_default();
        if tool == "read"
            && let Some(capture) = path.and_then(|path| self.skill_path.captures(path))
            && let Some(skill) = capture.get(1)
        {
            state.invoked.insert(skill.as_str().to_owned());
        }
        let paths: BTreeSet<String> = if matches!(tool, "read" | "write" | "edit") {
            path.filter(|path| self.plan_path.is_match(path))
                .map(|path| BTreeSet::from([path.replace('\\', "/")]))
                .unwrap_or_default()
        } else if tool == "apply_patch" {
            input
                .get("input")
                .and_then(Value::as_str)
                .map(|text| {
                    self.patch_path
                        .find_iter(text)
                        .map(|path| path.as_str().replace('\\', "/"))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            BTreeSet::new()
        };
        for path in paths {
            let mut sequence = self.sequence.lock().unwrap_or_else(PoisonError::into_inner);
            *sequence += 1;
            let reference = state
                .touches
                .entry(path.clone())
                .or_insert(PlanArtifactReference {
                    path,
                    count: 0,
                    last_touched_at: 0,
                });
            reference.count += 1;
            reference.last_touched_at = *sequence;
        }
    }
    pub fn replay(&self, session: &str, entries: &[maho_ext_api::SessionEntry]) {
        if session.is_empty() { return; }
        self.adopt_handoff(session);
        let kept = self.sessions.lock().unwrap_or_else(PoisonError::into_inner).remove(session).unwrap_or_default();
        let mut calls = BTreeMap::<String, (String, Value)>::new();
        for entry in entries {
            let data = &entry.data;
            if entry.kind != "message" { continue; }
            let Some(message) = data.get("message") else { continue; };
            match message.get("role").and_then(Value::as_str) {
                Some("user") if matches!(data.get("inputSource").and_then(Value::as_str), Some("interactive" | "rpc")) => {
                    let content = &message["content"];
                    let text = content.as_str().map(str::to_owned).unwrap_or_else(|| content.as_array().map(|blocks| blocks.iter().filter(|block| block["type"] == "text").filter_map(|block| block["text"].as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default());
                    self.input(session, &text, InputSource::Interactive);
                }
                Some("assistant") => {
                    if let Some(blocks) = message["content"].as_array() {
                        for block in blocks {
                            if block["type"] == "toolCall"
                                && let (Some(id), Some(name)) = (block["id"].as_str(), block["name"].as_str()) {
                                calls.insert(id.to_owned(), (name.to_owned(), block["arguments"].clone()));
                            }
                        }
                    }
                }
                Some("toolResult") => {
                    if let Some((name, args)) = message["toolCallId"].as_str().and_then(|id| calls.get(id)) {
                        self.tool_result(session, name, args, message["isError"] == true);
                    }
                }
                _ => {}
            }
        }
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let state = sessions.entry(session.to_owned()).or_default();
        state.invoked.extend(kept.invoked);
        state.requested.extend(kept.requested);
        for (path, touch) in kept.touches { state.touches.entry(path).or_insert(touch); }
    }
    pub fn shutdown(&self, session: &str, reason: SessionReason) {
        let removed = self
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(session);
        if shutdown_keeps_session(reason) {
            let state = removed.unwrap_or_default();
            let plan_touch_sequence = *self.sequence.lock().unwrap_or_else(PoisonError::into_inner);
            deposit_session_handoff(
                session,
                SessionHandoff {
                    invoked: state.invoked.into_iter().collect(),
                    requested: state.requested.into_iter().collect(),
                    plan_touches: state.touches.into_values().collect(),
                    plan_touch_sequence,
                },
            );
        } else {
            discard_session_handoff(session);
        }
        self.adopted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(session);
    }
    pub fn register(self: &Arc<Self>, api: &mut ExtensionApi) {
        for kind in [
            EventKind::SessionStart,
            EventKind::Input,
            EventKind::ToolResult,
            EventKind::SessionShutdown,
        ] {
            let tracker = self.clone();
            api.on(
                kind,
                Arc::new(move |event, context| {
                    let tracker = tracker.clone();
                    let session = context.session_manager.session_id().to_owned();
                    Box::pin(async move {
                        match event {
                            ExtensionEvent::SessionStart(_) => tracker.replay(&session, &context.session_manager.get_branch()),
                            ExtensionEvent::Input(input) => {
                                tracker.input(&session, &input.text, input.source)
                            }
                            ExtensionEvent::ToolResult(result) => tracker.tool_result(
                                &session,
                                &result.tool_name,
                                &result.input,
                                result.is_error,
                            ),
                            ExtensionEvent::SessionShutdown(event) => {
                                tracker.shutdown(&session, event.reason)
                            }
                            _ => {}
                        }
                        Ok(EventResult::None)
                    })
                }),
            );
        }
    }
}
