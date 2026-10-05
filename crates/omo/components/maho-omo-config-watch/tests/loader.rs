use std::{collections::BTreeMap, path::PathBuf, sync::{Arc, Mutex}};
use maho_omo_config_resolution::SenpiConfigDiagnostic;
use maho_omo_config_watch::validate::{ConfigWatchValidation, OmoConfigValidator};

fn diagnostic(path: &str, message: &str) -> SenpiConfigDiagnostic {
    SenpiConfigDiagnostic::Config(omo_config_core::OmoConfigDiagnostic {
        kind: "parse", path: path.into(), message: message.into(), issue_paths: Vec::new(),
    })
}

#[test]
fn injected_loader_preserves_rejection_order_until_each_source_is_repaired() {
    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    let current = diagnostics.clone();
    let env = BTreeMap::from([("HOME".into(), "/home/test".into())]);
    let expected_env = env.clone();
    let mut validator = OmoConfigValidator::with_loader("/home/test/project".into(), env,
        Box::new(move |cwd, env| {
            assert_eq!(cwd, "/home/test/project");
            assert_eq!(env, &expected_env);
            current.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
        }));
    let first = diagnostic("/home/test/project/.omo/omo.jsonc", "first");
    let second = diagnostic("/home/test/project/.omo/omo.json", "second");
    *diagnostics.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = vec![first.clone()];
    let changed = [PathBuf::from("/home/test/project/.omo")];
    let ConfigWatchValidation::Rejected { errors } = validator.validate(&changed) else { panic!("rejected") };
    assert_eq!(errors, ["first"]);
    *diagnostics.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = vec![second.clone(), first];
    let ConfigWatchValidation::Rejected { errors } = validator.validate(&changed) else { panic!("rejected") };
    assert_eq!(errors, ["first", "second"]);
    *diagnostics.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = vec![second];
    let ConfigWatchValidation::Rejected { errors } = validator.validate(&[]) else { panic!("sticky rejection") };
    assert_eq!(errors, ["second"]);
    diagnostics.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
    assert!(matches!(validator.validate(&[]), ConfigWatchValidation::Ok));
}

#[test]
fn injected_loader_baselines_existing_error_and_attributes_sibling_deletion() {
    let diagnostics = Arc::new(Mutex::new(vec![diagnostic("/unrelated/.omo/omo.jsonc", "existing")]));
    let current = diagnostics.clone();
    let mut validator = OmoConfigValidator::with_loader("/home/test/project".into(),
        BTreeMap::from([("HOME".into(), "/home/test".into())]), Box::new(move |_, _| current.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()));
    assert!(matches!(validator.validate(&["/unrelated/.omo/omo.jsonc".into()]), ConfigWatchValidation::Ok));
    diagnostics.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(diagnostic("/home/test/project/.omo/omo.json", "sibling"));
    let ConfigWatchValidation::Rejected { errors } = validator.validate(&["/home/test/project/.omo/omo.jsonc".into()]) else { panic!("sibling rejection") };
    assert_eq!(errors, ["sibling"]);
}
