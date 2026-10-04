use super::{completion_contracts::*,entry_renderers::*};
use memory_core::reflection::ReflectionTrigger;
fn trigger(trigger:ReflectionTrigger)->String{match trigger{ReflectionTrigger::StepCount=>"step-count",ReflectionTrigger::Compaction=>"compaction",ReflectionTrigger::Manual=>"manual",ReflectionTrigger::Dream=>"dream"}.into()}
fn fields(fields:Vec<Option<String>>)->String{join_fields(&fields.iter().map(|field|field.as_deref()).collect::<Vec<_>>())}
pub fn reflection_launched_text(entry:&ReflectionLaunchedEntry)->String{format!("memory reflection started run:{} trigger:{} (+{} steps)",normalize_renderer_text(&entry.run_id),trigger(entry.trigger),entry.backlog_steps)}
pub fn launched_spec(entry:&ReflectionLaunchedEntry)->NoticeSpec {
    let trigger=trigger(entry.trigger);let conversations=entry.conversation_ids.len();let phrase=if trigger=="manual"{"triggered manually".into()}else{format!("triggered by {trigger}")};
    NoticeSpec{glyph:"◐".into(),title:fields(vec![Some("Memory reflection started".into()),Some(run_label(&entry.run_id))]),tone:"accent".into(),why:format!("The outcome lands in this transcript when the run settles - {phrase} after {} new step{}.",entry.backlog_steps,if entry.backlog_steps==1{""}else{"s"}),extra:vec![NoticeExtraLine{text:fields(vec![Some(format!("{conversations} conversation{}",if conversations==1{""}else{"s"})),Some(format!("category {}",normalize_renderer_text(&entry.category))),optional_renderer_text(entry.model.as_deref()).map(|model|format!("model {model}")),optional_renderer_text(entry.thinking.as_deref()).map(|thinking|format!("thinking {thinking}"))]),tone:Some("dim".into())}],detail:Some(fields(vec![Some(format!("trigger {trigger}")),Some(format!("identity {}",normalize_renderer_text(&entry.identity))),Some(format!("started {}",normalize_renderer_text(&entry.started_at)))]))}
}
pub fn format_duration(ms:f64)->String{
    if !ms.is_finite()||ms<0.0{return "unknown".into();}
    if ms<1000.0{return format!("{}ms",ms.round());}
    let seconds=ms/1000.0;
    if seconds<60.0{return format!("{seconds:.1}s");}
    let minutes=(seconds/60.0).floor();format!("{minutes}m{:02}s",(seconds-minutes*60.0).round() as u64)
}
pub fn completion_spec(record:&ReflectionCompletionRecord)->NoticeSpec {
    let payoff=fields(vec![record.files_changed.filter(|count|*count>0).map(|count|format!("{count} file{} changed",if count==1{""}else{"s"})),record.merged_commit_sha.as_deref().map(|sha|format!("commit {}",String::from_utf16_lossy(&normalize_renderer_text(sha).encode_utf16().take(7).collect::<Vec<_>>()))),record.duration_ms.map(|ms|format!("took {}",format_duration(ms))),optional_renderer_text(record.reason.as_deref()).map(|reason|format!("reason {reason}")),optional_renderer_text(record.detail.as_deref()).map(|detail|detail_excerpt(&detail))]);
    NoticeSpec{glyph:outcome_glyph(&record.outcome).into(),title:fields(vec![Some(format!("Memory reflection {}",outcome_label(&record.outcome))),Some(run_label(&record.run_id))]),tone:outcome_theme_color(&record.outcome).into(),why:outcome_summary(&record.outcome).into(),extra:if payoff.is_empty(){vec![]}else{vec![NoticeExtraLine{text:payoff,tone:Some(outcome_theme_color(&record.outcome).into())}]},detail:Some(fields(vec![Some(format!("category {}",normalize_renderer_text(&record.category))),Some(format!("identity {}",normalize_renderer_text(&record.identity))),Some(format!("trigger {}",trigger(record.trigger))),optional_renderer_text(record.model.as_deref()).map(|model|format!("model {model}")),optional_renderer_text(record.thinking.as_deref()).map(|thinking|format!("thinking {thinking}"))]))}
}
pub fn summary_spec(summary:&ReflectionCompletionSummary)->NoticeSpec {
    let clean=summary.failed_count==0;let fingerprint=optional_renderer_text(Some(&summary.dominant_fingerprint));
    NoticeSpec{glyph:if clean{"●"}else{"⚠"}.into(),title:format!("Memory reflection · {} older completion{} collapsed",summary.count,if summary.count==1{""}else{"s"}),tone:if clean{"muted"}else{"warning"}.into(),why:if clean{"Delivered while this session was away; none need attention.".into()}else{format!("Delivered while this session was away; {} need attention.",summary.failed_count)},extra:if clean{vec![]}else{fingerprint.into_iter().map(|fingerprint|NoticeExtraLine{text:format!("most common {}",detail_excerpt(&fingerprint)),tone:Some("warning".into())}).collect()},detail:Some(fields(vec![optional_renderer_text(Some(&summary.oldest_iso)).map(|oldest|format!("oldest {oldest}")),optional_renderer_text(Some(&summary.newest_iso)).map(|newest|format!("newest {newest}"))]))}
}
pub type ResolveEntryTheme=std::sync::Arc<dyn Fn(&maho_ext_api::Theme)->std::sync::Arc<dyn EntryRenderTheme>+Send+Sync>;
pub fn register_reflection_completion_renderer(api:&mut maho_ext_api::ExtensionApi,theme:ResolveEntryTheme){
    for kind in [REFLECTION_COMPLETION_ENTRY_TYPE,REFLECTION_LAUNCHED_ENTRY_TYPE,REFLECTION_SUMMARY_ENTRY_TYPE]{
        let theme=theme.clone();
        api.register_entry_renderer(kind,std::sync::Arc::new(move |entry,options,native_theme|{
            let data=entry.data.get("data")?.clone();
            let spec=match kind{
                REFLECTION_COMPLETION_ENTRY_TYPE=>completion_spec(&serde_json::from_value(data).ok()?),
                REFLECTION_LAUNCHED_ENTRY_TYPE=>launched_spec(&serde_json::from_value(data).ok()?),
                _=>summary_spec(&serde_json::from_value(data).ok()?),
            };
            Some(Box::new(NoticeComponent{spec,expanded:options.expanded,theme:theme(native_theme)}))
        }),Default::default());
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Theme;
    impl EntryRenderTheme for Theme{fn fg(&self,tone:&str,text:&str)->String{format!("<{tone}>{text}</{tone}>")}fn italic(&self,text:&str)->String{format!("<italic>{text}</italic>")}}
    #[test]
    fn registered_native_renderer_renders_launch_and_expanded_detail(){
        let mut api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("memory",Default::default(),Default::default()),Default::default(),Default::default(),Default::default());
        register_reflection_completion_renderer(&mut api,std::sync::Arc::new(|_|std::sync::Arc::new(Theme)));
        let data=serde_json::json!({"schemaVersion":1,"runId":"run","identity":"agent","trigger":"manual","category":"quick","conversationIds":["session"],"backlogSteps":1,"startedAt":"now"});
        let entry=maho_ext_api::SessionEntry{id:"entry".into(),parent_id:None,timestamp:"now".into(),kind:"custom".into(),data:serde_json::json!({"data":data})};
        let renderer=&api.registered.entry_renderers[REFLECTION_LAUNCHED_ENTRY_TYPE];
        let mut component=renderer(&entry,&maho_ext_api::EntryRenderOptions{expanded:false},&Default::default()).unwrap();
        assert_eq!(component.render(120).len(),3);
        let mut expanded=renderer(&entry,&maho_ext_api::EntryRenderOptions{expanded:true},&Default::default()).unwrap();assert_eq!(expanded.render(120).len(),4);
        assert!(expanded.render(120)[0].starts_with("<accent>"));
    }
    #[test]
    fn duration_bounds_and_rounding(){assert_eq!(format_duration(f64::NAN),"unknown");assert_eq!(format_duration(-1.0),"unknown");assert_eq!(format_duration(999.5),"1000ms");assert_eq!(format_duration(1500.0),"1.5s");assert_eq!(format_duration(61_000.0),"1m01s");}
}
