use crate::{format::GoalToolRenderDetails,types::{GoalStatus,GoalToolSnapshot}};
use serde_json::Value;
pub fn native_goal_tool_renderers(tool_name:&'static str)->maho_ext_api::ToolRenderers<(),Value> {
    use std::sync::Arc;
    use maho_tui::components::text::Text;
    maho_ext_api::ToolRenderers {
        render_call:Some(Arc::new(move |args,theme,_| {
            let text=render_goal_tool_call(tool_name,args,|role,text|format!("{}{text}\x1b[39m",theme.colors.get(role).map_or("\x1b[39m",String::as_str)),|text|format!("\x1b[1m{text}\x1b[22m"));
            Box::new(Text::with_padding(text,0,0))
        })),
        render_result:Some(Arc::new(|result,options,theme,_| {
            let content=result.content.iter().filter_map(|content|match content { maho_ai::types::ContentBlock::Text(text)=>Some(maho_ext_api::ToolContent::text(&text.text)),_=>None }).collect();
            let result=maho_ext_api::ToolResult { content,details:Some(result.details.clone()) };
            let text=match render_goal_tool_result(&result,options.expanded,|role,text|format!("{}{text}\x1b[39m",theme.colors.get(role).map_or("\x1b[39m",String::as_str)),|text|format!("\x1b[1m{text}\x1b[22m")) { Ok(text)=>text,Err(error)=>std::panic::panic_any(error) };
            Box::new(Text::with_padding(text,0,0))
        })),
    }
}
pub fn status_glyph(status:GoalStatus)->&'static str { match status { GoalStatus::Active=>"●",GoalStatus::Paused=>"◌",GoalStatus::Blocked=>"■",GoalStatus::Complete=>"✓" } }
pub fn status_color(status:GoalStatus)->&'static str { match status { GoalStatus::Active=>"accent",GoalStatus::Paused=>"muted",GoalStatus::Blocked=>"error",GoalStatus::Complete=>"success" } }
pub fn iso_timestamp(epoch_seconds:f64)->Result<String,maho_ext_api::ExtensionFailure> {
    let at=epoch_seconds*1000.0;
    if !at.is_finite()||at.abs()>8_640_000_000_000_000.0 { return Err(maho_ext_api::ExtensionFailure::new("Invalid time value")); }
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
pub fn shorten(value:&str,max:usize)->String {
    let units=value.encode_utf16().collect::<Vec<_>>();
    if units.len()<=max { return value.into(); }
    format!("{}…",String::from_utf16_lossy(&units[..max.saturating_sub(1)]))
}
pub fn objective_call_preview(objective:&str)->String {
    let lines=objective.split('\n').map(|line|line.trim_matches(crate::validation::js_whitespace)).filter(|line|!line.is_empty()).collect::<Vec<_>>();
    let first=lines.first().copied().unwrap_or("");
    if first.is_empty() { return String::new(); }
    let truncated=shorten(first,80);
    if lines.len()>1 && truncated==first { format!("{first}…") } else { truncated }
}
pub fn parse_render_details(value:&Value)->Option<GoalToolRenderDetails> {
    let object=value.as_object()?;
    let goal=object.get("goal")?;
    let notice=object.get("notice").and_then(Value::as_str).map(str::to_owned);
    if goal.is_null() { return Some(GoalToolRenderDetails { goal:None,notice }); }
    let status:GoalStatus=serde_json::from_value(goal.get("status")?.clone()).ok()?;
    Some(GoalToolRenderDetails { goal:Some(GoalToolSnapshot {
        thread_id:goal.get("threadId").and_then(Value::as_str).unwrap_or("").into(),
        objective:goal.get("objective")?.as_str()?.into(),status,
        tokens_used:goal.get("tokensUsed")?.as_f64()?,time_used_seconds:goal.get("timeUsedSeconds")?.as_f64()?,
        created_at:goal.get("createdAt")?.as_f64()?,updated_at:goal.get("updatedAt")?.as_f64()?,
        blocked_reason:goal.get("blockedReason").and_then(Value::as_str).map(str::to_owned),
        blocked_at:goal.get("blockedAt").and_then(Value::as_f64),
    }),notice })
}
pub fn resolve_render_details(details:Option<&Value>,text:&str)->Option<GoalToolRenderDetails> {
    if let Some(parsed)=details.and_then(parse_render_details) { return Some(parsed); }
    let text=text.trim_matches(crate::validation::js_whitespace);
    if !text.starts_with('{') { return None; }
    parse_render_details(&serde_json::from_str::<Value>(text).ok()?)
}
pub fn goal_widget_lines(goal:&GoalToolSnapshot,expanded:bool,fg:impl Fn(&str,&str)->String,bold:impl Fn(&str)->String)->Result<Vec<String>,maho_ext_api::ExtensionFailure> {
    let status=fg(status_color(goal.status),&bold(&format!("{} {}",status_glyph(goal.status),goal.status)));
    let usage=format!(" • {} tokens • {}",crate::format::format_tokens_compact(goal.tokens_used),crate::format::format_goal_elapsed_seconds(goal.time_used_seconds));
    let mut lines=vec![format!("{status}{}",fg("muted",&usage))];
    let objectives=goal.objective.split('\n').map(|line|line.trim_matches(crate::validation::js_whitespace)).filter(|line|!line.is_empty()).collect::<Vec<_>>();
    for line in objectives.iter().take(if expanded { usize::MAX } else { 2 }) { lines.push(fg("toolOutput",&format!("  {}",if expanded { (*line).into() } else { shorten(line,120) }))); }
    if !expanded&&objectives.len()>2 { lines.push(fg("dim",&format!("  … +{} more lines",objectives.len()-2))); }
    if let Some(reason)=goal.blocked_reason.as_ref().filter(|reason|!reason.is_empty()) { lines.push(fg("warning",&format!("  ⚠ {reason}"))); }
    if expanded { lines.push(fg("dim",&format!("  created {} • updated {}",iso_timestamp(goal.created_at)?,iso_timestamp(goal.updated_at)?))); }
    Ok(lines)
}
pub fn render_goal_tool_call(tool_name:&str,args:&Value,fg:impl Fn(&str,&str)->String,bold:impl Fn(&str)->String)->String {
    let title=fg("toolTitle",&bold(tool_name));
    if tool_name=="create_goal"&&let Some(objective)=args.get("objective").and_then(Value::as_str) {
        let preview=objective_call_preview(objective); return if preview.is_empty() { title } else { format!("{title} {}",fg("muted",&preview)) };
    }
    if tool_name=="update_goal"&&let Some(status)=args.get("status").and_then(Value::as_str) {
        let mut line=format!("{title} {} {}",fg("muted","→"),fg(if status=="complete" { "success" } else { "error" },status));
        if let Some(reason)=args.get("reason").and_then(Value::as_str).map(|reason|reason.trim_matches(crate::validation::js_whitespace)).filter(|reason|!reason.is_empty()) { line.push_str(&format!(" {}",fg("muted",&format!("— {}",shorten(reason,60))))); }
        return line;
    }
    title
}
pub fn render_goal_tool_result(result:&maho_ext_api::ToolResult,expanded:bool,fg:impl Fn(&str,&str)->String,bold:impl Fn(&str)->String)->Result<String,maho_ext_api::ExtensionFailure> {
    let text=result.content.iter().find_map(|block|if let maho_ext_api::ToolContent::Text { text,.. }=block { Some(text.as_str()) } else { None }).unwrap_or("");
    let Some(details)=resolve_render_details(result.details.as_ref(),text) else { return Ok(fg("toolOutput",text)); };
    let mut lines=if let Some(goal)=&details.goal { goal_widget_lines(goal,expanded,&fg,bold)? } else { vec![fg("dim","No active goal is set.")] };
    if let Some(notice)=details.notice.filter(|notice|!notice.is_empty()) { lines.push(fg("dim",&format!("  {notice}"))); }
    Ok(lines.join("\n"))
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn native_renderers_use_unpadded_text_and_preserve_theme_ansi() {
        let renderers=native_goal_tool_renderers("create_goal");
        let mut context=maho_ext_api::ToolRenderContext { args:serde_json::json!({"objective":"work"}),tool_call_id:"create".into(),invalidate:std::rc::Rc::new(||{}),last_component:None,state:(),cwd:"/tmp".into(),execution_started:true,args_complete:true,is_partial:false,expanded:false,show_images:false,image_protocol:None,is_error:false,has_result:None,spinner_frame:None };
        let mut theme=maho_ext_api::Theme::default(); theme.colors.insert("toolTitle".into(),"\x1b[31m".into());
        let args=context.args.clone();
        let mut call=renderers.render_call.as_ref().unwrap()(&args,&theme,&mut context);
        let lines=call.render(80); assert_eq!(lines.len(),1); assert!(lines[0].starts_with("\x1b[31m\x1b[1m"));
        let mut result=maho_ext_api::AgentToolResult::text("fallback"); result.details=serde_json::json!({"goal":null});
        let mut output=renderers.render_result.as_ref().unwrap()(&result,Default::default(),&theme,&mut context);
        assert_eq!(output.render(80).len(),1);
    }
    #[test] fn renderer_entrypoints_preserve_theme_roles_and_fallback_selection() {
        let roles=std::cell::RefCell::new(Vec::new());
        let fg=|style:&str,text:&str| { roles.borrow_mut().push(style.to_owned()); text.to_owned() };
        render_goal_tool_call("create_goal",&serde_json::json!({"objective":"work"}),fg,str::to_owned); assert_eq!(*roles.borrow(),["toolTitle","muted"]);
        roles.borrow_mut().clear(); render_goal_tool_call("update_goal",&serde_json::json!({"status":"complete","reason":""}),fg,str::to_owned); assert_eq!(*roles.borrow(),["toolTitle","muted","success"]);
        roles.borrow_mut().clear(); render_goal_tool_call("update_goal",&serde_json::json!({"status":"blocked","reason":"waiting"}),fg,str::to_owned); assert_eq!(*roles.borrow(),["toolTitle","muted","error","muted"]);
        roles.borrow_mut().clear(); let mut result=maho_ext_api::ToolResult::text("fallback"); assert_eq!(render_goal_tool_result(&result,false,fg,str::to_owned).unwrap(),"fallback"); assert_eq!(*roles.borrow(),["toolOutput"]);
        roles.borrow_mut().clear(); result.details=Some(serde_json::json!({"goal":null,"notice":"hint"})); render_goal_tool_result(&result,false,fg,str::to_owned).unwrap(); assert_eq!(*roles.borrow(),["dim","dim"]);
    }
    #[test] fn widget_composition_limits_collapsed_objectives_and_formats_expanded_dates() {
        let goal=GoalToolSnapshot { thread_id:"s".into(),objective:"one\n two\n three".into(),status:GoalStatus::Blocked,tokens_used:12.0,time_used_seconds:30.0,created_at:1.2349,updated_at:-0.001,blocked_reason:Some("waiting".into()),blocked_at:Some(1.0) };
        let styles=std::cell::RefCell::new(Vec::new());
        let collapsed=goal_widget_lines(&goal,false,|style,text| { styles.borrow_mut().push(style.to_owned()); text.into() },str::to_owned).unwrap();
        assert_eq!(collapsed.len(),5); assert_eq!(*styles.borrow(),["error","muted","toolOutput","toolOutput","dim","warning"]);
        styles.borrow_mut().clear(); let expanded=goal_widget_lines(&goal,true,|style,text| { styles.borrow_mut().push(style.to_owned()); text.into() },str::to_owned).unwrap();
        assert_eq!(expanded.len(),6); assert_eq!(*styles.borrow(),["error","muted","toolOutput","toolOutput","toolOutput","warning","dim"]);
        assert!(expanded.last().unwrap().contains(&iso_timestamp(goal.created_at).unwrap())); assert!(expanded.last().unwrap().contains(&iso_timestamp(goal.updated_at).unwrap()));
    }
    #[test] fn iso_timestamp_preserves_js_timeclip_and_millisecond_truncation() {
        for (seconds,expected) in [(1.2349,"1970-01-01T00:00:01.234Z"),(-0.001,"1969-12-31T23:59:59.999Z"),(253402300800.0,"+010000-01-01T00:00:00.000Z"),(8640000000000.0,"+275760-09-13T00:00:00.000Z"),(-8640000000000.0,"-271821-04-20T00:00:00.000Z")] { assert_eq!(iso_timestamp(seconds).unwrap(),expected); }
        for seconds in [f64::NAN,f64::INFINITY,8640000000001.0] { assert!(iso_timestamp(seconds).is_err()); }
    }
    #[test] fn direct_null_goal_details_override_response_text() {
        let details=serde_json::json!({"goal":null,"notice":"hint"});
        let result=resolve_render_details(Some(&details),"invalid").unwrap();
        assert!(result.goal.is_none()); assert_eq!(result.notice.as_deref(),Some("hint"));
    }
    #[test] fn invalid_direct_details_fall_back_to_machine_response() {
        let result=resolve_render_details(Some(&serde_json::json!({"goal":{}})),"{\"goal\":null}").unwrap();
        assert!(result.goal.is_none());
    }
    #[test] fn non_json_response_does_not_make_widget_details() { assert!(resolve_render_details(None,"plain output").is_none()); }
    #[test] fn snapshot_accepts_numeric_fractional_and_negative_fields_like_upstream() {
        let result=parse_render_details(&serde_json::json!({"goal":{"objective":"work","status":"active","tokensUsed":1.5,"timeUsedSeconds":-2.5,"createdAt":-1.0,"updatedAt":2.5}})).unwrap().goal.unwrap();
        assert_eq!(result.tokens_used,1.5); assert_eq!(result.created_at,-1.0); assert_eq!(result.updated_at,2.5);
    }
}
