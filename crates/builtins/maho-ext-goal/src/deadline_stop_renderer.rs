//! Turn-deadline entry renderer registered by goal/index.ts (senpi 903b8336): the
//! `deadline-stop` entry renders as a "turn stopped on a deadline" notice. The notice text is
//! `buildDeadlineStopNotice` from core/turn-deadline.ts.

/// `TURN_DEADLINE_ENTRY_TYPE` in core/turn-deadline.ts.
pub const TURN_DEADLINE_ENTRY_TYPE: &str = "deadline-stop";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeadlineStop<'a> {
    pub kind: &'a str,
    pub limit_ms: f64,
    pub elapsed_ms: f64,
}

/// `isDeadlineStopData`: a known kind plus numeric `limitMs` and `elapsedMs`.
pub fn deadline_stop_data(data: &serde_json::Value) -> Option<DeadlineStop<'_>> {
    let kind = data.get("kind")?.as_str()?;
    if kind != "inactivity" && kind != "max_runtime" {
        return None;
    }
    Some(DeadlineStop {
        kind,
        limit_ms: data.get("limitMs")?.as_f64()?,
        elapsed_ms: data.get("elapsedMs")?.as_f64()?,
    })
}

fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

pub fn build_deadline_stop_notice(deadline: &DeadlineStop<'_>) -> String {
    let limit = if deadline.limit_ms < 60_000.0 {
        format!(
            "{} s",
            maho_ai::utils::js::number_to_string(js_round(deadline.limit_ms / 1000.0).max(1.0))
        )
    } else {
        format!(
            "{} min",
            maho_ai::utils::js::number_to_string(js_round(deadline.limit_ms / 60_000.0))
        )
    };
    let reason = if deadline.kind == "inactivity" {
        format!("no tool progress for {limit} (turn_inactivity_ms)")
    } else {
        format!("turn hit its maximum runtime of {limit} (turn_max_runtime_ms)")
    };
    format!(
        "turn stopped: {reason}. Action: send a message to continue, or raise the limit in settings"
    )
}

pub fn render_deadline_stop_entry() -> maho_ext_api::EntryRenderer {
    maho_ext_host::notice::adapters::notice_entry_renderer(|entry| {
        let data = deadline_stop_data(entry.data.get("data")?)?;
        Some(maho_ext_host::notice::spec::NoticeSpec {
            title: "turn stopped on a deadline".into(),
            tone: None,
            why: build_deadline_stop_notice(&data),
            extra: Vec::new(),
            expanded_line: None,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn notice(data: serde_json::Value) -> Option<String> {
        deadline_stop_data(&data).map(|data| build_deadline_stop_notice(&data))
    }

    #[test]
    fn notice_formats_seconds_minutes_and_kind() {
        assert_eq!(
            notice(json!({"kind":"inactivity","limitMs":3_600_000,"elapsedMs":3_600_100})).unwrap(),
            "turn stopped: no tool progress for 60 min (turn_inactivity_ms). Action: send a message to continue, or raise the limit in settings"
        );
        assert_eq!(
            notice(json!({"kind":"max_runtime","limitMs":45_500,"elapsedMs":46_000})).unwrap(),
            "turn stopped: turn hit its maximum runtime of 46 s (turn_max_runtime_ms). Action: send a message to continue, or raise the limit in settings"
        );
        assert!(
            notice(json!({"kind":"inactivity","limitMs":200,"elapsedMs":1}))
                .unwrap()
                .contains("for 1 s ")
        );
    }

    #[test]
    fn invalid_data_renders_nothing() {
        assert_eq!(
            notice(json!({"kind":"other","limitMs":1,"elapsedMs":1})),
            None
        );
        assert_eq!(
            notice(json!({"kind":"inactivity","limitMs":"1","elapsedMs":1})),
            None
        );
        assert_eq!(notice(json!({"kind":"inactivity","limitMs":1})), None);
    }
}
