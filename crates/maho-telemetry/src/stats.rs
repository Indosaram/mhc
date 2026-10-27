//! Usage aggregation and the summary report. Port of oh-my-pi `packages/stats/src/rollup.ts`
//! (`toAggregatedStats` formulas), `index.ts` (`printStatsSummary`, `formatCost`),
//! `client/data/formatters.ts` (`formatErrorRate`) and `packages/utils/src/format.ts`
//! (`formatNumber`, `formatDuration`, `formatPercent`), over in-memory records instead of SQLite
//! rollups.
use crate::record::UsageRecord;
use serde::Serialize;
use std::collections::BTreeMap;

const DAY_MS: i64 = 86_400_000;

/// Aggregate statistics over a set of records (oh-my-pi `AggregatedStats`, reduced).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Aggregated {
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub error_rate: f64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_cache_write_tokens: u64,
    pub total_tokens: u64,
    pub cache_rate: f64,
    pub cache_savings: f64,
    pub total_cost: f64,
    pub unpriced_requests: u64,
    pub avg_duration_ms: Option<f64>,
    pub avg_ttft_ms: Option<f64>,
    pub avg_tokens_per_second: Option<f64>,
    pub first_timestamp: i64,
    pub last_timestamp: i64,
}

#[derive(Default)]
struct Accumulator {
    requests: u64,
    failed: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    total_tokens: u64,
    cost: f64,
    unpriced: u64,
    uncached_prompt: f64,
    actual_prompt: f64,
    duration: (f64, u64),
    ttft: (f64, u64),
    tps: (f64, u64),
    first: Option<i64>,
    last: Option<i64>,
}

fn mean((sum, n): (f64, u64)) -> Option<f64> {
    (n > 0).then(|| sum / n as f64)
}

impl Accumulator {
    fn add(&mut self, record: &UsageRecord) {
        self.requests += 1;
        self.failed += u64::from(record.failed());
        self.input += record.input;
        self.output += record.output;
        self.cache_read += record.cache_read;
        self.cache_write += record.cache_write;
        self.total_tokens += record.total_tokens;
        let cost = record.cost.unwrap_or_default();
        self.cost += cost.total;
        if record.total_tokens > 0 && cost.total == 0.0 {
            self.unpriced += 1;
        }
        if record.input > 0 && cost.input > 0.0 {
            let rate = cost.input / record.input as f64;
            self.uncached_prompt +=
                rate * (record.input + record.cache_read + record.cache_write) as f64;
            self.actual_prompt += cost.input + cost.cache_read + cost.cache_write;
        }
        if let Some(duration) = record.duration_ms {
            self.duration.0 += duration as f64;
            self.duration.1 += 1;
            if duration > 0 {
                self.tps.0 += record.output as f64 * 1000.0 / duration as f64;
                self.tps.1 += 1;
            }
        }
        if let Some(ttft) = record.ttft_ms {
            self.ttft.0 += ttft as f64;
            self.ttft.1 += 1;
        }
        self.first = Some(
            self.first
                .map_or(record.timestamp, |t| t.min(record.timestamp)),
        );
        self.last = Some(
            self.last
                .map_or(record.timestamp, |t| t.max(record.timestamp)),
        );
    }

    fn finish(&self) -> Aggregated {
        let ratio = |num: f64, den: f64| if den > 0.0 { num / den } else { 0.0 };
        Aggregated {
            total_requests: self.requests,
            successful_requests: self.requests - self.failed,
            failed_requests: self.failed,
            error_rate: ratio(self.failed as f64, self.requests as f64),
            total_input_tokens: self.input,
            total_output_tokens: self.output,
            total_cache_read_tokens: self.cache_read,
            total_cache_write_tokens: self.cache_write,
            total_tokens: self.total_tokens,
            cache_rate: ratio(
                self.cache_read as f64,
                (self.input + self.cache_read) as f64,
            ),
            cache_savings: ratio(
                self.uncached_prompt - self.actual_prompt,
                self.uncached_prompt,
            ),
            total_cost: self.cost,
            unpriced_requests: self.unpriced,
            avg_duration_ms: mean(self.duration),
            avg_ttft_ms: mean(self.ttft),
            avg_tokens_per_second: mean(self.tps),
            first_timestamp: self.first.unwrap_or(0),
            last_timestamp: self.last.unwrap_or(0),
        }
    }
}

