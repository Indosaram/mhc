use crate::cache_warm::{GoalCacheWarmupEntryData,GoalCacheWarmupPhase,format_cache_ttl,format_wake_duration,format_warm_token_count,format_saved_usd};
fn entry_data(entry:&maho_ext_api::SessionEntry)->Option<GoalCacheWarmupEntryData> {
    serde_json::from_value(entry.data.get("data").unwrap_or(&entry.data).clone()).ok()
}
pub fn render_goal_cache_warmup_entry()->maho_ext_api::EntryRenderer {
    maho_ext_host::notice::adapters::notice_entry_renderer(|entry| {
        let data=entry_data(entry)?;
        let elapsed=if data.phase==GoalCacheWarmupPhase::Resumed { data.waited_ms.unwrap_or(data.delay_ms) } else { data.delay_ms };
        let expected=data.due_at_ms.filter(|value|value.is_finite()).and_then(crate::cache_warm::format_wake_timestamp).map_or_else(||format!("waited {}",format_wake_duration(elapsed)),|timestamp|format!("ready {timestamp} ({})",format_wake_duration(elapsed)));
        Some(maho_ext_host::notice::spec::NoticeSpec { title:title_line(&data,&expected),tone:None,why:why_line(&data,&expected),extra:warm_line(&data).into_iter().map(|text|maho_ext_host::notice::spec::NoticeLine { text,tone:Some(maho_ext_host::notice::spec::NoticeTone::Success) }).collect(),expanded_line:Some(expanded_line(&data,&expected)) })
    })
}
pub fn cache_warm_entry_replaces()->maho_ext_api::EntryReplaces {
    std::sync::Arc::new(|previous,next|is_same_goal_cache_warm_card(entry_data(previous).as_ref(),entry_data(next).as_ref()))
}
pub fn is_same_goal_cache_warm_card(previous:Option<&GoalCacheWarmupEntryData>,next:Option<&GoalCacheWarmupEntryData>)->bool {
    next.is_some_and(|next|!next.goal_id.is_empty() && previous.is_some_and(|previous|previous.goal_id==next.goal_id))
}
pub fn title_line(data:&GoalCacheWarmupEntryData,expected_wake:&str)->String {
    let sources=if data.active_monitor_count==1.0 { "1 wake source on duty".into() } else { format!("{} wake sources on duty",maho_ai::utils::js::number_to_string(data.active_monitor_count)) };
    let iteration=data.iteration.filter(|value|value.is_finite()&&value.fract()==0.0&&*value>0.0).map_or_else(String::new,|value|format!(" · iteration {}",maho_ai::utils::js::number_to_string(value)));
    match data.phase {
        GoalCacheWarmupPhase::Scheduled=>format!("⚡ Cache-warm wait{iteration} · {sources}"),
        GoalCacheWarmupPhase::Resumed=>format!("⚡ Cache-warm wake{iteration} · {expected_wake} · {sources}"),
    }
}
pub fn warm_line(data:&GoalCacheWarmupEntryData)->Option<String> {
    let cache=data.cache.as_ref()?;
    if cache.cached_tokens<=0.0 { return None; }
    let tokens=format!("~{} tokens",format_warm_token_count(cache.cached_tokens));
    if cache.ttl_seconds.is_some_and(|ttl|data.waited_ms.unwrap_or(data.delay_ms)>=ttl*1000.0) { return Some(format!("{tokens} were cached after the prior turn · prompt-cache TTL may have elapsed before this wake")); }
    let body=match data.phase { GoalCacheWarmupPhase::Scheduled=>format!("{tokens} kept warm"),GoalCacheWarmupPhase::Resumed=>format!("{tokens} stayed warm in the prompt cache") };
    let saved=cache.estimated_saved_usd.filter(|saved|*saved>0.0).map_or_else(String::new,|saved|format!(" · est. {} saved vs a cold re-read",format_saved_usd(saved)));
    Some(format!("{body}{saved}"))
}
pub fn why_line(data:&GoalCacheWarmupEntryData,expected_wake:&str)->String {
    match data.phase {
        GoalCacheWarmupPhase::Resumed=>"Woke on schedule to keep pursuing the goal.".into(),
        GoalCacheWarmupPhase::Scheduled=>{
            let backstop=format!("Stall backstop {expected_wake}");
            match data.cache.as_ref().and_then(|cache|cache.ttl_seconds) {
                None=>format!("{backstop} - the goal resumes as soon as a wake source delivers."),
                Some(ttl) if data.delay_ms<ttl*1000.0=>format!("{backstop} - the goal resumes as soon as a wake source delivers, inside the {} prompt-cache TTL.",format_cache_ttl(ttl)),
                Some(ttl)=>format!("{backstop} - the goal resumes as soon as a wake source delivers; the {} prompt-cache TTL may elapse first.",format_cache_ttl(ttl)),
            }
        },
    }
}
pub fn expanded_line(data:&GoalCacheWarmupEntryData,expected_wake:&str)->String { format!("goal {} · planned delay {} · {expected_wake}",data.goal_id,format_wake_duration(data.delay_ms)) }
#[cfg(test)] mod tests {
    use super::*;
    fn data(id:&str)->GoalCacheWarmupEntryData { serde_json::from_value(serde_json::json!({"phase":"scheduled","goalId":id,"delayMs":1000,"activeMonitorCount":1})).unwrap() }
    #[test] fn registered_notice_adapter_renders_and_replaces_same_goal() {
        let mut api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("goal","/tmp".into(),Default::default()),Default::default(),Default::default(),Default::default());
        let extension=crate::GoalExtension::new(std::sync::Arc::new(|_|crate::GoalStoreRef { base_dir:"/tmp".into(),thread_id:"s".into() }));
        maho_ext_api::Extension::register(&extension,&mut api);
        let entry=|id:&str|maho_ext_api::SessionEntry { id:id.into(),parent_id:None,timestamp:String::new(),kind:"custom".into(),data:serde_json::json!({"customType":crate::cache_warm::GOAL_CACHE_WARMUP_ENTRY_TYPE,"data":{"phase":"scheduled","goalId":id,"delayMs":1000,"activeMonitorCount":1,"cache":{"cachedTokens":1000}}}) };
        let renderer=&api.registered.entry_renderers[crate::cache_warm::GOAL_CACHE_WARMUP_ENTRY_TYPE];
        let mut theme=maho_ext_api::Theme::default(); theme.colors.insert("success".into(),"\x1b[32m".into());
        let mut collapsed=renderer(&entry("g"),&Default::default(),&theme).unwrap(); let lines=collapsed.render(80); assert!(!lines.is_empty()); assert!(lines.iter().any(|line|line.contains("\x1b[32m")));
        let mut expanded=renderer(&entry("g"),&maho_ext_api::EntryRenderOptions { expanded:true },&theme).unwrap(); assert!(expanded.render(80).len()>lines.len());
        let replaces=api.registered.entry_renderer_options[crate::cache_warm::GOAL_CACHE_WARMUP_ENTRY_TYPE].replaces.as_ref().unwrap();
        assert!(replaces(&entry("g"),&entry("g"))); assert!(!replaces(&entry("a"),&entry("g")));
    }
    #[test] fn same_nonempty_goal_id_replaces_card() { assert!(is_same_goal_cache_warm_card(Some(&data("g")),Some(&data("g")))); }
    #[test] fn missing_or_different_goal_never_replaces_card() { assert!(!is_same_goal_cache_warm_card(Some(&data("a")),Some(&data("b")))); assert!(!is_same_goal_cache_warm_card(None,Some(&data("g")))); assert!(!is_same_goal_cache_warm_card(Some(&data("")),Some(&data("")))); }
}
