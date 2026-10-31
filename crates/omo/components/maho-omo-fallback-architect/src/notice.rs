pub const FALLBACK_ARCHITECT_NOTICE_TYPE: &str = "omo-fallback-architect:notice";
pub struct NoticeComponent(pub senpi_task::tools::render::LinesView);
impl maho_tui::tui::Component for NoticeComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        senpi_task::tools::render::LinesComponent::render(&self.0, width)
    }
}
// `k3[.-]1` not followed by a digit; checked before the plain K3 label, which also matches `kimi-k3.1-pro`.
fn is_k3_1(lower: &str) -> bool {
    lower.match_indices("k3").any(|(at, _)| {
        let rest = &lower.as_bytes()[at + 2..];
        matches!(rest.first(), Some(b'.' | b'-'))
            && rest.get(1) == Some(&b'1')
            && !rest.get(2).is_some_and(u8::is_ascii_digit)
    })
}
pub fn friendly_model_name(selector: &str) -> String {
    let lower = selector.to_ascii_lowercase();
    if is_k3_1(&lower) {
        return "Kimi K3.1 Pro (max)".into();
    }
    for (pattern, name) in [
        ("kimi-k3", "Kimi K3 (max)"),
        ("claude-fable-5", "Fable 5"),
        ("claude-opus-5", "Opus 5"),
        ("glm-5", "GLM 5.2"),
    ] {
        if lower.contains(pattern) {
            return name.into();
        }
    }
    let id = selector.rsplit('/').next().unwrap_or(selector);
    if id.is_empty() {
        selector.into()
    } else {
        id.into()
    }
}
pub fn build_fallback_architect_notice(from: &str, to: &str) -> String {
    let from_name = friendly_model_name(from);
    let to_name = friendly_model_name(to);
    format!(
        "Model fallback engaged: {from} -> {to}.\n{from_name} declined that one, so {to_name} now drives the session — and top-tier reasoning stays one consult away through task(category: \"architect\"), which still runs Fable 5 at xhigh.\n{to_name} on execution + Fable 5 xhigh on deep reasoning: two frontier brains on one session."
    )
}
pub fn notice_lines(details: Option<(&str, &str)>) -> Vec<String> {
    let Some((from, to)) = details else {
        return vec!["(model fallback upgrade notice)".into()];
    };
    let from = friendly_model_name(from);
    let to = friendly_model_name(to);
    vec!["⚡ Fallback upgrade engaged".into(),format!("{from} declined that one — {to} takes the wheel, zero downtime."),"Top-tier reasoning stays on call: task(category: \"architect\") still consults Fable 5 at xhigh.".into(),format!("{to} on execution + Fable 5 xhigh on hard thinking — two frontier brains, one session.")]
}
