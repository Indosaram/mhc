use super::types::EvalLanguage;
use std::future::Future;
use std::time::Duration;

pub const TIMEOUT_STATE_GRACE_MS: u64 = 5_500;

pub async fn describe_timeout_state<F>(base: &str, pending: Option<F>) -> String
where
    F: Future<Output = (bool, Option<String>)>,
{
    let outcome = match pending {
        Some(pending) => {
            tokio::time::timeout(Duration::from_millis(TIMEOUT_STATE_GRACE_MS), pending)
                .await
                .ok()
        }
        None => None,
    };
    let mut message = match &outcome {
        None => format!(
            "{base} Kernel state may have been lost; re-establish any variables the next cell needs."
        ),
        Some((true, _)) => {
            format!("{base} The kernel was not restarted; variables from earlier cells are kept.")
        }
        Some((false, _)) => {
            format!("{base} The kernel was restarted; variables from earlier cells are lost.")
        }
    };
    if let Some((_, Some(note))) = outcome {
        message.push(' ');
        message.push_str(note.trim());
    }
    message
}

fn language_label(language: EvalLanguage) -> &'static str {
    match language {
        EvalLanguage::Js => "JavaScript worker",
    }
}

pub fn interruption_state_note(
    language: EvalLanguage,
    state_retained: Option<bool>,
) -> Option<String> {
    state_retained.map(|retained| {
        let label = language_label(language);
        if retained {
            format!("The {label} was not restarted; variables from earlier cells are kept.")
        } else {
            format!("The {label} was restarted; variables from earlier cells are lost.")
        }
    })
}

pub fn unknown_interruption_state_note(language: EvalLanguage) -> String {
    format!(
        "{} interrupt outcome is unknown; re-establish any variables the next cell needs.",
        language_label(language)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn timeout_state_messages_match_upstream_wording() {
        let kept =
            describe_timeout_state("Timed out.", Some(std::future::ready((true, None)))).await;
        assert_eq!(
            kept,
            "Timed out. The kernel was not restarted; variables from earlier cells are kept."
        );
        let lost =
            describe_timeout_state("Timed out.", Some(std::future::ready((false, None)))).await;
        assert_eq!(
            lost,
            "Timed out. The kernel was restarted; variables from earlier cells are lost."
        );
        let unknown = describe_timeout_state::<std::future::Ready<(bool, Option<String>)>>(
            "Timed out.",
            None,
        )
        .await;
        assert_eq!(
            unknown,
            "Timed out. Kernel state may have been lost; re-establish any variables the next cell needs."
        );
    }

    #[test]
    fn interruption_notes_match_upstream_wording() {
        assert_eq!(
            interruption_state_note(EvalLanguage::Js, Some(true)).as_deref(),
            Some("The JavaScript worker was not restarted; variables from earlier cells are kept.")
        );
        assert_eq!(
            interruption_state_note(EvalLanguage::Js, Some(false)).as_deref(),
            Some("The JavaScript worker was restarted; variables from earlier cells are lost.")
        );
        assert_eq!(
            unknown_interruption_state_note(EvalLanguage::Js),
            "JavaScript worker interrupt outcome is unknown; re-establish any variables the next cell needs."
        );
    }
}
