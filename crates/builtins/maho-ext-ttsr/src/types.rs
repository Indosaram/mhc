use std::collections::BTreeMap;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TtsrStreamSource { Text, Thinking, Tool }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TtsrToolScope { pub tool_name: String, pub path_glob: Option<String> }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TtsrScope { pub allow_text: bool, pub allow_thinking: bool, pub tool_scopes: Vec<TtsrToolScope> }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TtsrInterruptMode { Always, Never }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleSource { Builtin, Project, Global }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TtsrRule { pub name: String, pub path: Option<String>, pub content: String, pub description: Option<String>, pub globs: Option<Vec<String>>, pub condition: Vec<String>, pub scope: TtsrScope, pub interrupt_mode: TtsrInterruptMode, pub source: RuleSource }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepeatMode { Once, AfterGap }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TtsrSettings { pub enabled: bool, pub repeat_mode: RepeatMode, pub repeat_gap: u64, pub disabled_rules: Vec<String> }
impl Default for TtsrSettings { fn default() -> Self { Self { enabled: true, repeat_mode: RepeatMode::Once, repeat_gap: 10, disabled_rules: Vec::new() } } }
pub struct DetectorContext { pub source: TtsrStreamSource, pub stream_key: String, pub generation: u64 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetectorRule { CollapseRepetition, ControlTokenLeak, RepetitiveTurns }
#[derive(Clone, Debug, PartialEq)]
pub enum DetailValue { String(String), Number(f64), Boolean(bool) }
#[derive(Clone, Debug, PartialEq)]
pub struct DetectorMatch { pub rule: DetectorRule, pub reason: String, pub anomaly_start_offset: usize, pub garbage_start_offset: usize, pub detail: BTreeMap<String, DetailValue> }
pub trait StreamDetector<State> {
    fn create_state(&self) -> State;
    fn check_delta(&self, state: &mut State, delta: &[u16], ctx: &DetectorContext) -> Option<DetectorMatch>;
    fn flush(&self, _state: &mut State, _ctx: &DetectorContext) -> Option<DetectorMatch> { None }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TtsrContextMode { Keep, Truncate, Discard }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetectionOwner { CollapseRepetition, ControlTokenLeak }
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleRemediation {
    Nudge { context_mode: TtsrContextMode, nudge_key: String },
    ProviderError,
}
impl RuleRemediation {
    pub const fn context_mode(&self) -> TtsrContextMode { match self { Self::Nudge { context_mode, .. } => *context_mode, Self::ProviderError => TtsrContextMode::Discard } }
    pub const fn corruption_scope(&self) -> &'static str { match self { Self::Nudge { .. } => "output-region", Self::ProviderError => "generation" } }
    pub const fn retry_mode(&self) -> &'static str { match self { Self::Nudge { .. } => "nudge", Self::ProviderError => "provider-error" } }
    pub const fn error_kind(&self) -> Option<&'static str> { match self { Self::Nudge { .. } => None, Self::ProviderError => Some("control-token-leak") } }
}
pub fn collapse_remediation() -> RuleRemediation { RuleRemediation::Nudge { context_mode: TtsrContextMode::Truncate, nudge_key: "collapse-repetition".into() } }
pub const CONTROL_LEAK_REMEDIATION: RuleRemediation = RuleRemediation::ProviderError;
#[derive(Clone, Debug, PartialEq)]
pub struct DetectionResolution { pub owner: DetectionOwner, pub observed_rules: Vec<DetectionOwner>, pub detection: DetectorMatch, pub remediation: RuleRemediation }
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GenerationDetectionState { pub abort_claimed: bool, pub abort_owner: Option<DetectionOwner>, pub self_abort_at: Option<u64>, pub user_cancelled: bool }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TtsrInjectionRecord { pub rules: Vec<String>, pub owner: DetectionOwner, pub remediation: String, pub at: u64 }
pub const TTSR_INJECTION_CUSTOM_TYPE: &str = "ttsr-injection";
