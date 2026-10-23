//! The registration an operator saw and authorized for a hard stop (senpi `host-stop-generation.ts`).
use serde_json::Value;

/// `process_start_time` is `None` where the platform cannot read it (Windows); the key is always present on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostStopGeneration {
    pub instance_id: String,
    pub generation: u64,
    pub pid: u32,
    pub process_start_time: Option<String>,
}

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// `None` on any invalid field; a null start time is allowed.
pub fn parse_host_stop_generation(text: &str) -> Option<HostStopGeneration> {
    let value: Value = serde_json::from_str(text).ok()?;
    let object = value.as_object()?;
    let instance_id = object
        .get("instanceId")?
        .as_str()
        .filter(|id| !id.is_empty())?;
    let generation = safe_integer(object.get("generation")?)?;
    let pid = safe_integer(object.get("pid")?).filter(|pid| *pid > 0)?;
    let process_start_time = match object.get("processStartTime")? {
        Value::Null => None,
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        _ => return None,
    };
    Some(HostStopGeneration {
        instance_id: instance_id.into(),
        generation,
        pid: u32::try_from(pid).ok()?,
        process_start_time,
    })
}
fn safe_integer(value: &Value) -> Option<u64> {
    if let Some(integer) = value.as_u64() {
        return Some(integer).filter(|integer| *integer <= MAX_SAFE_INTEGER);
    }
    let float = value.as_f64()?;
    (float >= 0. && float.fract() == 0. && float <= MAX_SAFE_INTEGER as f64).then_some(float as u64)
}

/// How the current registration compares with the confirmed one.
/// - `Proven`: same generation, the start times agree; the live process is still checked before signalling.
/// - `BoundWithoutStartTime`: same pid, instance and generation on a platform that records no start time.
/// - `Stale`: a different generation, pid or start time holds the endpoint now.
/// - `Unprovable`: same generation, but the start time cannot prove the process (`unknown_owner`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopGenerationMatch {
    Proven,
    BoundWithoutStartTime,
    Stale,
    Unprovable,
}

pub fn compare_stop_generation(
    registered: Option<&crate::host_daemon_registration::RegisteredHost>,
    expected: &HostStopGeneration,
) -> StopGenerationMatch {
    compare_with(
        registered,
        expected,
        crate::host_stop_start_time::process_start_time_is_unreadable(),
    )
}
fn compare_with(
    registered: Option<&crate::host_daemon_registration::RegisteredHost>,
    expected: &HostStopGeneration,
    start_time_unreadable: bool,
) -> StopGenerationMatch {
    let Some(registered) = registered else {
        return StopGenerationMatch::Stale;
    };
    if registered.instance_id != expected.instance_id
        || registered.generation != expected.generation as f64
        || registered.pid != expected.pid
    {
        return StopGenerationMatch::Stale;
    }
    match (&registered.process_start_time, &expected.process_start_time) {
        (Some(recorded), Some(confirmed)) => {
            if recorded == confirmed {
                StopGenerationMatch::Proven
            } else {
                StopGenerationMatch::Stale
            }
        }
        (None, None) if start_time_unreadable => StopGenerationMatch::BoundWithoutStartTime,
        _ => StopGenerationMatch::Unprovable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_daemon_registration::RegisteredHost;
    fn registered(start: Option<&str>) -> RegisteredHost {
        RegisteredHost {
            pid: 42,
            process_start_time: start.map(str::to_owned),
            socket: None,
            instance_id: "id".into(),
            generation: 3.,
            writer: None,
        }
    }
    fn expected(start: Option<&str>) -> HostStopGeneration {
        HostStopGeneration {
            instance_id: "id".into(),
            generation: 3,
            pid: 42,
            process_start_time: start.map(str::to_owned),
        }
    }
    #[test]
    fn parses_a_whole_generation_and_rejects_every_invalid_field() {
        assert_eq!(
            parse_host_stop_generation(
                r#"{"instanceId":"id","generation":3,"pid":42,"processStartTime":"7"}"#
            ),
            Some(expected(Some("7")))
        );
        assert_eq!(
            parse_host_stop_generation(
                r#"{"instanceId":"id","generation":0,"pid":42,"processStartTime":null}"#
            ),
            Some(HostStopGeneration {
                generation: 0,
                ..expected(None)
            })
        );
        for bad in [
            r#"{"instanceId":"","generation":3,"pid":42,"processStartTime":null}"#,
            r#"{"instanceId":"id","generation":-1,"pid":42,"processStartTime":null}"#,
            r#"{"instanceId":"id","generation":1.5,"pid":42,"processStartTime":null}"#,
            r#"{"instanceId":"id","generation":3,"pid":0,"processStartTime":null}"#,
            r#"{"instanceId":"id","generation":3,"pid":42}"#,
            r#"{"instanceId":"id","generation":3,"pid":42,"processStartTime":""}"#,
            "not json",
            "[]",
        ] {
            assert_eq!(parse_host_stop_generation(bad), None, "{bad}");
        }
    }
    #[test]
    fn comparison_distinguishes_stale_proven_bound_and_unprovable() {
        assert_eq!(
            compare_with(None, &expected(Some("7")), false),
            StopGenerationMatch::Stale
        );
        assert_eq!(
            compare_with(
                Some(&RegisteredHost {
                    pid: 43,
                    ..registered(Some("7"))
                }),
                &expected(Some("7")),
                false
            ),
            StopGenerationMatch::Stale
        );
        assert_eq!(
            compare_with(Some(&registered(Some("7"))), &expected(Some("7")), false),
            StopGenerationMatch::Proven
        );
        assert_eq!(
            compare_with(Some(&registered(Some("8"))), &expected(Some("7")), false),
            StopGenerationMatch::Stale
        );
        assert_eq!(
            compare_with(Some(&registered(None)), &expected(None), true),
            StopGenerationMatch::BoundWithoutStartTime
        );
        assert_eq!(
            compare_with(Some(&registered(None)), &expected(None), false),
            StopGenerationMatch::Unprovable
        );
        assert_eq!(
            compare_with(Some(&registered(Some("7"))), &expected(None), true),
            StopGenerationMatch::Unprovable
        );
    }
}
