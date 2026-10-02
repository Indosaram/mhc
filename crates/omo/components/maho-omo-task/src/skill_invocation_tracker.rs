use std::{collections::{BTreeMap, BTreeSet}, sync::{Arc, Mutex, PoisonError}};
use maho_ext_api::{EventKind, EventResult, ExtensionApi, ExtensionEvent, InputSource};
use regex::Regex;
use senpi_task::agents::{PlanArtifactReference, SkillInvocationState};
use serde_json::Value;
#[derive(Clone, Default)]
pub struct SessionSkills { invoked: BTreeSet<String>, requested: BTreeSet<String>, touches: BTreeMap<String, PlanArtifactReference> }
impl SkillInvocationState for SessionSkills {
    fn has_invoked(&self, skill: &str) -> bool { self.invoked.contains(skill) }
    fn has_user_requested(&self, skill: &str) -> bool { self.requested.contains(skill) }
    fn has_plan_artifact(&self) -> bool { !self.touches.is_empty() }
    fn plan_artifact_references(&self) -> Vec<PlanArtifactReference> { let mut references: Vec<_> = self.touches.values().cloned().collect(); references.sort_by(|a,b| b.count.cmp(&a.count).then(b.last_touched_at.cmp(&a.last_touched_at))); references }
}
pub struct SkillInvocationTracker {
    sessions: Mutex<BTreeMap<String, SessionSkills>>, sequence: Mutex<u64>, expanded: Regex, injected: Vec<Regex>, requested: Regex, own_words: Vec<Regex>, skill_path: Regex, plan_path: Regex, patch_path: Regex,
}
impl SkillInvocationTracker {
    pub fn new() -> Result<Arc<Self>, regex::Error> {
        Ok(Arc::new(Self { sessions: Mutex::new(BTreeMap::new()), sequence: Mutex::new(0), expanded: Regex::new(r#"(?i)<skill\s+name="([^"]+)""#)?, injected: [r"(?is)<ultrawork-mode>.*?</ultrawork-mode>", r"(?is)<system-reminder>.*?</system-reminder>", r#"(?is)<skill\s+name="[^"]*".*?</skill>"#, r#"(?is)<skill\s+name="[^"]*".*$"#].into_iter().map(Regex::new).collect::<Result<_,_>>()?, requested: Regex::new(r"(?i)\bulw[-_ ]?plan\b")?, own_words: [r"(?i)\b(?:make|write|create|draw|draft|build)\s+(?:me\s+)?(?:a|an|the)?\s*(?:work|implementation|action)?\s*plan\b",r"(?i)\bplan\b[^.!?\n]{0,40}\bbefore\s+(?:you\s+)?(?:cod(?:e|ing)|implement|start|work)",r"(?i)\bbefore\s+(?:you\s+)?(?:cod(?:e|ing)|implement|start)\b[^.!?\n]{0,40}\bplan\b",r"(?i)\bplan\s+(?:it|this|that|the\s+work)\s+(?:out\s+)?first\b",r"계획(?:서)?(?:부터|을|를|\s)*\s*(?:먼저\s*)?(?:세워|세우|짜|작성해|수립해)",r"(?:먼저|우선)\s*계획(?:서)?(?:을|를)?\s*(?:세워|세우|짜|작성해|수립해)"].into_iter().map(Regex::new).collect::<Result<_,_>>()?, skill_path: Regex::new(r"(?i)[\\/]skills[\\/]([^\\/]+)[\\/]SKILL\.md$")?, plan_path: Regex::new(r"(?i)(^|[\\/])\.omo[\\/]plans[\\/][^\\/]+\.md$")?, patch_path: Regex::new(r#"(?i)\.omo[\\/]plans[\\/][^\s"'`]+\.md"#)? }))
    }
    pub fn state_for(&self, session: &str) -> SessionSkills { self.sessions.lock().unwrap_or_else(PoisonError::into_inner).get(session).cloned().unwrap_or_default() }
    pub fn input(&self, session: &str, text: &str, source: InputSource) {
        if session.is_empty() || source == InputSource::Extension { return; }
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner); let state = sessions.entry(session.to_owned()).or_default();
        if let Some(command) = text.strip_prefix("/skill:") { let skill = command.split(' ').next().unwrap_or_default().trim(); if !skill.is_empty() { state.invoked.insert(skill.to_owned()); state.requested.insert(skill.to_owned()); } return; }
        for capture in self.expanded.captures_iter(text) { if let Some(skill) = capture.get(1).map(|name| name.as_str().trim()).filter(|name| !name.is_empty()) { state.invoked.insert(skill.to_owned()); state.requested.insert(skill.to_owned()); } }
        let mut visible = text.to_owned(); for pattern in &self.injected { visible = pattern.replace_all(&visible, "").into_owned(); }
        if self.requested.is_match(&visible) || self.own_words.iter().any(|pattern| pattern.is_match(&visible)) { state.requested.insert("ulw-plan".into()); }
    }
    pub fn tool_result(&self, session: &str, tool: &str, input: &Value, is_error: bool) {
        if session.is_empty() || is_error { return; }
        let path = input.get("path").and_then(Value::as_str);
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner); let state = sessions.entry(session.to_owned()).or_default();
        if tool == "read" && let Some(capture) = path.and_then(|path| self.skill_path.captures(path)) && let Some(skill) = capture.get(1) { state.invoked.insert(skill.as_str().to_owned()); }
        let paths: BTreeSet<String> = if matches!(tool, "read" | "write" | "edit") { path.filter(|path| self.plan_path.is_match(path)).map(|path| BTreeSet::from([path.replace('\\', "/")])).unwrap_or_default() } else if tool == "apply_patch" { input.get("input").and_then(Value::as_str).map(|text| self.patch_path.find_iter(text).map(|path| path.as_str().replace('\\', "/")).collect()).unwrap_or_default() } else { BTreeSet::new() };
        for path in paths { let mut sequence = self.sequence.lock().unwrap_or_else(PoisonError::into_inner); *sequence += 1; let reference = state.touches.entry(path.clone()).or_insert(PlanArtifactReference { path, count: 0, last_touched_at: 0 }); reference.count += 1; reference.last_touched_at = *sequence; }
    }
    pub fn shutdown(&self, session: &str) { self.sessions.lock().unwrap_or_else(PoisonError::into_inner).remove(session); }
    pub fn register(self: &Arc<Self>, api: &mut ExtensionApi) {
        for kind in [EventKind::Input, EventKind::ToolResult, EventKind::SessionShutdown] {
            let tracker = self.clone();
            api.on(kind, Arc::new(move |event, context| { let tracker = tracker.clone(); let session = context.session_manager.session_id().to_owned(); Box::pin(async move { match event { ExtensionEvent::Input(input) => tracker.input(&session, &input.text, input.source), ExtensionEvent::ToolResult(result) => tracker.tool_result(&session, &result.tool_name, &result.input, result.is_error), ExtensionEvent::SessionShutdown(_) => tracker.shutdown(&session), _ => {} } Ok(EventResult::None) }) }));
        }
    }
}
