use maho_codemode::interpreters::detect::*;
use maho_codemode::interpreters::resolve_command::*;
use maho_codemode::tool::types::EvalLanguage;

#[test]
fn parses_python_ruby_julia_versions() {
    for (input, expected) in [("Python 3.12.4", "3.12.4"), ("ruby 3.4.10", "3.4.10"), ("julia version 1.12.6", "1.12.6"), ("JULIA version v1.9.1", "1.9.1")] { assert_eq!(parse_version(input).as_deref(), Some(expected)); }
    assert!(parse_version("").is_none());
}
#[test]
fn platform_candidate_order() {
    assert_eq!(candidates_for(EvalLanguage::Py, true), ["python", "py -3", "python3"]);
    assert_eq!(candidates_for(EvalLanguage::Py, false), ["python3", "python"]);
}
#[tokio::test]
async fn caches_real_python_detection() {
    let mut detector = InterpreterDetector::new("24.1.0".into(), false);
    let first = detector.detect(EvalLanguage::Py).await;
    assert!(matches!(first, InterpreterDetection::Detected { .. }));
    assert_eq!(first, detector.detect(EvalLanguage::Py).await);
}
#[tokio::test]
async fn javascript_uses_host_version() {
    let mut detector = InterpreterDetector::new("24.1.0".into(), false);
    assert_eq!(detector.detect(EvalLanguage::Js).await, InterpreterDetection::Detected { path: "node".into(), version: "24.1.0".into(), resolved_path: None });
}

#[tokio::test]
async fn injected_probes_preserve_candidate_arguments_cache_and_resolution() {
    use std::sync::{Arc,Mutex};
    let calls=Arc::new(Mutex::new(Vec::new()));let observed=calls.clone();
    let mut detector=InterpreterDetector::with_probes("24.1.0".into(),true,Arc::new(move |command,args,budget| {
        observed.lock().unwrap().push((command.clone(),args,budget));
        Box::pin(async move {if command=="python" {Err("missing".into())} else {Ok((String::new(),"Python 3.12.4".into()))}})
    }),Arc::new(|command|Some(std::path::PathBuf::from(format!("/fixture/{command}")))));
    let detected=detector.detect(EvalLanguage::Py).await;
    assert_eq!(detected,InterpreterDetection::Detected {path:"py -3".into(),version:"3.12.4".into(),resolved_path:Some("/fixture/py".into())});
    assert_eq!(detector.detect(EvalLanguage::Py).await,detected);
    detector.detect(EvalLanguage::Js).await;
    assert_eq!(*calls.lock().unwrap(),vec![("python".into(),vec!["--version".into()],3000),("py".into(),vec!["-3".into(),"--version".into()],3000)]);
}
#[test]
fn resolves_python_on_path() {
    let env = std::env::vars().collect();
    assert!(resolve_command_path("python3", &env, std::path::Path::new("/tmp"), false).is_some());
    assert!(resolve_command_path("", &env, std::path::Path::new("/tmp"), false).is_none());
}
#[test]
fn ignores_non_executable_and_directories() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("python"), "not executable").unwrap();
    let env = std::collections::HashMap::from([("PATH".into(), root.path().to_string_lossy().into_owned())]);
    assert!(resolve_command_path("python", &env, root.path(), false).is_none());
    assert!(resolve_command_path(&root.path().to_string_lossy(), &env, root.path(), false).is_none());
}

#[tokio::test]
async fn disabled_languages_are_unavailable_without_probing() {
    let mut settings = maho_codemode::config::settings::CodemodeSettings::default();
    settings.languages = maho_codemode::config::settings::Languages { py: false, js: true, rb: false, jl: false };
    let mut detector = InterpreterDetector::new("24.1.0".into(), false);
    let availability = get_interpreter_availability(&settings, &mut detector).await;
    assert_eq!(availability.each_ref().map(|(language, _)| *language), [EvalLanguage::Py, EvalLanguage::Js, EvalLanguage::Rb, EvalLanguage::Jl]);
    for (language, status) in availability {
        if language == EvalLanguage::Js {
            assert!(status.enabled);
            assert_eq!(status.detected, InterpreterDetection::Detected { path: "node".into(), version: "24.1.0".into(), resolved_path: None });
        } else {
            assert!(!status.enabled);
            assert_eq!(status.detected, InterpreterDetection::Unavailable);
        }
    }
}
