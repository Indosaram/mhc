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
    let expression = regex::Regex::new(r"(?i)(?:Python|ruby|julia)[\x09-\x0d\x20\x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]+(?:version[\x09-\x0d\x20\x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]+)?v?([0-9]+(?:\.[0-9]+){1,3})").expect("constant interpreter version regex");
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
    probe: InterpreterProbe,
    resolve: InterpreterResolver,
}

pub type InterpreterProbe = std::sync::Arc<dyn Fn(String,Vec<String>,u64)->std::pin::Pin<Box<dyn std::future::Future<Output=Result<(String,String),String>>+Send>>+Send+Sync>;
pub type InterpreterResolver = std::sync::Arc<dyn Fn(&str)->Option<std::path::PathBuf>+Send+Sync>;

impl InterpreterDetector {
    pub fn new(node_version: String, windows: bool) -> Self {
        Self::with_probes(node_version,windows,std::sync::Arc::new(|command,args,timeout_ms|Box::pin(async move {
            let mut process=tokio::process::Command::new(command);
            process.args(args).kill_on_drop(true);
            let output=tokio::time::timeout(Duration::from_millis(timeout_ms),process.output()).await.map_err(|error|error.to_string())?.map_err(|error|error.to_string())?;
            if !output.status.success() {return Err("interpreter probe failed".into());}
            Ok((String::from_utf8_lossy(&output.stdout).into_owned(),String::from_utf8_lossy(&output.stderr).into_owned()))
        })),std::sync::Arc::new(move |command| {
            let env=std::env::vars().collect();
            std::env::current_dir().ok().and_then(|cwd|resolve_command_path(command,&env,&cwd,windows))
        }))
    }
    pub fn with_probes(node_version:String,windows:bool,probe:InterpreterProbe,resolve:InterpreterResolver)->Self {
        Self {cache:HashMap::new(),node_version,windows,probe,resolve}
    }
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
                let args=words.chain(std::iter::once("--version")).map(str::to_owned).collect();
                if let Ok((stdout,stderr))=(self.probe)(command.into(),args,3000).await
                    && let Some(version)=parse_version(&format!("{stdout}\n{stderr}")) {
                        let resolved_path=(self.resolve)(command);
                        detected = InterpreterDetection::Detected { path: (*candidate).into(), version, resolved_path };
                        break;
                    }
            }
        }
        self.cache.insert(key, detected.clone());
        detected
    }
}