/// Aggregates one set of records.
pub fn aggregate<'a>(records: impl IntoIterator<Item = &'a UsageRecord>) -> Aggregated {
    let mut accumulator = Accumulator::default();
    for record in records {
        accumulator.add(record);
    }
    accumulator.finish()
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStats {
    pub provider: String,
    pub model: String,
    #[serde(flatten)]
    pub stats: Aggregated,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderStats {
    pub folder: String,
    #[serde(flatten)]
    pub stats: Aggregated,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayPoint {
    /// UTC day start, ms since epoch.
    pub timestamp: i64,
    pub requests: u64,
    pub errors: u64,
    pub tokens: u64,
    pub cost: f64,
}

/// The `mhc usage` report.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub source: String,
    pub records: usize,
    pub overall: Aggregated,
    pub by_model: Vec<ModelStats>,
    pub by_folder: Vec<FolderStats>,
    pub by_day: Vec<DayPoint>,
}

fn by_cost_then_requests(a: &Aggregated, b: &Aggregated) -> std::cmp::Ordering {
    b.total_cost
        .total_cmp(&a.total_cost)
        .then(b.total_requests.cmp(&a.total_requests))
}

/// Builds the report; `since_ms` keeps records with `timestamp >= since_ms`.
pub fn report(source: &str, records: &[UsageRecord], since_ms: Option<i64>) -> Report {
    let kept: Vec<&UsageRecord> = records
        .iter()
        .filter(|record| since_ms.is_none_or(|since| record.timestamp >= since))
        .collect();
    let mut models: BTreeMap<(String, String), Vec<&UsageRecord>> = BTreeMap::new();
    let mut folders: BTreeMap<String, Vec<&UsageRecord>> = BTreeMap::new();
    let mut days: BTreeMap<i64, Vec<&UsageRecord>> = BTreeMap::new();
    for record in &kept {
        models
            .entry((record.provider.clone(), record.model.clone()))
            .or_default()
            .push(record);
        folders.entry(record.cwd.clone()).or_default().push(record);
        days.entry(record.timestamp.div_euclid(DAY_MS) * DAY_MS)
            .or_default()
            .push(record);
    }
    let mut by_model: Vec<ModelStats> = models
        .into_iter()
        .map(|((provider, model), records)| ModelStats {
            provider,
            model,
            stats: aggregate(records),
        })
        .collect();
    by_model.sort_by(|a, b| by_cost_then_requests(&a.stats, &b.stats));
    let mut by_folder: Vec<FolderStats> = folders
        .into_iter()
        .map(|(folder, records)| FolderStats {
            folder,
            stats: aggregate(records),
        })
        .collect();
    by_folder.sort_by(|a, b| by_cost_then_requests(&a.stats, &b.stats));
    let by_day = days
        .into_iter()
        .map(|(timestamp, records)| {
            let stats = aggregate(records);
            DayPoint {
                timestamp,
                requests: stats.total_requests,
                errors: stats.failed_requests,
                tokens: stats.total_tokens,
                cost: stats.total_cost,
            }
        })
        .collect();
    Report {
        source: source.to_owned(),
        records: kept.len(),
        overall: aggregate(kept.iter().copied()),
        by_model,
        by_folder,
        by_day,
    }
}

fn trim1(value: f64) -> String {
    let text = format!("{value:.1}");
    text.strip_suffix(".0").map_or(text.clone(), str::to_owned)
}

/// oh-my-pi `formatNumber`: `999`, `1.5K`, `25K`, `1.5M`, `25M`, `1.5B`.
pub fn format_number(n: u64) -> String {
    let value = n as f64;
    match n {
        0..1_000 => n.to_string(),
        1_000..10_000 => format!("{}K", trim1(value / 1e3)),
        10_000..1_000_000 => format!("{}K", (value / 1e3).round()),
        1_000_000..10_000_000 => format!("{}M", trim1(value / 1e6)),
        10_000_000..1_000_000_000 => format!("{}M", (value / 1e6).round()),
        1_000_000_000..10_000_000_000 => format!("{}B", trim1(value / 1e9)),
        _ => format!("{}B", (value / 1e9).round()),
    }
}

/// `x.y%`.
pub fn format_percent(ratio: f64) -> String {
    format!("{:.1}%", ratio * 100.0)
}

/// oh-my-pi `formatErrorRate`: small non-zero rates are not rounded to zero.
pub fn format_error_rate(ratio: f64) -> String {
    let percent = ratio * 100.0;
    if percent > 0.0 && percent < 0.005 {
        return "<0.01%".to_owned();
    }
    if percent > 0.0 && percent < 0.1 {
        return format!("{percent:.2}%");
    }
    format!("{percent:.1}%")
}

/// oh-my-pi stats `formatCost`: `N/A` for all-unpriced usage.
pub fn format_cost(cost: f64, unpriced: u64) -> String {
    if cost == 0.0 && unpriced > 0 {
        "N/A".to_owned()
    } else if cost < 0.01 {
        format!("${cost:.4}")
    } else if cost < 1.0 {
        format!("${cost:.3}")
    } else {
        format!("${cost:.2}")
    }
}

/// oh-my-pi `formatDuration`: `123ms`, `1.5s`, `30m15s`, `2h30m`, `3d2h`.
pub fn format_duration(ms: f64) -> String {
    const SEC: f64 = 1_000.0;
    const MIN: f64 = 60.0 * SEC;
    const HOUR: f64 = 60.0 * MIN;
    const DAY: f64 = 24.0 * HOUR;
    if !ms.is_finite() || ms <= 0.0 {
        return "0ms".to_owned();
    }
    if ms < SEC {
        return format!("{}ms", ms.floor());
    }
    if ms < MIN {
        let tenths = (ms / 100.0).round();
        return if tenths < 600.0 {
            format!("{:.1}s", tenths / 10.0)
        } else {
            "1m".to_owned()
        };
    }
    let pair = |big: f64, small_unit: f64, a: &str, b: &str| {
        let major = (ms / big).floor();
        let minor = ((ms % big) / small_unit).floor();
        if minor > 0.0 {
            format!("{major}{a}{minor}{b}")
        } else {
            format!("{major}{a}")
        }
    };
    if ms < HOUR {
        pair(MIN, SEC, "m", "s")
    } else if ms < DAY {
        pair(HOUR, MIN, "h", "m")
    } else {
        pair(DAY, HOUR, "d", "h")
    }
}

fn optional_duration(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_owned(), format_duration)
}

/// The summary text (oh-my-pi `printStatsSummary`, without colour or premium requests).
pub fn summary_text(report: &Report) -> String {
    if report.records == 0 {
        return "No usage recorded yet.".to_owned();
    }
    let overall = &report.overall;
    let mut lines = vec![
        "=== AI Usage Statistics ===".to_owned(),
        String::new(),
        "Overall:".to_owned(),
        format!(
            "  Requests: {} ({} errors)",
            format_number(overall.total_requests),
            format_number(overall.failed_requests)
        ),
        format!("  Error Rate: {}", format_error_rate(overall.error_rate)),
        format!(
            "  Total Tokens: {}",
            format_number(overall.total_input_tokens + overall.total_output_tokens)
        ),
        format!(
            "  Input Tokens: {}",
            format_number(overall.total_input_tokens)
        ),
        format!(
            "  Output Tokens: {}",
            format_number(overall.total_output_tokens)
        ),
        format!("  Cache Rate: {}", format_percent(overall.cache_rate)),
        format!("  Cache Savings: {}", format_percent(overall.cache_savings)),
        format!(
            "  API-equivalent estimate: {}",
            format_cost(overall.total_cost, overall.unpriced_requests)
        ),
        format!(
            "  Avg Duration: {}",
            optional_duration(overall.avg_duration_ms)
        ),
        format!("  Avg TTFT: {}", optional_duration(overall.avg_ttft_ms)),
    ];
    if let Some(tps) = overall.avg_tokens_per_second {
        lines.push(format!("  Avg Tokens/s: {tps:.1}"));
    }
    if !report.by_model.is_empty() {
        lines.push(String::new());
        lines.push("By Model (API-equivalent estimates):".to_owned());
        for model in report.by_model.iter().take(10) {
            lines.push(format!(
                "  {}: {} reqs, {}, {} cache rate, {} cache savings",
                model.model,
                format_number(model.stats.total_requests),
                format_cost(model.stats.total_cost, model.stats.unpriced_requests),
                format_percent(model.stats.cache_rate),
                format_percent(model.stats.cache_savings)
            ));
        }
    }
    if !report.by_folder.is_empty() {
        lines.push(String::new());
        lines.push("By Folder (API-equivalent estimates):".to_owned());
        for folder in report.by_folder.iter().take(10) {
            lines.push(format!(
                "  {}: {} reqs, {}",
                folder.folder,
                format_number(folder.stats.total_requests),
                format_cost(folder.stats.total_cost, folder.stats.unpriced_requests)
            ));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::Cost;

    fn record(model: &str, cwd: &str, day: i64, failed: bool) -> UsageRecord {
        UsageRecord {
            v: 1,
            timestamp: day * DAY_MS + 1_000,
            cwd: cwd.into(),
            provider: "p".into(),
            model: model.into(),
            stop_reason: if failed { "error" } else { "stop" }.into(),
            input: 100,
            output: 50,
            cache_read: 300,
            total_tokens: 450,
            cost: Some(Cost {
                input: 0.1,
                output: 0.2,
                cache_read: 0.03,
                cache_write: 0.0,
                total: 0.33,
            }),
            duration_ms: Some(2_000),
            ttft_ms: Some(500),
            ..UsageRecord::default()
        }
    }

    #[test]
    fn aggregate_follows_the_original_formulas() {
        let records = [record("a", "/w", 1, false), record("a", "/w", 1, true)];
        let stats = aggregate(&records);
        assert_eq!(stats.total_requests, 2);
        assert_eq!(stats.failed_requests, 1);
        assert_eq!(stats.error_rate, 0.5);
        assert_eq!(stats.cache_rate, 0.75);
        // uncached = 0.001/token * 400 = 0.4 per record; actual = 0.13 → (0.4-0.13)/0.4
        assert!((stats.cache_savings - 0.675).abs() < 1e-9);
        assert!((stats.total_cost - 0.66).abs() < 1e-9);
        assert_eq!(stats.avg_duration_ms, Some(2_000.0));
        assert_eq!(stats.avg_tokens_per_second, Some(25.0));
        assert_eq!(stats.unpriced_requests, 0);
        let unpriced = UsageRecord {
            total_tokens: 10,
            ..UsageRecord::default()
        };
        assert_eq!(aggregate([&unpriced]).unpriced_requests, 1);
        assert_eq!(aggregate([]).error_rate, 0.0);
    }

    #[test]
    fn report_groups_sorts_and_filters() {
        let mut cheap = record("cheap", "/x", 2, false);
        cheap.cost = None;
        let records = vec![
            record("a", "/w", 1, false),
            record("a", "/w", 2, false),
            cheap,
        ];
        let report = report("ledger", &records, None);
        assert_eq!(report.records, 3);
        assert_eq!(report.by_model[0].model, "a");
        assert_eq!(report.by_folder[0].folder, "/w");
        assert_eq!(report.by_day.len(), 2);
        assert_eq!(report.by_day[1].requests, 2);
        let recent = super::report("ledger", &records, Some(2 * DAY_MS));
        assert_eq!(recent.records, 2);
        let json = serde_json::to_value(&recent).expect("json");
        assert_eq!(json["byModel"][0]["totalRequests"], 1);
        assert!(json["overall"]["cacheRate"].is_number());
        let text = summary_text(&report);
        assert!(text.contains("  Requests: 3 (0 errors)"), "{text}");
        assert!(
            text.contains("  cheap: 1 reqs, N/A, 75.0% cache rate"),
            "{text}"
        );
        assert!(text.contains("  /w: 2 reqs, $0.660"), "{text}");
        assert!(text.contains("  Avg Duration: 2.0s"), "{text}");
        assert_eq!(
            summary_text(&super::report("ledger", &[], None)),
            "No usage recorded yet."
        );
    }

    #[test]
    fn formats_match_the_original_helpers() {
        assert_eq!(format_number(999), "999");
        assert_eq!(format_number(1_000), "1K");
        assert_eq!(format_number(1_500), "1.5K");
        assert_eq!(format_number(25_400), "25K");
        assert_eq!(format_number(1_500_000), "1.5M");
        assert_eq!(format_number(2_500_000_000), "2.5B");
        assert_eq!(format_error_rate(0.00001), "<0.01%");
        assert_eq!(format_error_rate(0.0005), "0.05%");
        assert_eq!(format_error_rate(0.25), "25.0%");
        assert_eq!(format_cost(0.0, 1), "N/A");
        assert_eq!(format_cost(0.0, 0), "$0.0000");
        assert_eq!(format_cost(0.5, 0), "$0.500");
        assert_eq!(format_cost(12.345, 0), "$12.35");
        assert_eq!(format_duration(123.7), "123ms");
        assert_eq!(format_duration(1_500.0), "1.5s");
        assert_eq!(format_duration(59_960.0), "1m");
        assert_eq!(format_duration(1_815_000.0), "30m15s");
        assert_eq!(format_duration(9_000_000.0), "2h30m");
        assert_eq!(format_duration(266_400_000.0), "3d2h");
        assert_eq!(format_duration(3_600_000.0), "1h");
    }
}
