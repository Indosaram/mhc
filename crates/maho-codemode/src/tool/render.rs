//! Port of the unthemed eval render pipeline. Native theme/widget registration
//! remains blocked on the public extension rendering contracts (see evidence).
use maho_ext_api::{AgentToolResult, ContentBlock};
use maho_tui::{components::text::Text, tui::Component};
use serde_json::Value;
use super::{display_code::display_code, eval_request::normalize_eval_summary, tool_widgets::{code_point_prefix, format_duration}, types::{EvalLanguage, EvalToolRequest}};

#[derive(Default)]
pub struct EvalRenderContext {
    pub expanded: bool,
    pub has_result: bool,
    pub is_partial: bool,
    pub is_error: bool,
    pub show_images: bool,
    pub image_protocol: Option<String>,
    pub spinner_frame: Option<usize>,
    pub now: f64,
}

fn visual(text: &str, width: usize) -> Vec<String> {
    Text::with_padding(text, 0, 0).render(width.max(1)).into_iter().map(|line|line.trim_end().to_owned()).collect()
}
fn preview(text: &str, width: usize, limit: usize, kind: &str) -> Vec<String> {
    let mut lines=visual(text,width);
    if lines.len()>limit {
        let skipped=lines.len()-limit;
        lines=lines.split_off(skipped);
        let mut result=visual(&format!("{skipped} earlier {kind} lines"),width);
        result.extend(lines);result
    } else {lines}
}
fn summary(text: &str, width: usize, expanded: bool) -> Vec<String> {
    let lines=visual(text,width);
    if expanded || lines.len()<=3 {return lines;}
    let mut lines:Vec<_>=visual(text,width.saturating_sub(1).max(1)).into_iter().take(3).collect();
    if let Some(last)=lines.last_mut() {last.push('…');}
    lines
}
fn language(language: EvalLanguage) -> &'static str {match language {EvalLanguage::Js=>"js",EvalLanguage::Py=>"py",EvalLanguage::Jl=>"jl",EvalLanguage::Rb=>"rb"}}
fn parse_language(value: &Value) -> EvalLanguage {match value.as_str() {Some("py")=>EvalLanguage::Py,Some("jl")=>EvalLanguage::Jl,Some("rb")=>EvalLanguage::Rb,_=>EvalLanguage::Js}}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {value[key].as_str().unwrap_or("")}
fn prefixed(body: &str, width: usize, prefix: &str, continuation: &str) -> Vec<String> {
    let lines=visual(body,width.saturating_sub(prefix.chars().count()).max(1));
    if lines.is_empty() {return vec![prefix.trim_end().into()];}
    lines.into_iter().enumerate().map(|(i,line)|format!("{}{line}",if i==0 {prefix} else {continuation})).collect()
}
fn presentation(status: &str, frame: Option<usize>) -> (&str,&str) {
    const FRAMES:[&str;10]=["⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏"];
    match status {
        "pending"=>("pending","○"),"queued"=>("queued","○"),
        "running"=>("running",FRAMES[frame.unwrap_or(0)%10]),"detached"=>("detached","↗"),
        "complete"|"completed"=>("done","✓"),"error"=>("error","✗"),
        "failed"=>("failed","✗"),"aborted"=>("aborted","×"),"cancelled"=>("cancelled","×"),
        _=>("running",FRAMES[frame.unwrap_or(0)%10]),
    }
}
fn throughput(details: &Value) -> Option<String> {
    let calls=details["toolCallCount"].as_f64()?;
    if calls<=0.0 {return None;}
    let seconds=details["wallDurationMs"].as_f64().unwrap_or(0.0)/1000.0;
    let rate=if seconds>0.0 {format!("{:.2} calls/s",calls/seconds)} else {"n/a calls/s".into()};
    Some(format!("{calls} {} · {rate}",if calls==1.0 {"call"} else {"calls"}))
}
fn runtime_badge(details: &Value) -> String {
    let runtime=&details["runtime"];
    if !runtime.is_object() {return String::new();}
    let badge=super::runtime_label::format_runtime_badge(parse_language(&details["language"]),text(runtime,"name"),text(runtime,"version"),runtime["path"].as_str(),&std::env::var("HOME").unwrap_or_default());
    format!(" ({badge})")
}
fn event_number(event: &Value, key: &str) -> f64 {event[key].as_f64().filter(|n|n.is_finite()).unwrap_or(0.0)}
fn plural(count: f64, singular: &str, multiple: &str) -> String {format!("{count} {}",if count==1.0 {singular} else {multiple})}
fn format_status(event: &Value) -> String {
    let op=text(event,"op");
    let icon=if op.starts_with("git_") {"⌁"} else {match op {"read"|"write"|"cat"|"touch"=>"▣","ls"|"cd"|"pwd"|"mkdir"=>"▤","run"|"sh"=>"▶","completion"=>"◇","phase"=>"◆",_=>"•"}};
    if !text(event,"error").is_empty() {return format!("{icon} {op}: {}",text(event,"error"));}
    let mut parts=vec![];
    match op {
        "read"|"write"=>{
            parts.push(format!("{} chars",event["chars"].as_f64().unwrap_or_else(||event_number(event,"bytes"))));
            if !text(event,"path").is_empty() {parts.push(format!("{} {}",if op=="read" {"from"} else {"to"},text(event,"path")));}
        }
        "cat"=>{parts.push(plural(event_number(event,"files"),"file","files"));parts.push(format!("{} chars",event_number(event,"chars")));}
        "ls"=>parts.push(plural(event_number(event,"count"),"entry","entries")),
        "env"=>{
            let key=text(event,"key");let value:String=text(event,"value").encode_utf16().take(30).map(|n|char::from_u32(u32::from(n)).unwrap_or('�')).collect();
            if matches!(text(event,"action"),"get"|"set") && !key.is_empty() {parts.push(format!("{}{key}={value}",if text(event,"action")=="set" {"set "} else {""}));}
            else {parts.push(plural(event_number(event,"count"),"variable","variables"));}
        }
        "git_status"=>{
            if event["clean"]==true {parts.push("clean".into());}
            else {
                let changes:Vec<_>=["staged","modified","untracked"].into_iter().filter(|key|event_number(event,key)>0.0).map(|key|format!("{} {key}",event_number(event,key))).collect();
                parts.push(if changes.is_empty() {"unknown".into()} else {changes.join(", ")});
            }
            if !text(event,"branch").is_empty() {parts.push(format!("on {}",text(event,"branch")));}
        }
        "git_diff"=>{parts.push(plural(event_number(event,"lines"),"line","lines"));if event["staged"]==true {parts.push("staged".into());}}
        "git_log"=>parts.push(plural(event_number(event,"commits"),"commit","commits")),
        "run"|"sh"=>{
            let command=event["command"].as_str().unwrap_or_else(||text(event,"cmd"));if !command.is_empty() {parts.push(command.into());}
            if let Some(code)=event["exitCode"].as_f64() {parts.push(format!("exit {code}"));}
        }
        "completion"=>{
            let model=text(event,"model");let tier=text(event,"tier");
            if !model.is_empty() {parts.push(model.into());}
            if !tier.is_empty() && tier!=model {parts.push(tier.into());}
            parts.push(format!("{} chars",event_number(event,"chars")));
        }
        "log"=>parts.push(text(event,"message").into()),"phase"=>parts.push(text(event,"title").into()),
        "status-events-omitted"=>parts.push(format!("{} earlier events omitted",event_number(event,"count"))),
        _=>{
            if let Some(count)=event.get("count") {parts.push(count.as_str().map_or_else(||count.to_string(),str::to_owned));}
            if !text(event,"path").is_empty() {parts.push(text(event,"path").into());}
        }
    }
    let description=parts.into_iter().filter(|p|!p.is_empty()).collect::<Vec<_>>().join(" · ");
    format!("{icon} {op}{}",if description.is_empty() {String::new()} else {format!(" {description}")})
}
fn status_lines(events: &[Value], context: &EvalRenderContext) -> Vec<String> {
    let events:Vec<_>=events.iter().filter(|event|text(event,"op")!="agent").collect();
    let omitted=events.first().filter(|event|text(event,"op")=="status-events-omitted").map_or(0.0,|event|event_number(event,"count"));
    let visible=if omitted>0.0 {&events[1..]} else {&events[..]};
    let skipped=if context.expanded {0} else {visible.len().saturating_sub(3)};
    let mut lines=vec![];
    let count=skipped as f64+omitted;
    if count>0.0 {lines.push(format!("├ … {count} earlier status events"));}
    for (index,event) in visible[skipped..].iter().enumerate() {lines.push(format!("{} {}",if index+skipped+1==visible.len() {"└"} else {"├"},format_status(event)));}
    lines
}
fn agent_lines(events: &[Value], context: &EvalRenderContext, width: usize) -> Vec<String> {
    let mut rows:Vec<&Value>=vec![];
    for event in events.iter().filter(|event|text(event,"op")=="agent") {
        let id=text(event,"id");
        if !id.is_empty() && let Some(index)=rows.iter().position(|row|text(row,"id")==id) {rows[index]=event;} else {rows.push(event);}
    }
    let mut lines=vec![];
    for (index,event) in rows.iter().enumerate() {
        let status=match text(event,"status") {"pending"=>"pending","completed"=>"completed","failed"=>"failed","aborted"=>"aborted",_=>"running"};
        let (label,icon)=presentation(status,context.spinner_frame);
        let id=if text(event,"id").is_empty() {"agent"} else {text(event,"id")};
        let mut body=format!("{icon} {id} {label}");
        if matches!(status,"completed"|"failed"|"aborted") && event_number(event,"durationMs")>0.0 {body.push_str(&format!(" · {}",format_duration(event_number(event,"durationMs"))));}
        let last=index+1==rows.len();let continuation=if last {"  "} else {"│ "};
        lines.extend(prefixed(&body,width,if last {"└ "} else {"├ "},continuation));
        if status=="running" {
            let tool=text(event,"currentTool");let intent=text(event,"lastIntent");
            if !tool.is_empty() || !intent.is_empty() {
                let detail=if tool.is_empty() {intent.into()} else if intent.is_empty() {tool.into()} else {format!("{tool}: {intent}")};
                lines.extend(prefixed(&detail,width,&format!("{continuation}└ "),&format!("{continuation}  ")));
            }
        }
    }
    lines
}
fn render_cell(cell: &Value, context: &EvalRenderContext, args: &EvalToolRequest, details: &Value, width: usize, first: bool, single: bool) -> Vec<String> {
    let status=text(cell,"status");let (label,icon)=presentation(status,context.spinner_frame);
    let mut header=format!("eval {}{} {label} {icon}",text(cell,"language"),runtime_badge(cell));
    if let Some(queued)=cell["queuedBehind"].as_array().filter(|v|!v.is_empty()) {
        header.push_str(&format!(" · queued behind {}",queued.iter().filter_map(Value::as_str).map(crate::host_sdk::sanitize_terminal_label).collect::<Vec<_>>().join(", ")));
    }
    let badge=(!context.is_partial && single && status=="complete").then(||throughput(details)).flatten();
    if let Some(badge)=&badge {header.push_str(&format!(" · {badge}"));}
    let elapsed=if matches!(status,"pending"|"running") {cell["startedAt"].as_f64().map(|start|(context.now-start).max(0.0)).or_else(||cell["durationMs"].as_f64())} else {cell["durationMs"].as_f64()};
    let elapsed=if badge.is_some() {details["wallDurationMs"].as_f64().or(elapsed)} else {elapsed};
    if let Some(elapsed)=elapsed {header.push_str(&format!(" · {}",format_duration(elapsed)));}
    if first && let EvalToolRequest::Run(input)=args {
        if input.reset==Some(true) {header.push_str(" · reset");}
        if let Some(timeout)=input.timeout {header.push_str(&format!(" · timeout {timeout}s"));}
    }
    let mut lines=prefixed(&header,width,"╭─ ","│  ");
    if let Some(value)=cell["summary"].as_str() {lines.extend(summary(value,width.saturating_sub(2).max(1),context.expanded).into_iter().map(|line|format!("│ {line}")));}
    let code=if text(cell,"code").trim().is_empty() {"...".into()} else {display_code(text(cell,"code"),parse_language(&cell["language"]))};
    for line in preview(&code,width.saturating_sub(2).max(1),if context.expanded {usize::MAX} else {4},"code") {lines.extend(prefixed(&line,width,"│ ","│ "));}
    let output=text(cell,"output").trim_end();
    if !output.is_empty() {
        lines.extend(prefixed("output",width,"├─ ","│  "));
        for line in preview(output,width.saturating_sub(2).max(1),if context.expanded {usize::MAX} else {8},"output") {lines.extend(prefixed(&line,width,"│ ","│ "));}
    }
    if let Some(events)=cell["statusEvents"].as_array() {
        let status=status_lines(events,context);
        if !status.is_empty() {
            lines.extend(prefixed("status",width,"├─ ","│  "));
            for line in status {lines.extend(prefixed(&line,width,"│ ","│ "));}
        }
        lines.push("╰─".into());lines.extend(agent_lines(events,context,width));
    } else {lines.push("╰─".into());}
    lines
}

