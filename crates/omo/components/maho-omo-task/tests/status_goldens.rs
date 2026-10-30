use std::collections::BTreeMap;
use maho_omo_task::status_row_format::{background_widget_rows,build_widget_rows,format_task_row};
use senpi_task::state::TaskRunStats;
use serde_json::Value;
#[test] fn task_rows_match_pinned_upstream_status_goldens() {
    let fixture:Value=serde_json::from_str(include_str!("golden/status-rows.json")).expect("generated status fixture");
    for case in fixture["cases"].as_array().expect("cases") {
        let record=senpi_task::store::parse_task_record(&case["record"],"status golden",&mut Vec::new()).expect("record"); let records=[record.clone()]; let activity=BTreeMap::from([(record.task_id.clone(),case["activity"].as_str().expect("activity").into())]); let stats:Option<TaskRunStats>=case.get("stats").map(|stats| serde_json::from_value(stats.clone()).expect("stats"));
        assert_eq!(format_task_row(&record, None),case["taskRow"].as_str().expect("task row")); assert_eq!(build_widget_rows(&records),serde_json::from_value::<Vec<String>>(case["staticRows"].clone()).expect("static rows"));
        for render in case["renders"].as_array().expect("renders") { let width=usize::try_from(render["width"].as_u64().expect("width")).expect("width"); let expected:Vec<String>=serde_json::from_value(render["rows"].clone()).expect("rows"); assert_eq!(background_widget_rows(&records,&activity,2250,&|_| stats.clone(),Some(width)),expected,"{} width {width}",case["name"]); }
    }
}

#[test] fn parent_fallback_status_distinguishes_selected_from_observed() {
    use senpi_task::state::*;
    let mut record = create_task_record(TaskRecordInput::default(), Some(1)).unwrap();
    record.parent_model = Some("provider/parent".into());
    record.parent_model_fallback = Some(ParentModelFallback { phase: None, from_model: "provider/child".into(), reason: ParentModelFallbackReason::UsageLimit, at: "now".into(), detail: None, resets: Some("2 days".into()), interrupted_tools: None });
    record.resolved_model = Some(ResolvedModelRecord { provider: "provider".into(), model_id: "parent".into(), display: "provider/parent".into(), source: ResolvedModelSource::Category, variant: None, reasoning_effort: None, reasoning: None, service_tier: None });
    let switching = format_task_row(&record, None); assert!(switching.contains("parent-model:switching, usage limit, resets 2 days"));
    record.effective_model = Some(EffectiveModelRecord { provider: "provider".into(), model_id: "parent".into(), display: "provider/parent".into(), source: EffectiveModelSource::Fallback, switched_by_operator: None, variant: None, reasoning_effort: None, reasoning: None, service_tier: None });
    let observed = format_task_row(&record, None); assert!(observed.contains("parent-model:usage limit, resets 2 days")); assert!(!observed.contains("switching"));
}
