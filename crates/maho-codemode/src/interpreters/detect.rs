use super::resolve_command::resolve_command_path;
use crate::tool::types::EvalLanguage;
use std::collections::HashMap;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterpreterDetection {
    Detected { path: String, version: String, resolved_path: Option<std::path::PathBuf> },
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageAvailability {
    pub enabled: bool,
    pub detected: InterpreterDetection,
}

pub type InterpreterAvailability = [(EvalLanguage, LanguageAvailability); 4];

pub async fn get_interpreter_availability(settings: &crate::config::settings::CodemodeSettings, detector: &mut InterpreterDetector) -> InterpreterAvailability {
    let mut availability = [(EvalLanguage::Py, settings.languages.py), (EvalLanguage::Js, settings.languages.js), (EvalLanguage::Rb, settings.languages.rb), (EvalLanguage::Jl, settings.languages.jl)]
        .map(|(language, enabled)| (language, LanguageAvailability { enabled, detected: InterpreterDetection::Unavailable }));
    for (language, status) in &mut availability {
        if status.enabled { status.detected = detector.detect(*language).await; }
    }
    availability
}

pub fn parse_version(output: &str) -> Option<String> {
    let expression = regex::Regex::new(r"(?i)(?:Python|ruby|julia)\s+(?:version\s+)?v?(\d+(?:\.\d+){1,3})").expect("constant interpreter version regex");
    expression.captures(output.trim()).and_then(|captures| captures.get(1)).map(|capture| capture.as_str().into())
}

pub fn candidates_for(language: EvalLanguage, windows: bool) -> &'static [&'static str] {
    match language {
        EvalLanguage::Js => &[],
        EvalLanguage::Py if windows => &["python", "py -3", "python3"],
        EvalLanguage::Py => &["python3", "python"],
        EvalLanguage::Rb => &["ruby"],
        EvalLanguage::Jl => &["julia"],
    }
}

pub struct InterpreterDetector {
    cache: HashMap<String, InterpreterDetection>,
    pub node_version: String,
    pub windows: bool,
}

impl InterpreterDetector {
    pub fn new(node_version: String, windows: bool) -> Self { Self { cache: HashMap::new(), node_version, windows } }
    pub async fn detect(&mut self, language: EvalLanguage) -> InterpreterDetection {
        let key = format!("{language:?}");
        if let Some(cached) = self.cache.get(&key) { return cached.clone(); }
        let mut detected = InterpreterDetection::Unavailable;
        if language == EvalLanguage::Js {
            detected = InterpreterDetection::Detected { path: "node".into(), version: self.node_version.clone(), resolved_path: None };
        } else {
            for candidate in candidates_for(language, self.windows) {
                let mut words = candidate.split(' ');
                let command = words.next().unwrap_or(candidate);
                let mut process = tokio::process::Command::new(command);
                process.args(words).arg("--version").kill_on_drop(true);
                if let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(3), process.output()).await
                    && output.status.success()
                    && let Some(version) = parse_version(&format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))) {
                        let env = std::env::vars().collect();
                        let resolved_path = std::env::current_dir().ok().and_then(|cwd| resolve_command_path(command, &env, &cwd, self.windows));
                        detected = InterpreterDetection::Detected { path: (*candidate).into(), version, resolved_path };
                        break;
                    }
            }
        }
        self.cache.insert(key, detected.clone());
        detected
    }
}
