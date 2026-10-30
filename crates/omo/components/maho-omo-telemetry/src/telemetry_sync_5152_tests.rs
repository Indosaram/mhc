//! omo v5.1.52 telemetry rows: the bundled-skills override, the opt-out OTLP spool purge, and the
//! schema/vocabulary drift (machine_hash gone, new model ids).

use std::path::{MAIN_SEPARATOR, Path};
use std::sync::{Arc, Mutex};

use telemetry_core::TelemetryEnv;

use crate::product_identity::{ALL_KNOWN_MODEL_IDS, OMO_NATIVE_COMMON_PROPERTY_SCHEMAS};

fn env(pairs: &[(&str, &str)]) -> TelemetryEnv {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

#[test]
fn a_valid_bundled_skills_override_becomes_the_separator_terminated_skills_root() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir");
    let override_dir = dir.path().join("bundled");
    std::fs::create_dir(&override_dir).expect("mkdir");
    let env = env(&[("OMO_BUNDLED_SKILLS_DIR", &override_dir.to_string_lossy())]);
    let reports = Mutex::new(Vec::<String>::new());
    // When
    let root = crate::product_identity::get_builtin_skills_root(
        &env,
        Path::new("/plugin/skills"),
        &|message| reports.lock().unwrap().push(message.to_owned()),
    );
    // Then
    assert_eq!(
        root.to_string_lossy(),
        format!("{}{MAIN_SEPARATOR}", override_dir.to_string_lossy())
    );
    assert!(reports.into_inner().unwrap().is_empty());
}

#[test]
fn an_invalid_bundled_skills_override_falls_back_and_is_reported_once() {
    // Given
    let env = env(&[("OMO_BUNDLED_SKILLS_DIR", "relative/skills-5152")]);
    let reports = Mutex::new(Vec::<String>::new());
    let report = |message: &str| reports.lock().unwrap().push(message.to_owned());
    // When
    let first = crate::product_identity::get_builtin_skills_root(
        &env,
        Path::new("/plugin/skills"),
        &report,
    );
    let second = crate::product_identity::get_builtin_skills_root(
        &env,
        Path::new("/plugin/skills"),
        &report,
    );
    // Then
    let expected = format!("/plugin/skills{MAIN_SEPARATOR}");
    assert_eq!(first.to_string_lossy(), expected);
    assert_eq!(second.to_string_lossy(), expected);
    assert_eq!(
        reports.into_inner().unwrap(),
        [
            "OMO_BUNDLED_SKILLS_DIR=relative/skills-5152 ignored (relative); needs a readable absolute dir"
        ]
    );
}

const BATCH: &str = "batch-logs-0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b.otlp";
const BOOT: &str = "0123456789abcdef0123456789abcdef";

fn spool(agent: &Path) -> std::path::PathBuf {
    let spool = agent.join("omo-senpi").join("otlp");
    std::fs::create_dir_all(&spool).expect("spool");
    std::fs::write(spool.join(BATCH), b"queued").expect("batch");
    spool
}

#[test]
fn an_opted_out_start_purges_the_idle_senpi_otlp_spool() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir");
    let agent = dir.path().join("agent");
    let spool = spool(&agent);
    let env = env(&[("SENPI_CODING_AGENT_DIR", &agent.to_string_lossy())]);
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&events);
    // When
    crate::session_summary_recovery::purge_senpi_otlp_spool_on_opt_out(
        &env,
        Some(Arc::new(|_| false)),
        Some(Arc::new(move |input| {
            seen.lock().unwrap().push(format!("{:?}", input.event))
        })),
    );
    // Then
    assert!(!spool.join(BATCH).exists());
    assert_eq!(*events.lock().unwrap(), Vec::<String>::new());
}

#[test]
fn a_spool_with_a_live_writer_is_left_alone() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir");
    let agent = dir.path().join("agent");
    let spool = spool(&agent);
    std::fs::write(spool.join(format!("active-logs-4242-{BOOT}.otlp")), b"").expect("active");
    let env = env(&[("SENPI_CODING_AGENT_DIR", &agent.to_string_lossy())]);
    // When
    crate::session_summary_recovery::purge_senpi_otlp_spool_on_opt_out(
        &env,
        Some(Arc::new(|pid| pid == 4242)),
        None,
    );
    // Then
    assert!(spool.join(BATCH).exists());
}

#[test]
fn the_common_schema_drops_machine_hash_and_the_vocabulary_knows_the_new_models() {
    let common = OMO_NATIVE_COMMON_PROPERTY_SCHEMAS
        .as_object()
        .expect("common schema");
    assert!(!common.contains_key("omo.machine_hash"));
    assert!(!common.contains_key("machine_hash"));
    for model in [
        "claude-fable-5-5",
        "k3.1-pro",
        "k3.1-pro-highspeed",
        "kimi-k3.1-pro",
        "kimi-k3.1-pro-highspeed",
    ] {
        assert!(ALL_KNOWN_MODEL_IDS.contains(model), "{model}");
    }
}
