use crate::{crash_schema::*, process_crash_records::ClaimedCrashRecord};
use serde_json::{Value, json};
fn closed<'a>(value: Option<&'a str>, vocabulary: &[&str], fallback: &'a str) -> &'a str {
    value.filter(|v| vocabulary.contains(v)).unwrap_or(fallback)
}
fn version(value: &Value) -> &str {
    value
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 32
                && s.as_bytes()[0].is_ascii_digit()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".+-".contains(&b))
        })
        .unwrap_or("unknown")
}
pub fn process_crashed_properties(
    crash: &ClaimedCrashRecord,
    os: &dyn telemetry_core::TelemetryOsProvider,
) -> Value {
    let record = &crash.record;
    let detection = record["detection"]
        .as_str()
        .unwrap_or(if crash.source == "rpc-host" {
            "supervisor"
        } else {
            "unknown"
        });
    let uptime = record["uptimeMs"].as_f64().unwrap_or(0.0);
    let bucket = if uptime < 60_000.0 {
        "lt_1m"
    } else if uptime < 600_000.0 {
        "1_10m"
    } else if uptime < 3_600_000.0 {
        "10_60m"
    } else if uptime < 21_600_000.0 {
        "1_6h"
    } else if uptime < 86_400_000.0 {
        "6_24h"
    } else {
        "24h_plus"
    };
    let signal = match record["signal"].as_str() {
        Some(value) => closed(Some(value), CRASH_SIGNALS, "other"),
        None => {
            if detection == "unclean_exit" {
                "unknown"
            } else {
                "none"
            }
        }
    };
    let mut properties = json!({"process_kind":closed(record["kind"].as_str().or(if crash.source=="rpc-host" {Some("rpc-host")} else {None}),PROCESS_KINDS,"unknown"),"shard_kind":crash.shard_kind,"detection":closed(Some(detection),CRASH_DETECTIONS,"unknown"),"signal":signal,"uptime_ms":uptime.round(),"uptime_bucket":bucket,"crashed_bun_version":version(&record["bunVersion"]),"crashed_engine_version":version(&record["senpiVersion"]),"crashed_omo_version":version(&record["productVersion"]),"$os":os.platform(),"arch":os.arch()});
    if let Some(code) = record["code"].as_i64() {
        properties["exit_code"] = json!(code);
    }
    properties
}
