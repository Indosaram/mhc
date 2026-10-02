#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveBucket {
    EvalOnly,
    NonEval,
    Mixed,
}
pub struct ClassifiableWave {
    pub tool_names: Vec<String>,
    pub span_ms: f64,
}
#[derive(Debug, Default, PartialEq, Eq)]
pub struct NonEvalWaveCounters {
    pub waves_total: usize,
    pub waves_multi: usize,
    pub joined_calls: usize,
    pub wave_size_histogram: String,
}
#[derive(Debug, Default, PartialEq)]
pub struct WaveBucketSummary {
    pub non_eval: NonEvalWaveCounters,
    pub eval_only_waves: usize,
    pub eval_only_duration_ms: f64,
    pub mixed_waves: usize,
    pub eval_outer_joined_calls: usize,
    pub mixed_non_eval_joined_calls: usize,
}
pub fn is_eval_tool_name(name: &str) -> bool {
    let normalized = name.trim().to_lowercase().replace('-', "_");
    ["eval", "codemode", "code_mode"].iter().any(|suffix| {
        normalized == *suffix
            || ["_", ":", "/"]
                .iter()
                .any(|separator| normalized.ends_with(&format!("{separator}{suffix}")))
    })
}
pub fn classify_wave_bucket(wave: &ClassifiableWave) -> WaveBucket {
    let eval = wave
        .tool_names
        .iter()
        .filter(|name| is_eval_tool_name(name))
        .count();
    if eval == 0 {
        WaveBucket::NonEval
    } else if eval == wave.tool_names.len() {
        WaveBucket::EvalOnly
    } else {
        WaveBucket::Mixed
    }
}
pub fn summarize_wave_buckets(waves: &[ClassifiableWave]) -> WaveBucketSummary {
    let mut result = WaveBucketSummary::default();
    let mut histogram = [0_usize; 8];
    for wave in waves {
        match classify_wave_bucket(wave) {
            WaveBucket::EvalOnly => {
                result.eval_only_waves += 1;
                if wave.span_ms.is_finite() {
                    result.eval_only_duration_ms += wave.span_ms;
                }
                result.eval_outer_joined_calls += wave.tool_names.len();
            }
            WaveBucket::Mixed => {
                result.mixed_waves += 1;
                for name in &wave.tool_names {
                    if is_eval_tool_name(name) {
                        result.eval_outer_joined_calls += 1;
                    } else {
                        result.mixed_non_eval_joined_calls += 1;
                    }
                }
            }
            WaveBucket::NonEval => {
                let size = wave.tool_names.len();
                result.non_eval.waves_total += 1;
                result.non_eval.joined_calls += size;
                if size > 1 {
                    result.non_eval.waves_multi += 1;
                }
                if size > 0 {
                    let index = [1, 2, 3, 4, 8, 16, 32]
                        .iter()
                        .position(|&max| size <= max)
                        .unwrap_or(7);
                    histogram[index] += 1;
                }
            }
        }
    }
    result.non_eval.wave_size_histogram = histogram
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(":");
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    fn w(names: &[&str], span_ms: f64) -> ClassifiableWave {
        ClassifiableWave {
            tool_names: names.iter().map(|s| (*s).into()).collect(),
            span_ms,
        }
    }
    #[test]
    fn non_eval() {
        assert_eq!(
            classify_wave_bucket(&w(&["bash", "read", "grep"], 0.0)),
            WaveBucket::NonEval
        );
    }
    #[test]
    fn eval_only() {
        for names in [vec!["eval"], vec!["eval", "eval"]] {
            assert_eq!(classify_wave_bucket(&w(&names, 0.0)), WaveBucket::EvalOnly);
        }
    }
    #[test]
    fn mixed() {
        assert_eq!(
            classify_wave_bucket(&w(&["bash", "eval"], 0.0)),
            WaveBucket::Mixed
        );
    }
    #[test]
    fn naming() {
        for n in [
            "eval",
            "codemode",
            "mcp:eval",
            "code-mode",
            "EVAL",
            " eval ",
            "server/eval",
            "tool_eval",
            "code_mode",
        ] {
            assert!(is_eval_tool_name(n));
        }
        for n in [
            "evaluate_foo",
            "evaluate",
            "ln",
            "bash",
            "read",
            "codemodel",
            "eval_helper",
            "",
            "   ",
        ] {
            assert!(!is_eval_tool_name(n));
        }
    }
    #[test]
    fn mixed_does_not_leak() {
        let s =
            summarize_wave_buckets(&[w(&["bash", "eval"], 1200.0), w(&["bash", "read"], 400.0)]);
        assert_eq!(s.non_eval.waves_total, 1);
        assert_eq!(s.non_eval.joined_calls, 2);
        assert_eq!(s.mixed_waves, 1);
        assert_eq!(s.eval_only_waves, 0);
    }
    #[test]
    fn domain_counters() {
        let clean = vec![
            w(&["bash", "read", "grep"], 1.0),
            w(&["bash", "read"], 1.0),
            w(&["read"], 1.0),
        ];
        let baseline = summarize_wave_buckets(&clean);
        let mut polluted = clean;
        polluted.extend([
            w(&["eval"], 1.0),
            w(&["eval", "eval", "eval"], 1.0),
            w(&["bash", "eval"], 1.0),
            w(&["bash", "read", "eval"], 1.0),
        ]);
        let s = summarize_wave_buckets(&polluted);
        assert_eq!(s.non_eval, baseline.non_eval);
        assert_eq!(s.eval_outer_joined_calls, 6);
        assert_eq!(s.mixed_non_eval_joined_calls, 3);
        assert_eq!(s.non_eval.wave_size_histogram, "1:1:1:0:0:0:0:0");
    }
    #[test]
    fn durations() {
        let s = summarize_wave_buckets(&[
            w(&["eval"], 3000.0),
            w(&["codemode"], 500.0),
            w(&["bash", "eval"], 1200.0),
            w(&["bash"], 700.0),
        ]);
        assert_eq!(s.eval_only_waves, 2);
        assert!((s.eval_only_duration_ms - 3500.0).abs() < f64::EPSILON);
        assert_eq!(s.eval_outer_joined_calls, 3);
        assert_eq!(s.mixed_non_eval_joined_calls, 1);
    }
    #[test]
    fn histogram() {
        let waves = [1, 2, 3, 4, 6, 12, 20, 40].map(|size| ClassifiableWave {
            tool_names: vec!["bash".into(); size],
            span_ms: 1.0,
        });
        assert_eq!(
            summarize_wave_buckets(&waves).non_eval.wave_size_histogram,
            "1:1:1:1:1:1:1:1"
        );
    }
    #[test]
    fn malformed() {
        for names in [vec![], vec!["", "   "], vec!["Ｅｖａｌ"]] {
            assert_eq!(classify_wave_bucket(&w(&names, 0.0)), WaveBucket::NonEval);
        }
        assert_eq!(
            classify_wave_bucket(&w(&["코드", "EVAL"], 0.0)),
            WaveBucket::Mixed
        );
        let s = summarize_wave_buckets(&[w(&[], 0.0), w(&[""], 100.0)]);
        assert_eq!(s.non_eval.waves_total, 2);
        assert_eq!(s.non_eval.joined_calls, 1);
    }
    #[test]
    fn stateless() {
        let waves = [
            w(&["bash", "read"], 1.0),
            w(&["eval"], 900.0),
            w(&["bash", "eval"], 1100.0),
        ];
        assert_eq!(
            summarize_wave_buckets(&waves),
            summarize_wave_buckets(&waves)
        );
        assert_eq!(
            summarize_wave_buckets(&[]).non_eval.wave_size_histogram,
            "0:0:0:0:0:0:0:0"
        );
    }
}
