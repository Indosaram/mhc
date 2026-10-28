//! The interactive line-prompt flow (spec §3).

use crate::host::{Choice, SetupHost, resolve_model, resolve_provider};
use crate::steps::{MODEL, PROVIDER, Step};

/// Terminal input and output of the flow. `None` from an `ask` means end of input.
pub trait Prompter {
    fn say(&mut self, text: &str);
    fn ask(&mut self, prompt: &str) -> Option<String>;
    /// Like [`Prompter::ask`] without echo.
    fn ask_secret(&mut self, prompt: &str) -> Option<String>;
}

/// How many models are listed before asking for a typed id.
pub const MODEL_LIST_LIMIT: usize = 20;

/// Why the flow ended without a choice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stop {
    /// End of input: nothing is written.
    Aborted,
    /// No provider or model is available.
    Empty(String),
}

fn pick<T: Clone>(items: &[T], answer: &str, by_name: impl Fn(&str) -> Option<T>) -> Option<T> {
    let answer = answer.trim();
    if let Ok(index) = answer.parse::<usize>() {
        return index.checked_sub(1).and_then(|i| items.get(i)).cloned();
    }
    by_name(answer)
}

/// Runs the selected `steps`; `preset_provider` skips the provider question.
pub fn run(
    host: &dyn SetupHost,
    prompter: &mut dyn Prompter,
    steps: &[Step],
    preset_provider: Option<&str>,
) -> Result<Choice, Stop> {
    let providers = host.providers();
    if providers.is_empty() {
        return Err(Stop::Empty("no provider is known to this build".into()));
    }
    prompter.say("First-run setup: choose a provider and a default model. Ctrl-D cancels.");
    let ask_provider = steps.contains(&PROVIDER);
    let provider = match preset_provider.and_then(|id| resolve_provider(&providers, id)) {
        Some(provider) => provider.clone(),
        None if !ask_provider => providers
            .iter()
            .find(|p| p.has_credential)
            .unwrap_or(&providers[0])
            .clone(),
        None => {
            let mut lines = vec!["Providers:".to_owned()];
            for (i, p) in providers.iter().enumerate() {
                let mark = if p.has_credential { "  ✓" } else { "" };
                let name = if p.name == p.id {
                    p.id.clone()
                } else {
                    format!("{} ({})", p.id, p.name)
                };
                lines.push(format!("  {:>2}. {name}{mark}", i + 1));
            }
            prompter.say(&lines.join("\n"));
            loop {
                let answer = prompter
                    .ask("Provider (number or id): ")
                    .ok_or(Stop::Aborted)?;
                match pick(&providers, &answer, |a| {
                    resolve_provider(&providers, a).cloned()
                }) {
                    Some(p) => break p,
                    None => prompter.say(&format!("unknown provider \"{}\"", answer.trim())),
                }
            }
        }
    };
    let key = if ask_provider && !provider.has_credential {
        let answer = prompter
            .ask_secret(&format!(
                "API key for {} (empty to skip and use an environment key later): ",
                provider.id
            ))
            .ok_or(Stop::Aborted)?;
        Some(answer.trim().to_owned()).filter(|k| !k.is_empty())
    } else {
        None
    };
    let models = host.models(&provider.id);
    if models.is_empty() {
        return Err(Stop::Empty(format!(
            "provider {} has no models",
            provider.id
        )));
    }
    let model = if steps.contains(&MODEL) {
        let shown = &models[..models.len().min(MODEL_LIST_LIMIT)];
        let mut lines = vec![format!("Models of {}:", provider.id)];
        for (i, m) in shown.iter().enumerate() {
            lines.push(format!("  {:>2}. {m}", i + 1));
        }
        if models.len() > shown.len() {
            lines.push(format!(
                "  … {} more; type an id",
                models.len() - shown.len()
            ));
        }
        prompter.say(&lines.join("\n"));
        loop {
            let answer = prompter
                .ask(&format!("Default model [{}]: ", models[0]))
                .ok_or(Stop::Aborted)?;
            if answer.trim().is_empty() {
                break models[0].clone();
            }
            match pick(shown, &answer, |a| resolve_model(&provider.id, &models, a)) {
                Some(m) => break m,
                None => prompter.say(&format!("unknown model \"{}\"", answer.trim())),
            }
        }
    } else {
        models[0].clone()
    };
    prompter.say(&format!(
        "Default model: {}/{model}{}",
        provider.id,
        if key.is_some() { " (key stored)" } else { "" }
    ));
    Ok(Choice {
        provider: provider.id,
        model,
        key,
    })
}

