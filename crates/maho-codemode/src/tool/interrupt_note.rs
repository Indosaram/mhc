use super::types::EvalLanguage;
use std::future::Future;
use std::time::Duration;

pub const TIMEOUT_STATE_GRACE_MS: u64 = 5_500;

pub async fn describe_timeout_state<F>(base: &str, pending: Option<F>) -> String
where F: Future<Output = (bool, Option<String>)> {
    let outcome = match pending {
        Some(pending) => tokio::time::timeout(Duration::from_millis(TIMEOUT_STATE_GRACE_MS), pending).await.ok(),
        None => None,
    };
    let mut message = match &outcome {
        None => format!("{base} Kernel state may have been lost; re-establish any variables the next cell needs."),
        Some((true, _)) => format!("{base} The kernel remains running; its existing variables are preserved."),
        Some((false, _)) => format!("{base} The kernel was unresponsive and restarted; variables from earlier cells are lost."),
    };
    if let Some((_, Some(note))) = outcome { message.push(' '); message.push_str(note.trim()); }
    message
}

fn language_label(language: EvalLanguage) -> &'static str {
    match language { EvalLanguage::Js => "JavaScript worker", EvalLanguage::Py => "Python kernel", EvalLanguage::Rb => "Ruby kernel", EvalLanguage::Jl => "Julia kernel" }
}

pub fn interruption_state_note(language: EvalLanguage, state_retained: Option<bool>) -> Option<String> {
    state_retained.map(|retained| {
        let label = language_label(language);
        if retained { format!("{label} was interrupted and remains running; its existing variables are preserved.") }
        else { format!("{label} was unresponsive to interrupt and was restarted; variables from earlier cells are lost.") }
    })
}

pub fn unknown_interruption_state_note(language: EvalLanguage) -> String {
    format!("{} interrupt outcome is unknown; re-establish any variables the next cell needs.", language_label(language))
}
