use crate::{parse::LoopTarget,types::{CronEntry,LoopPhase,LoopState}};
pub const LOOP_ARGUMENT_HINT:&str="[interval] [prompt] | stop [id|all] | status | pause | resume";
pub const LOOP_COMMAND_DESCRIPTION:&str="Repeat a prompt on a fixed interval or a self-paced schedule (e.g. /loop 5m check the deploy)";
pub const LOOP_HEADLESS_REJECTION:&str="/loop needs an interactive session; it is not available in print mode, so nothing was armed.";
#[derive(Debug,thiserror::Error)]
#[error("Invalid time value")]
pub struct InvalidTimeValue;
fn format_expiry(expires_at:Option<f64>)->Result<String,InvalidTimeValue> {
    let Some(at)=expires_at else { return Ok("after 7 days".into()); };
    if !at.is_finite() || at.abs()>8_640_000_000_000_000.0 { return Err(InvalidTimeValue); }
    let millis=at.trunc() as i64; let days=millis.div_euclid(86400000)+719468;
    let era=days.div_euclid(146097); let day=days-era*146097;
    let year_of_era=(day-day/1460+day/36524-day/146096)/365;
    let day_of_year=day-(365*year_of_era+year_of_era/4-year_of_era/100);
    let month_prime=(5*day_of_year+2)/153; let date=day_of_year-(153*month_prime+2)/5+1;
    let month=month_prime+if month_prime<10 { 3 } else { -9 }; let year=year_of_era+era*400+i64::from(month<=2);
    let year=if (0..=9999).contains(&year) { format!("{year:04}") } else if year<0 { format!("-{:06}",-year) } else { format!("+{year:06}") };
    let time=millis.rem_euclid(86400000);
    Ok(format!("{year}-{month:02}-{date:02}T{:02}:{:02}:{:02}.{:03}Z",time/3600000,time/60000%60,time/1000%60,time%1000))
}
pub fn format_fixed_loop_confirmation(outcome:&crate::index::LoopCreateOk,requested_raw:&str)->Result<String,InvalidTimeValue> {
    let cadence=outcome.effective_cadence.as_deref().unwrap_or("on schedule");
    let parenthetical=if outcome.rounding_notice.is_none() { format!("every {cadence}") } else { format!("every {cadence}; requested {requested_raw}") };
    let mut lines=vec![format!("Loop {} scheduled as `{}` ({parenthetical}).",outcome.loop_id,outcome.cron_expression.as_deref().unwrap_or(""))];
    if let Some(notice)=&outcome.rounding_notice { lines.push(notice.clone()); }
    lines.push(format!("It expires automatically at {} (7 days).",format_expiry(outcome.expires_at)?));
    lines.push(format!("Stop it with `/loop stop {}`.",outcome.loop_id));
    lines.push("Running the first tick now.".into()); Ok(lines.join("\n"))
}
pub fn format_dynamic_loop_confirmation(outcome:&crate::index::LoopCreateOk)->Result<String,InvalidTimeValue> {
    let mut lines=vec![format!("Loop {} started in dynamic mode: the model paces each iteration with `schedule_wakeup`.",outcome.loop_id)];
    if let Some(id)=&outcome.superseded_loop_id { lines.push(format!("Superseded dynamic loop {id}.")); }
    lines.push(format!("It expires automatically at {} (7 days).",format_expiry(outcome.expires_at)?));
    lines.push(format!("Stop it with `/loop stop {}` or a `schedule_wakeup` call with `{{ stop: true }}`.",outcome.loop_id));
    lines.push("Running the first iteration now.".into()); Ok(lines.join("\n"))
}
pub fn format_bare_loop_confirmation(outcome:&crate::index::LoopCreateOk,entry:Option<&CronEntry>)->Result<String,InvalidTimeValue> {
    let mode=match entry { Some(CronEntry::Fixed { effective_interval,.. })=>format!("fixed, every {}",effective_interval.human),Some(CronEntry::Dynamic { .. })|None=>"dynamic pacing".into() };
    Ok([format!("Loop {} started ({mode}).",outcome.loop_id),format!("It expires automatically at {} (7 days).",format_expiry(outcome.expires_at)?),format!("Stop it with `/loop stop {}`.",outcome.loop_id),"Running the first tick now.".into()].join("\n"))
}
pub fn format_loop_status_listing(status_line:Option<&str>,state:&LoopState)->Result<String,InvalidTimeValue> {
    let mut entries=Vec::new();
    for entry in state.entries.values() {
        let (fields,lifecycle,mode)=match entry { CronEntry::Fixed { fields,lifecycle,effective_interval,cron_expression,.. }=>(fields,lifecycle,format!("fixed, `{cron_expression}` (every {})",effective_interval.human)),CronEntry::Dynamic { fields,lifecycle,.. }=>(fields,lifecycle,"dynamic".into()) };
        if lifecycle.phase==LoopPhase::Ended { continue; }
        entries.push(format!("- {} ({mode}) · expires {}{}",fields.id,format_expiry(Some(fields.expires_at))?,if lifecycle.phase==LoopPhase::Suspended { " · paused" } else { "" }));
    }
    if entries.is_empty() { return Ok("No active loops.".into()); }
    let mut lines=Vec::new(); if let Some(status)=status_line { lines.push(status.into()); }
    lines.push("Active loops:".into()); lines.extend(entries); Ok(lines.join("\n"))
}
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct ArgumentCompletion { pub value:String,pub label:String }
pub fn complete_loop_arguments(prefix:&str)->Option<Vec<ArgumentCompletion>> {
    let prefix=prefix.trim_matches(|c|matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')).to_lowercase();
    let matches:Vec<_>=["stop","status","pause","resume"].into_iter().filter(|verb|verb.starts_with(&prefix)).map(|verb|ArgumentCompletion { value:verb.into(),label:verb.into() }).collect();
    if matches.is_empty() { None } else { Some(matches) }
}
#[derive(Clone,Debug,PartialEq,Eq)]
pub enum TargetResolution { Apply(LoopTarget),None,Ambiguous(Vec<String>) }
pub fn active_loop_ids(state:&LoopState)->Vec<String> {
    state.entries.values().filter_map(|entry| {
        let (fields,lifecycle)=match entry { CronEntry::Fixed { fields,lifecycle,.. }|CronEntry::Dynamic { fields,lifecycle,.. }=>(fields,lifecycle) };
        (lifecycle.phase!=LoopPhase::Ended).then(||fields.id.clone())
    }).collect()
}
pub fn resolve_command_target(target:&LoopTarget,state:&LoopState)->TargetResolution {
    match target {
        LoopTarget::All|LoopTarget::Id(_)=>TargetResolution::Apply(target.clone()),
        LoopTarget::Implicit=>match active_loop_ids(state).as_slice() {
            []=>TargetResolution::None,
            [id]=>TargetResolution::Apply(LoopTarget::Id(id.clone())),
            ids=>TargetResolution::Ambiguous(ids.to_vec()),
        },
    }
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn completion_trims_js_whitespace_and_filters_prefix() { let result=complete_loop_arguments("\u{feff} ST "); assert_eq!(result.unwrap().into_iter().map(|item|item.value).collect::<Vec<_>>(),["stop","status"]); }
    #[test] fn unmatched_completion_is_absent() { assert_eq!(complete_loop_arguments("other"),None); }
    #[test] fn invalid_expiry_is_rejected() { assert!(format_expiry(Some(f64::NAN)).is_err()); }
    #[test] fn expiry_uses_millisecond_precision() { assert_eq!(format_expiry(Some(1234.9)).unwrap(),"1970-01-01T00:00:01.234Z"); }
    #[test] fn expiry_covers_javascript_timeclip_and_extended_negative_years() {
        for (at,expected) in [(-1.0,"1969-12-31T23:59:59.999Z"),(253402300800000.0,"+010000-01-01T00:00:00.000Z"),(8640000000000000.0,"+275760-09-13T00:00:00.000Z"),(-8640000000000000.0,"-271821-04-20T00:00:00.000Z")] { assert_eq!(format_expiry(Some(at)).unwrap(),expected); }
        assert!(format_expiry(Some(8640000000000001.0)).is_err());
    }
    #[test] fn implicit_target_on_empty_state_applies_nothing() { let state=crate::store::empty_loop_state("s"); assert_eq!(resolve_command_target(&LoopTarget::Implicit,&state),TargetResolution::None); }
    #[test] fn explicit_target_is_not_changed_by_missing_entry() { let state=crate::store::empty_loop_state("s"); assert_eq!(resolve_command_target(&LoopTarget::Id("missing".into()),&state),TargetResolution::Apply(LoopTarget::Id("missing".into()))); }
}
