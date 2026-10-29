use serde_json::Value;
use std::collections::HashMap;

pub const MAX_TRACKED_CALLS: usize = 2000;
#[derive(Debug, Clone, PartialEq)]
pub struct PairedToolCall {
    pub tool_call_id: String,
    pub tool_name: String,
    pub start_ms: f64,
    pub end_ms: f64,
}
#[derive(Debug, PartialEq)]
pub struct ConcurrencyWave {
    pub calls: Vec<PairedToolCall>,
    pub span_ms: f64,
    pub max_concurrency: i32,
}
#[derive(Debug, Default, PartialEq, Eq)]
pub struct WaveCounters {
    pub observed_calls: usize,
    pub paired_calls: usize,
    pub incomplete: usize,
    pub clock_anomalies: usize,
    pub dropped_calls: usize,
    pub malformed: usize,
}
#[derive(Debug, PartialEq)]
pub struct WaveAssembly {
    pub waves: Vec<ConcurrencyWave>,
    pub counters: WaveCounters,
}

pub fn assemble_waves(observations: &[Value]) -> WaveAssembly {
    let mut counters = WaveCounters::default();
    let mut pending = HashMap::new();
    let mut paired = Vec::new();
    for value in observations {
        let parsed = (|| {
            let kind = value.get("kind")?.as_str()?;
            let id = value.get("toolCallId")?.as_str()?;
            let name = value.get("toolName")?.as_str()?;
            let at = value.get("atMs")?.as_f64()?;
            if !matches!(kind, "start" | "end")
                || id.is_empty()
                || name.is_empty()
                || !at.is_finite()
                || at < 0.0
            {
                return None;
            }
            Some((kind, id, name, at))
        })();
        let Some((kind, id, name, at)) = parsed else {
            counters.malformed += 1;
            continue;
        };
        if kind == "start" {
            counters.observed_calls += 1;
            if paired.len() + pending.len() >= MAX_TRACKED_CALLS {
                counters.dropped_calls += 1;
                continue;
            }
            pending.insert(id.to_owned(), (name.to_owned(), at));
        } else if let Some((tool_name, start_ms)) = pending.remove(id) {
            if at < start_ms {
                counters.clock_anomalies += 1;
                continue;
            }
            counters.paired_calls += 1;
            paired.push(PairedToolCall {
                tool_call_id: id.into(),
                tool_name,
                start_ms,
                end_ms: at,
            });
        }
    }
    counters.incomplete = pending.len();
    paired.sort_by(|a, b| {
        a.start_ms
            .total_cmp(&b.start_ms)
            .then(a.end_ms.total_cmp(&b.end_ms))
    });
    let mut waves = Vec::new();
    let mut current = Vec::new();
    let mut reach = f64::NEG_INFINITY;
    for call in paired {
        if !current.is_empty() && call.start_ms > reach {
            waves.push(build_wave(std::mem::take(&mut current)));
            reach = f64::NEG_INFINITY;
        }
        reach = reach.max(call.end_ms);
        current.push(call);
    }
    if !current.is_empty() {
        waves.push(build_wave(current));
    }
    WaveAssembly { waves, counters }
}
fn build_wave(calls: Vec<PairedToolCall>) -> ConcurrencyWave {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut boundaries = Vec::new();
    for call in &calls {
        min = min.min(call.start_ms);
        max = max.max(call.end_ms);
        boundaries.push((call.start_ms, 1));
        boundaries.push((call.end_ms, -1));
    }
    boundaries.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut active = 0;
    let mut peak = 0;
    for (_, delta) in boundaries {
        active += delta;
        peak = peak.max(active);
    }
    ConcurrencyWave {
        calls,
        span_ms: max - min,
        max_concurrency: peak,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn start(id: &str, at: f64) -> Value {
        json!({"kind":"start","toolCallId":id,"toolName":"bash","atMs":at})
    }
    fn end(id: &str, at: f64) -> Value {
        json!({"kind":"end","toolCallId":id,"toolName":"bash","atMs":at})
    }
    fn pairs(intervals: &[(&str, f64, f64)]) -> Vec<Value> {
        intervals
            .iter()
            .flat_map(|(id, a, b)| [start(id, *a), end(id, *b)])
            .collect()
    }
    fn shape(result: &WaveAssembly) -> Vec<(usize, f64, i32)> {
        result
            .waves
            .iter()
            .map(|w| (w.calls.len(), w.span_ms, w.max_concurrency))
            .collect()
    }
    #[test]
    fn overlapping() {
        let r = assemble_waves(&pairs(&[
            ("a", 0.0, 500.0),
            ("b", 100.0, 600.0),
            ("c", 200.0, 400.0),
        ]));
        assert_eq!(shape(&r), vec![(3, 600.0, 3)]);
        assert_eq!(
            r.waves[0]
                .calls
                .iter()
                .map(|c| c.tool_call_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
    }
    #[test]
    fn sequential() {
        assert_eq!(
            shape(&assemble_waves(&pairs(&[
                ("a", 0.0, 100.0),
                ("b", 200.0, 300.0),
                ("c", 400.0, 500.0)
            ]))),
            vec![(1, 100.0, 1); 3]
        );
    }
    #[test]
    fn mixed() {
        assert_eq!(
            shape(&assemble_waves(&pairs(&[
                ("a", 0.0, 300.0),
                ("b", 100.0, 200.0),
                ("lonely", 900.0, 950.0)
            ]))),
            vec![(2, 300.0, 2), (1, 50.0, 1)]
        );
    }
    #[test]
    fn chained() {
        assert_eq!(
            shape(&assemble_waves(&pairs(&[
                ("a", 0.0, 5.0),
                ("b", 4.0, 9.0),
                ("c", 8.0, 12.0)
            ]))),
            vec![(3, 12.0, 2)]
        );
    }
    #[test]
    fn incomplete() {
        let mut o = pairs(&[("done", 0.0, 100.0)]);
        o.push(start("orphan", 50.0));
        let r = assemble_waves(&o);
        assert_eq!(r.counters.incomplete, 1);
        assert_eq!(r.waves[0].calls[0].tool_call_id, "done");
    }
    #[test]
    fn reversed_clock() {
        let r = assemble_waves(&pairs(&[("sane", 0.0, 100.0), ("reversed", 900.0, 500.0)]));
        assert_eq!(r.counters.clock_anomalies, 1);
        assert_eq!(r.counters.paired_calls, 1);
        assert_eq!(r.waves.len(), 1);
    }
    #[test]
    fn interleaved_cap() {
        let mut o = Vec::new();
        for n in 0_u32..2010 {
            let t = f64::from(n) * 10.0;
            o.extend([start(&n.to_string(), t), end(&n.to_string(), t + 5.0)]);
        }
        let r = assemble_waves(&o);
        assert_eq!(r.counters.paired_calls, MAX_TRACKED_CALLS);
        assert_eq!(r.counters.dropped_calls, 10);
        assert_eq!(r.counters.observed_calls, 2010);
    }
    #[test]
    fn parallel_cap() {
        let mut o: Vec<_> = (0_u32..2500)
            .map(|n| start(&n.to_string(), f64::from(n)))
            .collect();
        o.extend((0_u32..2500).map(|n| end(&n.to_string(), 2500.0 + f64::from(n))));
        let r = assemble_waves(&o);
        assert_eq!(r.counters.paired_calls, MAX_TRACKED_CALLS);
        assert_eq!(r.counters.dropped_calls, 500);
        assert_eq!(r.counters.observed_calls, 2500);
    }
    #[test]
    fn pending_cap() {
        let o: Vec<_> = (0_u32..2500)
            .map(|n| start(&n.to_string(), f64::from(n)))
            .collect();
        let r = assemble_waves(&o);
        assert_eq!(r.counters.incomplete, MAX_TRACKED_CALLS);
        assert_eq!(r.counters.dropped_calls, 500);
        assert_eq!(
            r.counters.paired_calls + r.counters.incomplete + r.counters.dropped_calls,
            2500
        );
    }
    #[test]
    fn malformed() {
        let mut o = vec![
            json!({"kind":"start","toolCallId":42,"toolName":"bash","atMs":0}),
            start("nan", f64::NAN),
            end("nan", 10.0),
            start("negative", -5.0),
            end("negative", 10.0),
            start("", 0.0),
            json!({"kind":"end","toolCallId":"missing-ms","toolName":"bash"}),
            Value::Null,
            json!("not-an-observation"),
        ];
        o.extend(pairs(&[("valid", 0.0, 100.0)]));
        let r = assemble_waves(&o);
        assert_eq!(r.counters.malformed, 7);
        assert_eq!(r.counters.paired_calls, 1);
        assert_eq!(shape(&r), vec![(1, 100.0, 1)]);
    }
    #[test]
    fn session_isolation() {
        let mut o = pairs(&[("a", 0.0, 100.0), ("b", 50.0, 150.0)]);
        o.push(start("orphan", 10.0));
        assert_eq!(assemble_waves(&o).counters.incomplete, 1);
        let r = assemble_waves(&pairs(&[("a", 0.0, 10.0)]));
        assert_eq!(r.counters.incomplete, 0);
        assert_eq!(r.counters.observed_calls, 1);
        assert_eq!(shape(&r), vec![(1, 10.0, 1)]);
    }
    #[test]
    fn chronological_order() {
        let r = assemble_waves(&pairs(&[("late", 800.0, 900.0), ("early", 0.0, 100.0)]));
        assert_eq!(r.waves[0].calls[0].tool_call_id, "early");
        assert_eq!(r.waves[1].calls[0].tool_call_id, "late");
    }
    #[test]
    fn touching_zero_length() {
        assert_eq!(
            shape(&assemble_waves(&pairs(&[
                ("a", 0.0, 100.0),
                ("b", 100.0, 100.0)
            ]))),
            vec![(2, 100.0, 1)]
        );
    }
}
