//! Settings → ledger → `mhc usage`, and the opt-outs, without any network.
use maho_telemetry::otlp::{Run, run_payloads};
use maho_telemetry::record::{Ledger, UsageRecord};
use maho_telemetry::{ConfigSwitches, cli, resolve};
use std::collections::HashMap;

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |key: &str| map.get(key).cloned()
}

fn assistant(model: &str) -> serde_json::Value {
    serde_json::json!({
        "role": "assistant", "api": "openai-completions", "provider": "offline", "model": model,
        "usage": {"input": 1200, "output": 300, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 1500,
                  "cost": {"input": 0.0012, "output": 0.003, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0042}},
        "stopReason": "stop", "timestamp": 1_800_000_000_000_i64
    })
}

#[test]
fn ledger_records_feed_the_usage_report() {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = resolve(&env(&[]), ConfigSwitches::default(), dir.path(), "0");
    let ledger = Ledger::new(settings.ledger.clone().expect("ledger on by default"));
    for model in ["m1", "m1", "m2"] {
        let record = UsageRecord::from_assistant(&assistant(model)).expect("record");
        ledger.append(&record).expect("append");
    }
    let text = cli::run(&[], &settings, dir.path()).expect("report");
    assert!(text.contains("Requests: 3 (0 errors)"), "{text}");
    assert!(text.contains("Input Tokens: 3.6K"), "{text}");
    assert!(text.contains("  m1: 2 reqs, $0.0084"), "{text}");
    let json: serde_json::Value = serde_json::from_str(
        &cli::run(&["--json".to_owned()], &settings, dir.path()).expect("json"),
    )
    .expect("parse");
    assert_eq!(json["overall"]["totalRequests"], 3);
    assert_eq!(json["byDay"].as_array().map(Vec::len), Some(1));
}

#[test]
fn opt_outs_leave_nothing_to_export() {
    let dir = tempfile::tempdir().expect("tempdir");
    let endpoint = ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:9");
    let on = resolve(
        &env(&[endpoint]),
        ConfigSwitches::default(),
        dir.path(),
        "0",
    );
    let mut run = Run::start("s", 1);
    run.chat(
        &UsageRecord::from_assistant(&assistant("m")).expect("record"),
        2,
    );
    run.finish(3);
    let config = on.export.as_ref().expect("export on with an endpoint");
    assert_eq!(run_payloads(config, &run).len(), 3);
    for opt_out in [
        ("DO_NOT_TRACK", "1"),
        ("MAHO_TELEMETRY", "0"),
        ("OTEL_SDK_DISABLED", "true"),
    ] {
        let off = resolve(
            &env(&[endpoint, opt_out]),
            ConfigSwitches::default(),
            dir.path(),
            "0",
        );
        assert!(off.export.is_none(), "{opt_out:?}");
    }
}