pub fn render_eval_call(args: &EvalToolRequest, context: &EvalRenderContext, width: usize) -> Vec<String> {
    if context.has_result {return vec![];}
    let input=match args {
        EvalToolRequest::List=>return visual("eval list",width),
        EvalToolRequest::Peek {cell_id}=>return visual(&format!("eval peek {cell_id}"),width),
        EvalToolRequest::Stop {cell_id}=>return visual(&format!("eval stop {cell_id}"),width),
        EvalToolRequest::Run(input)=>input,
    };
    let normalized=normalize_eval_summary(&Value::String(input.summary.clone()));
    if context.spinner_frame.is_some() {
        let cell=serde_json::json!({"language":language(input.language),"code":input.code,"summary":normalized,"status":"running","output":""});
        return render_cell(&cell,context,args,&Value::Null,width,true,true);
    }
    let mut title=format!("eval {}",language(input.language));
    if input.reset==Some(true) {title.push_str(" reset");}
    if let Some(timeout)=input.timeout {title.push_str(&format!(" timeout {timeout}s"));}
    let mut lines=visual(&title,width);
    if let Some(normalized)=normalized {lines.extend(summary(&normalized,width,context.expanded));}
    let code=if input.code.trim().is_empty() {"...".into()} else {display_code(&input.code,input.language)};
    lines.extend(preview(&code,width,if context.expanded {usize::MAX} else {4},"code"));lines
}

