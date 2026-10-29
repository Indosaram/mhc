#[derive(Clone, Copy)]
pub struct MeasurableCall {
    pub start_ms: f64,
    pub end_ms: f64,
}
pub struct MeasurableWave {
    pub calls: Vec<MeasurableCall>,
    pub span_ms: f64,
    pub max_concurrency: f64,
}
#[derive(Debug, PartialEq)]
pub struct ModeledSavedMs {
    pub label: &'static str,
    pub value_ms: f64,
}
#[derive(Debug, PartialEq)]
pub struct UpperBoundSavedMs {
    pub label: &'static str,
    pub value_ms: f64,
}

fn usable_durations(calls: &[MeasurableCall]) -> Vec<f64> {
    calls
        .iter()
        .filter(|c| c.start_ms.is_finite() && c.end_ms.is_finite() && c.end_ms >= c.start_ms)
        .map(|c| c.end_ms - c.start_ms)
        .collect()
}
pub fn modeled_wall_clock_saved_ms(wave: &MeasurableWave) -> ModeledSavedMs {
    let durations = usable_durations(&wave.calls);
    ModeledSavedMs {
        label: "modeled",
        value_ms: if durations.len() <= 1 || !wave.span_ms.is_finite() {
            0.0
        } else {
            durations.iter().sum::<f64>() - wave.span_ms
        },
    }
}
pub fn upper_bound_saved_ms(wave: &MeasurableWave) -> UpperBoundSavedMs {
    let durations = usable_durations(&wave.calls);
    let count = durations.iter().fold(0.0, |n, _| n + 1.0);
    UpperBoundSavedMs {
        label: "upper_bound",
        value_ms: if durations.len() <= 1 {
            0.0
        } else {
            (count - 1.0) * durations.iter().sum::<f64>() / count
        },
    }
}
pub fn saved_round_trips(waves: &[MeasurableWave]) -> f64 {
    waves
        .iter()
        .filter(|w| w.max_concurrency.is_finite())
        .map(|w| (w.max_concurrency - 1.0).max(0.0))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wave(intervals: &[(f64, f64)], span: f64, peak: f64) -> MeasurableWave {
        MeasurableWave {
            calls: intervals
                .iter()
                .map(|&(start_ms, end_ms)| MeasurableCall { start_ms, end_ms })
                .collect(),
            span_ms: span,
            max_concurrency: peak,
        }
    }
    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 0.00000001, "{a} != {b}");
    }
    #[test]
    fn simultaneous_four() {
        let w = wave(&[(0.0, 2.0), (0.0, 2.2), (0.0, 1.9), (0.0, 2.1)], 2.2, 4.0);
        let s = modeled_wall_clock_saved_ms(&w);
        assert_eq!(s.label, "modeled");
        close(s.value_ms, 6.0);
    }
    #[test]
    fn long_tail() {
        let w = wave(&[(0.0, 0.3), (0.0, 0.3), (0.0, 0.3), (0.0, 9.0)], 9.0, 4.0);
        close(modeled_wall_clock_saved_ms(&w).value_ms, 0.9);
        let s = upper_bound_saved_ms(&w);
        assert_eq!(s.label, "upper_bound");
        close(s.value_ms, 7.425);
    }
    #[test]
    fn single_call() {
        let w = wave(&[(3.0, 11.0)], 8.0, 1.0);
        close(modeled_wall_clock_saved_ms(&w).value_ms, 0.0);
        close(upper_bound_saved_ms(&w).value_ms, 0.0);
        close(saved_round_trips(&[w]), 0.0);
    }
    #[test]
    fn empty() {
        let w = wave(&[], 0.0, 0.0);
        close(modeled_wall_clock_saved_ms(&w).value_ms, 0.0);
        close(upper_bound_saved_ms(&w).value_ms, 0.0);
        close(saved_round_trips(&[w]), 0.0);
        close(saved_round_trips(&[]), 0.0);
    }
    #[test]
    fn negative_not_clamped() {
        close(
            modeled_wall_clock_saved_ms(&wave(&[(0.0, 1.0), (0.0, 1.0)], 10.0, 2.0)).value_ms,
            -8.0,
        );
    }
    #[test]
    fn chained_span() {
        close(
            modeled_wall_clock_saved_ms(&wave(&[(0.0, 5.0), (4.0, 9.0), (8.0, 12.0)], 12.0, 2.0))
                .value_ms,
            2.0,
        );
    }
    #[test]
    fn chained_concurrency() {
        close(
            saved_round_trips(&[wave(&[(0.0, 5.0), (4.0, 9.0), (8.0, 12.0)], 12.0, 2.0)]),
            1.0,
        );
    }
    #[test]
    fn several_waves() {
        close(
            saved_round_trips(&[
                wave(&[], 0.0, 4.0),
                wave(&[], 0.0, 2.0),
                wave(&[], 0.0, 1.0),
            ]),
            4.0,
        );
    }
    #[test]
    fn malformed_intervals() {
        for bad in [(0.0, f64::INFINITY), (10.0, 2.0)] {
            let w = wave(&[bad, (0.0, 4.0)], 4.0, 2.0);
            close(modeled_wall_clock_saved_ms(&w).value_ms, 0.0);
            close(upper_bound_saved_ms(&w).value_ms, 0.0);
        }
        let w = wave(&[(0.0, f64::NAN), (0.0, 4.0), (0.0, 4.0)], 4.0, 3.0);
        close(modeled_wall_clock_saved_ms(&w).value_ms, 4.0);
        close(upper_bound_saved_ms(&w).value_ms, 4.0);
    }
    #[test]
    fn malformed_span_concurrency() {
        close(
            modeled_wall_clock_saved_ms(&wave(&[(0.0, 2.0), (0.0, 2.0)], f64::NAN, 2.0)).value_ms,
            0.0,
        );
        close(saved_round_trips(&[wave(&[], 2.0, f64::NAN)]), 0.0);
    }
    #[test]
    fn shuffled() {
        close(
            modeled_wall_clock_saved_ms(&wave(&[(8.0, 12.0), (0.0, 5.0), (4.0, 9.0)], 12.0, 2.0))
                .value_ms,
            2.0,
        );
    }
    #[test]
    fn repeated() {
        let w = wave(&[(0.0, 0.3), (0.0, 0.3), (0.0, 0.3), (0.0, 9.0)], 9.0, 4.0);
        for _ in 0..5 {
            assert_eq!(
                modeled_wall_clock_saved_ms(&w),
                modeled_wall_clock_saved_ms(&w)
            );
            assert_eq!(upper_bound_saved_ms(&w), upper_bound_saved_ms(&w));
        }
    }
}
