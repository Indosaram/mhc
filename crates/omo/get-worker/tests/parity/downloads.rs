//! Port of `test/downloads.test.ts`.

use get_worker::{
    parse_rollup_rows, read_download_stats, rollup_query, route, run_downloads_rollup,
    DownloadStats, FetchResponse, Method, Request,
};

use crate::fakes::{Harness, StaticFetcher};

const MIGRATION: &str = include_str!("../../migrations/0001_downloads.sql");
const ANALYTICS_SQL: &str =
    "https://api.cloudflare.com/client/v4/accounts/account/analytics_engine/sql";

fn row(source: &str, count: f64, day: &str, kind: &str) -> serde_json::Value {
    serde_json::json!({
        "day": day,
        "kind": kind,
        "source": source,
        "version": "5.1.1",
        "asset": "omo-linux-x64",
        "count": count.to_string(),
    })
}

fn answer(rows: &[serde_json::Value]) -> StaticFetcher {
    StaticFetcher::new().with(
        ANALYTICS_SQL,
        FetchResponse::new(200, serde_json::json!({ "data": rows }).to_string()),
    )
}

#[test]
fn rollup_query_covers_whole_utc_days_only_and_weights_by_the_sample_interval() {
    let sql = rollup_query("omo_downloads").expect("a valid dataset");
    assert!(sql.contains("toStartOfDay(NOW() - INTERVAL '6' DAY)"), "{sql}");
    assert!(sql.contains("SUM(_sample_interval * double1)"));
    assert!(sql.contains("blob6 != 'qa'"));
    assert!(rollup_query("omo; DROP TABLE x").is_err());
}

#[test]
fn rejects_a_malformed_analytics_engine_payload_instead_of_storing_zeros() {
    assert!(parse_rollup_rows(&serde_json::json!({ "meta": [] })).is_err());
    let bad = row("r2", 1.0, "yesterday", "binary");
    assert!(parse_rollup_rows(&serde_json::json!({ "data": [bad] })).is_err());
}

#[test]
fn upserts_per_day_so_an_hourly_rerun_replaces_counts_instead_of_adding_them() {
    let h = Harness::new(MIGRATION, StaticFetcher::new());
    let first = answer(&[
        row("r2", 3.0, "2026-09-29", "binary"),
        row("cache", 5.0, "2026-09-29", "binary"),
        row("github", 2.0, "2026-09-29", "binary"),
    ]);
    run_downloads_rollup(h.env(), &first).expect("first rollup");
    let second = answer(&[
        row("r2", 4.0, "2026-09-29", "binary"),
        row("cache", 5.0, "2026-09-29", "binary"),
        row("github", 2.0, "2026-09-29", "binary"),
    ]);
    run_downloads_rollup(h.env(), &second).expect("second rollup");

    let stats = read_download_stats(&h.db).expect("stats");
    assert_eq!(
        stats,
        DownloadStats {
            served_from_mirror: 9,
            redirected_to_git_hub: 2,
            adjustments: 0,
            uncounted_by_git_hub: 9,
            rolled_up_through: Some("2026-09-29".to_string()),
        }
    );
}

#[test]
fn github_redirects_never_add_to_the_uncounted_total_and_adjustments_subtract_qa_installs() {
    let h = Harness::new(MIGRATION, StaticFetcher::new());
    let fetcher = answer(&[
        row("r2", 6.0, "2026-09-29", "binary"),
        row("github", 40.0, "2026-09-29", "binary"),
        row("r2", 9.0, "2026-09-29", "engine"),
    ]);
    run_downloads_rollup(h.env(), &fetcher).expect("rollup");
    h.db.execute("INSERT INTO download_adjustments (delta, reason) VALUES (-4, 'qa installs')")
        .expect("adjustment insert");

    let stats = read_download_stats(&h.db).expect("stats");
    assert_eq!(stats.served_from_mirror, 6);
    assert_eq!(stats.uncounted_by_git_hub, 2);

    let response = route(
        &Request::new(Method::Get, "https://get.omo.dev/stats/downloads"),
        &h.context(),
    )
    .expect("route");
    let body: serde_json::Value =
        serde_json::from_str(&response.body_text()).expect("stats json");
    assert_eq!(body["uncountedByGitHub"], serde_json::json!(2));
    assert_eq!(body["redirectedToGitHub"], serde_json::json!(40));
    assert_eq!(response.headers.get("Cache-Control"), Some("public, max-age=300"));
}

#[test]
fn a_failed_analytics_engine_call_leaves_the_stored_counts_untouched() {
    let h = Harness::new(MIGRATION, StaticFetcher::new());
    let ok = answer(&[row("r2", 7.0, "2026-09-29", "binary")]);
    run_downloads_rollup(h.env(), &ok).expect("rollup");

    let denied = StaticFetcher::new().with(ANALYTICS_SQL, FetchResponse::new(403, "denied"));
    let error = run_downloads_rollup(h.env(), &denied).expect_err("denied");
    assert_eq!(error.message, "Analytics Engine SQL returned 403");
    assert_eq!(read_download_stats(&h.db).expect("stats").served_from_mirror, 7);
}
