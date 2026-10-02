use crate::{expose::status::McpServerExposureStatus,service_types::McpServerSnapshot};
pub struct McpStatusRow {pub exposure:McpServerExposureStatus,pub snapshot:McpServerSnapshot}
pub async fn build_mcp_status_rows<F,Fut>(snapshots:Vec<McpServerSnapshot>,get_exposure_status:F)->Vec<McpStatusRow>
where F:Fn(String)->Fut,Fut:std::future::Future<Output=McpServerExposureStatus> {
    futures::future::join_all(snapshots.into_iter().map(|snapshot|{
        let exposure=get_exposure_status(snapshot.name.clone());
        async move {McpStatusRow {exposure:exposure.await,snapshot}}
    })).await
}
pub fn format_mcp_status(title:&str,rows:&[McpStatusRow])->String {
    let mut lines=vec![title.to_owned()];
    for row in rows {
        let snapshot=&row.snapshot;
        let average=if snapshot.counters.call_count==0 {0.0}else{(snapshot.counters.total_latency_ms/(snapshot.counters.call_count as f64)+0.5).floor()};
        let config=serde_json::to_value(snapshot.config_state).unwrap_or_default();
        let lifecycle=serde_json::to_value(snapshot.lifecycle_state).unwrap_or_default();
        let source=snapshot.source.map_or_else(||"n/a".into(),|source|source.to_string());
        let path=snapshot.source_path.as_ref().map_or_else(||"n/a".into(),|path|path.display().to_string());
        let tools=row.exposure.tool_count.map_or_else(||"?".into(),|count|count.to_string());
        let uptime=match snapshot.uptime_ms {None=>"n/a".into(),Some(ms) if ms<1000.0=>"<1s".into(),Some(ms)=>format!("{}s",(ms/1000.0+0.5).floor())};
        let mut line=format!("{} {} state={} origin={} source={} tools={} uptime={} calls={} errors={} latency={}ms reconnects={}",snapshot.name,config.as_str().unwrap_or(""),lifecycle.as_str().unwrap_or(""),source,path,tools,uptime,snapshot.counters.call_count,snapshot.counters.error_count,average,snapshot.counters.reconnect_count);
        if let Some(error)=snapshot.last_error.as_ref().filter(|error|!error.is_empty()){line.push_str(&format!(" lastError={error}"));}
        if let Some(hint)=row.exposure.hint.as_ref().filter(|hint|!hint.is_empty()){line.push_str(&format!(" hint={hint}"));}
        lines.push(line);
    }
    lines.join("\n")
}