/// A [`Prompter`] over scripted answers, for tests and non-terminal callers.
#[derive(Debug, Default)]
pub struct Scripted {
    pub answers: std::collections::VecDeque<String>,
    pub transcript: Vec<String>,
}

impl Scripted {
    pub fn new(answers: &[&str]) -> Self {
        Self {
            answers: answers.iter().map(|a| (*a).to_owned()).collect(),
            transcript: Vec::new(),
        }
    }
}

impl Prompter for Scripted {
    fn say(&mut self, text: &str) {
        self.transcript.push(text.to_owned());
    }
    fn ask(&mut self, prompt: &str) -> Option<String> {
        self.transcript.push(prompt.to_owned());
        self.answers.pop_front()
    }
    fn ask_secret(&mut self, prompt: &str) -> Option<String> {
        self.ask(prompt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::fake::FakeHost;
    use crate::steps::ALL_STEPS;

    #[test]
    fn numbers_choose_provider_and_model_and_a_key_is_asked_when_missing() {
        let host = FakeHost::sample();
        let mut prompter = Scripted::new(&["2", "sk-9", "1"]);
        let choice = run(&host, &mut prompter, &ALL_STEPS, None).expect("choice");
        assert_eq!(
            choice,
            Choice {
                provider: "beta".into(),
                model: "b1".into(),
                key: Some("sk-9".into())
            }
        );
        assert!(
            prompter
                .transcript
                .iter()
                .any(|l| l.contains("alpha (ALPHA)  ✓"))
        );
    }

    #[test]
    fn ids_work_and_a_credentialed_provider_is_not_asked_for_a_key() {
        let host = FakeHost::sample();
        let mut prompter = Scripted::new(&["bogus", "alpha", "alpha/a2"]);
        let choice = run(&host, &mut prompter, &ALL_STEPS, None).expect("choice");
        assert_eq!(choice.model, "a2");
        assert_eq!(choice.key, None);
        assert!(
            prompter
                .transcript
                .iter()
                .any(|l| l == "unknown provider \"bogus\"")
        );
        assert!(!prompter.transcript.iter().any(|l| l.starts_with("API key")));
    }

    #[test]
    fn empty_answers_take_defaults_and_skip_the_key() {
        let host = FakeHost::sample();
        let mut prompter = Scripted::new(&["beta", "", ""]);
        let choice = run(&host, &mut prompter, &ALL_STEPS, None).expect("choice");
        assert_eq!((choice.model.as_str(), choice.key), ("b1", None));
    }

    #[test]
    fn end_of_input_aborts() {
        let host = FakeHost::sample();
        for answers in [&[][..], &["beta"][..], &["alpha"][..]] {
            let mut prompter = Scripted::new(answers);
            assert_eq!(
                run(&host, &mut prompter, &ALL_STEPS, None),
                Err(Stop::Aborted),
                "{answers:?}"
            );
        }
    }

    #[test]
    fn a_preset_provider_is_not_asked() {
        let host = FakeHost::sample();
        let mut prompter = Scripted::new(&["", "1"]);
        let choice = run(&host, &mut prompter, &ALL_STEPS, Some("beta")).expect("choice");
        assert_eq!(choice.provider, "beta");
        assert!(
            !prompter
                .transcript
                .iter()
                .any(|l| l.starts_with("Provider"))
        );
    }

    #[test]
    fn an_empty_catalog_stops() {
        let host = FakeHost::default();
        let mut prompter = Scripted::new(&[]);
        assert!(matches!(
            run(&host, &mut prompter, &ALL_STEPS, None),
            Err(Stop::Empty(_))
        ));
    }
}
