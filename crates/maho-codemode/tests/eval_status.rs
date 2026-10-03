use maho_codemode::{extension::eval_status::*, tool::{detached_cell_contract::EvalDetachedCellStatusEntry, types::EvalLanguage}};

fn entry(id: &str, language: EvalLanguage, summary: Option<&str>, started: f64) -> EvalDetachedCellStatusEntry {
    EvalDetachedCellStatusEntry { cell_id: id.into(), language, summary: summary.map(str::to_owned), started_at_ms: started, queued_behind: None }
}
fn py(summary: Option<&str>) -> EvalDetachedCellStatusEntry { entry("cell-123", EvalLanguage::Py, summary, 1_000_000.0) }
#[test] fn status_key() { assert_eq!(EVAL_CELLS_STATUS_KEY, "eval-cells"); }
#[test] fn empty_clears() { assert_eq!(format_eval_cell_status(&[], 0.0), None); }
#[test] fn single_summary() { assert_eq!(format_eval_cell_status(&[py(Some("numpy feather rerun"))], 1_005_000.0).unwrap(), "↗ py · numpy feather rerun (5s)"); }
#[test] fn missing_summary() { assert_eq!(format_eval_cell_status(&[entry("cell-123", EvalLanguage::Js, None, 1_000_000.0)], 1_180_000.0).unwrap(), "↗ js · cell-123 (3m)"); }
#[test] fn empty_summary() { assert_eq!(format_eval_cell_status(&[py(Some(""))], 1_000_000.0).unwrap(), "↗ py · cell-123 (0s)"); }
#[test] fn long_summary() { let value = format_eval_cell_status(&[py(Some(&"x".repeat(80)))], 1_000_000.0).unwrap(); assert_eq!(value.encode_utf16().count(), 48); assert!(value.ends_with("… (0s)")); }
#[test] fn korean_summary() { let value = format_eval_cell_status(&[py(Some(&"src 전체에서 legacyClient 사용처 집계".repeat(3)))], 1_000_000.0).unwrap(); assert!(value.encode_utf16().count() <= 48); assert!(value.contains("src 전체에서")); assert!(value.ends_with("… (0s)")); }
#[test] fn multiple_fit() { assert_eq!(format_eval_cell_status(&[py(Some("alpha")), py(Some("beta"))], 1_060_000.0).unwrap(), "↗ eval 2: alpha, beta (1m)"); }
#[test] fn whole_label_tail() { assert_eq!(format_eval_cell_status(&[py(Some("first-cell-title")), py(Some("second-cell-title")), py(Some("third-cell-title"))], 1_000_000.0).unwrap(), "↗ eval 3: first-cell-title +2 more (0s)"); }
#[test] fn long_first_keeps_count() { let value = format_eval_cell_status(&[py(Some("a-very-long-cell-title-that-cannot-fit")), py(Some("another-long-cell-title-that-cannot-fit"))], 1_000_000.0).unwrap(); assert!(value.starts_with("↗ eval 2: a-very-long-cell-tit")); assert!(value.ends_with("… +1 more (0s)")); assert!(value.encode_utf16().count() <= 48); }
#[test] fn elapsed_advances() { let values = [py(Some("long running cell"))]; assert!(format_eval_cell_status(&values, 1_005_000.0).unwrap().ends_with("(5s)")); assert!(format_eval_cell_status(&values, 1_006_000.0).unwrap().ends_with("(6s)")); }
#[test] fn oldest_running_cell() { assert_eq!(eval_cell_elapsed_seconds(&[py(Some("alpha")), entry("b", EvalLanguage::Js, Some("beta"), 1_030_000.0)], 1_090_000.0), 90.0); }
#[test] fn backwards_clock() { assert!(format_eval_cell_status(&[py(Some("clock skew"))], 995_000.0).unwrap().ends_with("(0s)")); }
#[test] fn queued_cells_do_not_tick() { let mut value = py(Some("waiting")); value.queued_behind = Some(vec!["first".into()]); assert_eq!(eval_cell_elapsed_seconds(&[value.clone()], 1_090_000.0), 0.0); assert_eq!(format_eval_cell_status(&[value], 1_090_000.0).unwrap(), "↗ py · queued waiting (queued)"); }
#[test] fn elapsed_large_units() { assert_eq!(format_elapsed_seconds(9000.0), "2h 30m"); assert_eq!(format_elapsed_seconds(93780.0), "1d 2h 3m"); }
