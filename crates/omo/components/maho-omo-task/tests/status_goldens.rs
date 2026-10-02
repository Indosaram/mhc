use std::collections::BTreeMap;
use maho_omo_task::status_row_format::{background_widget_rows,build_widget_rows,format_task_row};
use senpi_task::state::TaskRunStats;
use serde_json::Value;
#[test] fn task_rows_match_pinned_upstream_status_goldens() {
    let fixture:Value=serde_json::from_str(include_str!("golden/status-rows.json")).expect("generated status fixture");
    for case in fixture["cases"].as_array().expect("cases") {
        let record=senpi_task::store::parse_task_record(&case["record"],"status golden",&mut Vec::new()).expect("record"); let records=[record.clone()]; let activity=BTreeMap::from([(record.task_id.clone(),case["activity"].as_str().expect("activity").into())]); let stats:Option<TaskRunStats>=case.get("stats").map(|stats| serde_json::from_value(stats.clone()).expect("stats"));
        assert_eq!(format_task_row(&record),case["taskRow"].as_str().expect("task row")); assert_eq!(build_widget_rows(&records),serde_json::from_value::<Vec<String>>(case["staticRows"].clone()).expect("static rows"));
        for render in case["renders"].as_array().expect("renders") { let width=usize::try_from(render["width"].as_u64().expect("width")).expect("width"); let expected:Vec<String>=serde_json::from_value(render["rows"].clone()).expect("rows"); assert_eq!(background_widget_rows(&records,&activity,2250,&|_| stats.clone(),Some(width)),expected,"{} width {width}",case["name"]); }
    }
}
