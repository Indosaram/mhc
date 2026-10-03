fn main()->Result<(),Box<dyn std::error::Error>> {
    let width=std::env::args().nth(1).map_or(Ok(80),|value|value.parse::<usize>())?;
    let timestamp=std::env::args().nth(2).map_or(Ok(0.0),|value|value.parse::<f64>())?;
    println!("{}",maho_ext_goal::cache_warm::format_wake_timestamp(timestamp).ok_or("invalid timestamp")?);
    let renderer=maho_ext_goal::cache_warm_renderer::render_goal_cache_warmup_entry();
    let mut theme=maho_ext_api::Theme::default();
    for (role,color) in [("accent",6),("dim",8),("success",2)] { theme.colors.insert(role.into(),format!("\x1b[38;5;{color}m")); }
    for phase in ["scheduled","resumed"] {
        let entry=maho_ext_api::SessionEntry { id:"card".into(),parent_id:None,timestamp:String::new(),kind:"custom".into(),data:serde_json::json!({"customType":"goal-cache-warmup","data":{"phase":phase,"goalId":"goal-fixture","iteration":2,"delayMs":270000,"dueAtMs":timestamp,"waitedMs":300000,"activeMonitorCount":2,"cache":{"cachedTokens":120000,"ttlSeconds":300,"estimatedSavedUsd":0.324}}}) };
        for expanded in [false,true] {
            let mut component=renderer(&entry,&maho_ext_api::EntryRenderOptions { expanded },&theme).ok_or("missing component")?;
            for line in component.render(width) { println!("{line}"); }
        }
    }
    Ok(())
}