pub fn render_eval_result(result: &AgentToolResult, args: &EvalToolRequest, context: &EvalRenderContext, width: usize) -> Vec<String> {
    let details=&result.details;
    let fallback=context.show_images && context.image_protocol.is_none();
    let output=result.content.iter().filter_map(|part|match part {
        ContentBlock::Text(part) if part.audience!=Some(maho_ai::types::TextAudience::Model)=>Some(part.text.clone()),
        ContentBlock::Image(part) if fallback=>Some(format!("[image: {}]",crate::host_sdk::sanitize_terminal_label(&part.mime_type))),
        _=>None,
    }).collect::<Vec<_>>().join("\n");
    if details.get("action").is_some() {let mut lines=visual("eval list",width);lines.extend(visual(&output,width));return lines;}
    let mut lines=vec![];
    if let Some(cells)=details["cells"].as_array().filter(|cells|!cells.is_empty()) {
        for (index,cell) in cells.iter().enumerate() {
            if index>0 {lines.push(String::new());}
            lines.extend(render_cell(cell,context,args,details,width,index==0,cells.len()==1));
        }
        if fallback {for part in &result.content {if let ContentBlock::Image(part)=part {lines.push(String::new());lines.extend(visual(&format!("[image: {}]",crate::host_sdk::sanitize_terminal_label(&part.mime_type)),width));}}}
        if let Some(phase)=details["phase"].as_str() {lines.extend(visual(&format!("phase {phase}"),width));}
    } else {
        let status=if context.is_error || details["isError"]==true {"error"} else if context.is_partial {"running"} else {"done"};
        lines.extend(visual(&format!("eval {}{} {status}",details["language"].as_str().unwrap_or("?"),runtime_badge(details)),width));
        if let Some(value)=details["summary"].as_str() {lines.extend(summary(value,width,context.expanded));}
        let mut metadata=vec![];
        if let Some(phase)=details["phase"].as_str().filter(|s|!s.is_empty()) {metadata.push(format!("phase {phase}"));}
        if !context.is_partial && let Some(duration)=details["wallDurationMs"].as_f64().or_else(||details["durationMs"].as_f64()) {metadata.push(format!("took {}",format_duration(duration)));}
        if status=="done" && let Some(badge)=throughput(details) {metadata.push(badge);}
        if !metadata.is_empty() {lines.extend(visual(&metadata.join(" | "),width));}
        lines.push(String::new());
        if !output.trim_end().is_empty() {lines.extend(preview(output.trim_end(),width,if context.expanded {usize::MAX} else {8},"output"));}
        else if !(context.show_images && context.image_protocol.is_some() && result.content.iter().any(|part|matches!(part,ContentBlock::Image(_)))) {lines.extend(visual("(no output)",width));}
    }
    if details["cells"].as_array().is_none_or(Vec::is_empty) && let Some(events)=details["statusEvents"].as_array() {
        let mut status=status_lines(events,context);status.extend(agent_lines(events,context,width));
        if !status.is_empty() {lines.push(String::new());for line in status {lines.extend(visual(&line,width));}}
    }
    if let Some(values)=details["jsonOutputs"].as_array().filter(|v|!v.is_empty()) {
        if !lines.is_empty() {lines.push(String::new());}
        for (index,value) in values.iter().enumerate() {
            lines.extend(visual(&format!("display[{}]",index+1),width));
            let tree=super::json_tree::render_json_tree_lines(value,if context.expanded {6} else {2},if context.expanded {200} else {6},if context.expanded {2000} else {60});
            for line in tree.lines {lines.extend(visual(&line,width));}
            if tree.truncated {lines.extend(visual("…",width));}
        }
    }
    if let Some(calls)=details["toolCalls"].as_array().filter(|calls|!calls.is_empty()) {
        lines.push(String::new());let skipped=if context.expanded {0} else {calls.len().saturating_sub(5)};
        if skipped>0 {lines.extend(visual(&format!("{skipped} earlier tool {}",if skipped==1 {"call"} else {"calls"}),width));}
        for call in &calls[skipped..] {
            let title=format!("- tool.{}: {}",text(call,"name"),if call["ok"]==true {"ok"} else {"error"});
            if let Some(error)=call["error"].as_str() {
                let guarded=code_point_prefix(error,512);
                let full=visual(&format!("{title} ({guarded})"),width);
                if context.expanded {lines.extend(visual(&format!("{title} ({error})"),width));}
                else if guarded==error && full.len()<=4 {lines.extend(full);}
                else {
                    let marker=visual("[tool error omitted]",width);
                    let mut row:Vec<_>=visual(&title,width).into_iter().take(4usize.saturating_sub(marker.len()).max(1)).collect();
                    row.extend(visual(&format!("  ({guarded})"),width).into_iter().take(4usize.saturating_sub(row.len()+marker.len())));
                    row.extend(marker.into_iter().take(4usize.saturating_sub(row.len())));lines.extend(row);
                }
            } else {lines.extend(visual(&title,width));}
        }
    }
    lines
}
