//! Port of `tool/render.ts`: the themed eval call/result pipeline. Without a theme (print/json
//! modes, or a host theme that exports no colors) every `style` call returns the plain text.
use std::path::PathBuf;
use maho_ext_api::{AgentToolResult, ContentBlock, Theme};
use serde_json::Value;
use super::{display_code::display_code, eval_request::normalize_eval_summary, tool_widgets::{WidgetOptions, code_point_prefix, format_duration, native_theme, render_all_visual_lines as visual, render_tool_call_widget, style}, types::{EvalLanguage, EvalToolRequest}};

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
    pub theme: Option<Theme>,
    pub cwd: PathBuf,
}

const TOOL_CALL_PREVIEW_COUNT: usize = 5;
const TOOL_CALL_COLLAPSED_VISUAL_LINES: usize = 4;
const TOOL_CALL_COLLAPSED_ERROR_CODE_POINTS: usize = 512;

fn preview_text(text: &str, max_lines: usize, width: usize) -> (Vec<String>, usize) {
    let mut lines=visual(text,width);
    if lines.len()<=max_lines {return (lines,0);}
    let skipped=lines.len()-max_lines;
    (lines.split_off(skipped),skipped)
}
fn preview(text: &str, width: usize, limit: usize, kind: &str, theme: Option<&Theme>) -> Vec<String> {
    let (lines,skipped)=preview_text(text,limit,width);
    if skipped==0 {return lines;}
    let mut result=visual(&style(theme,"muted",&format!("{skipped} earlier {kind} lines")),width);
    result.extend(lines);result
}
fn summary(text: &str, width: usize, expanded: bool) -> Vec<String> {
    let lines=visual(text,width);
    if expanded || lines.len()<=3 {return lines;}
    let mut lines:Vec<_>=visual(text,width.saturating_sub(1).max(1)).into_iter().take(3).collect();
    if let Some(last)=lines.last_mut() {last.push('…');}
    lines
}
fn styled_summary(text: &str, width: usize, expanded: bool, theme: Option<&Theme>) -> Vec<String> {
    summary(text,width,expanded).into_iter().map(|line|style(theme,"muted",&line)).collect()
}
fn bold(theme: Option<&Theme>, text: &str) -> String {
    if theme.is_none() {return text.to_owned();}
    format!("\x1b[1m{}\x1b[22m",text.replace("\x1b[22m","\x1b[1m"))
}
fn language(language: EvalLanguage) -> &'static str {match language {EvalLanguage::Js=>"js",EvalLanguage::Py=>"py",EvalLanguage::Jl=>"jl",EvalLanguage::Rb=>"rb"}}
fn language_for_highlighter(language: EvalLanguage) -> &'static str {match language {EvalLanguage::Py=>"python",EvalLanguage::Js=>"javascript",EvalLanguage::Rb=>"ruby",EvalLanguage::Jl=>"julia"}}
fn parse_language(value: &Value) -> EvalLanguage {match value.as_str() {Some("py")=>EvalLanguage::Py,Some("jl")=>EvalLanguage::Jl,Some("rb")=>EvalLanguage::Rb,_=>EvalLanguage::Js}}
fn highlighted_code(code: &str, language: EvalLanguage, theme: Option<&Theme>) -> String {
    let display=if code.trim().is_empty() {"...".to_owned()} else {display_code(code,language)};
    match theme.and_then(native_theme) {
        Some(native)=>maho_interactive::theme::highlight_code(&native,&display,Some(language_for_highlighter(language))).join("\n"),
        None=>display,
    }
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {value[key].as_str().unwrap_or("")}
fn prefixed(body: &str, width: usize, prefix: &str, continuation: &str, color: &str, theme: Option<&Theme>) -> Vec<String> {
    let lines=visual(body,width.saturating_sub(prefix.chars().count()).max(1));
    if lines.is_empty() {return vec![style(theme,color,prefix.trim_end())];}
    lines.into_iter().enumerate().map(|(i,line)|format!("{}{line}",style(theme,color,if i==0 {prefix} else {continuation}))).collect()
}
fn presentation(status: &str, frame: Option<usize>) -> (&str,&str,&'static str) {
    const FRAMES:[&str;10]=["⠋","⠙","⠹","⠸","⠼","⠴","⠦","⠧","⠇","⠏"];
    match status {
        "pending"=>("pending","○","muted"),"queued"=>("queued","○","muted"),
        "running"=>("running",FRAMES[frame.unwrap_or(0)%10],"warning"),"detached"=>("detached","↗","warning"),
        "complete"|"completed"=>("done","✓","success"),"error"=>("error","✗","error"),
        "failed"=>("failed","✗","error"),"aborted"=>("aborted","×","error"),"cancelled"=>("cancelled","×","error"),
        _=>("running",FRAMES[frame.unwrap_or(0)%10],"warning"),
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
fn format_status(event: &Value, theme: Option<&Theme>) -> String {
    let op=text(event,"op");
    let icon=style(theme,"muted",if op.starts_with("git_") {"⌁"} else {match op {"read"|"write"|"cat"|"touch"=>"▣","ls"|"cd"|"pwd"|"mkdir"=>"▤","run"|"sh"=>"▶","completion"=>"◇","phase"=>"◆",_=>"•"}});
    if !text(event,"error").is_empty() {return format!("{icon} {}: {}",style(theme,"warning",op),style(theme,"dim",text(event,"error")));}
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
    format!("{icon} {}{}",style(theme,"muted",op),if description.is_empty() {String::new()} else {format!(" {}",style(theme,"dim",&description))})
}
fn status_lines(events: &[Value], context: &EvalRenderContext) -> Vec<String> {
    let theme=context.theme.as_ref();
    let events:Vec<_>=events.iter().filter(|event|text(event,"op")!="agent").collect();
    let omitted=events.first().filter(|event|text(event,"op")=="status-events-omitted").map_or(0.0,|event|event_number(event,"count"));
    let visible=if omitted>0.0 {&events[1..]} else {&events[..]};
    let skipped=if context.expanded {0} else {visible.len().saturating_sub(3)};
    let mut lines=vec![];
    let count=skipped as f64+omitted;
    if count>0.0 {lines.push(style(theme,"dim",&format!("├ … {count} earlier status events")));}
    for (index,event) in visible[skipped..].iter().enumerate() {lines.push(format!("{} {}",style(theme,"dim",if index+skipped+1==visible.len() {"└"} else {"├"}),format_status(event,theme)));}
    lines
}
fn agent_lines(events: &[Value], context: &EvalRenderContext, width: usize) -> Vec<String> {
    let theme=context.theme.as_ref();
    let mut rows:Vec<&Value>=vec![];
    for event in events.iter().filter(|event|text(event,"op")=="agent") {
        let id=text(event,"id");
        if !id.is_empty() && let Some(index)=rows.iter().position(|row|text(row,"id")==id) {rows[index]=event;} else {rows.push(event);}
    }
    let mut lines=vec![];
    for (index,event) in rows.iter().enumerate() {
        let status=match text(event,"status") {"pending"=>"pending","completed"=>"completed","failed"=>"failed","aborted"=>"aborted",_=>"running"};
        let (label,icon,color)=presentation(status,context.spinner_frame);
        let id=if text(event,"id").is_empty() {"agent"} else {text(event,"id")};
        let mut body=format!("{} {} {label}",style(theme,color,icon),bold(theme,id));
        if matches!(status,"completed"|"failed"|"aborted") && event_number(event,"durationMs")>0.0 {body.push_str(&format!(" · {}",style(theme,"dim",&format_duration(event_number(event,"durationMs")))));}
        let last=index+1==rows.len();let continuation=if last {"  "} else {"│ "};
        lines.extend(prefixed(&body,width,if last {"└ "} else {"├ "},continuation,"dim",theme));
        if status=="running" {
            let tool=text(event,"currentTool");let intent=text(event,"lastIntent");
            if !tool.is_empty() || !intent.is_empty() {
                let detail=if tool.is_empty() {intent.into()} else if intent.is_empty() {tool.into()} else {format!("{tool}: {intent}")};
                lines.extend(prefixed(&detail,width,&format!("{continuation}└ "),&format!("{continuation}  "),"dim",theme));
            }
        }
    }
    lines
}

/// One eval card half. The call/result pipelines above are width-aware, so the lines are built at
/// render time from the card state the host hands the renderer.
struct EvalCard {
    args: Value,
    result: Option<AgentToolResult>,
    expanded: bool,
    has_result: bool,
    is_partial: bool,
    is_error: bool,
    show_images: bool,
    image_protocol: Option<String>,
    spinner_frame: Option<usize>,
    theme: Option<Theme>,
    cwd: PathBuf,
    ticker: std::rc::Rc<LiveTicker>,
}

const LIVE_ELAPSED_TICK_MS: u64 = 1_000;
// A live row repaints on every tick, so this many ticks without a render means the row is gone.
const LIVE_TICKER_MAX_IDLE_TICKS: u32 = 60;

/// senpi `PlainTextComponent.syncLiveTicker`: the host animates no eval row, so while a cell is
/// non-terminal the card repaints itself once a second to advance the header's elapsed time.
struct LiveTicker {
    ticker: std::cell::Cell<Option<maho_tui::ui_intervals::UiIntervalId>>,
    live: std::cell::Cell<bool>,
    invalidate: std::rc::Rc<dyn Fn()>,
    idle_ticks: std::cell::Cell<u32>,
}

impl LiveTicker {
    fn new(is_live: bool, invalidate: std::rc::Rc<dyn Fn()>) -> std::rc::Rc<Self> {
        let ticker=std::rc::Rc::new(Self {ticker:std::cell::Cell::new(None),live:std::cell::Cell::new(false),invalidate,idle_ticks:std::cell::Cell::new(0)});
        ticker.sync_live_ticker(is_live);
        ticker
    }
    fn sync_live_ticker(self: &std::rc::Rc<Self>, is_live: bool) {
        self.live.set(is_live);
        if !is_live {self.stop_live_ticker();return;}
        self.arm_ticker();
    }
    fn stop_live_ticker(&self) {
        if let Some(id)=self.ticker.take() {maho_tui::ui_intervals::clear_ui_interval(id);}
    }
    fn arm_ticker(self: &std::rc::Rc<Self>) {
        if self.ticker.get().is_some() {return;}
        let weak=std::rc::Rc::downgrade(self);
        self.ticker.set(Some(maho_tui::ui_intervals::set_ui_interval(std::time::Duration::from_millis(LIVE_ELAPSED_TICK_MS),std::rc::Rc::new(move ||{if let Some(ticker)=weak.upgrade() {ticker.tick();}}))));
    }
    fn tick(&self) {
        self.idle_ticks.set(self.idle_ticks.get()+1);
        if self.idle_ticks.get()>=LIVE_TICKER_MAX_IDLE_TICKS {self.stop_live_ticker();return;}
        (self.invalidate)();
    }
    fn rendered(self: &std::rc::Rc<Self>) {
        self.idle_ticks.set(0);
        if self.live.get() {self.arm_ticker();}
    }
}

impl Drop for LiveTicker {
    fn drop(&mut self) {self.stop_live_ticker();}
}

fn has_live_cell(details: &Value) -> bool {
    details["cells"].as_array().is_some_and(|cells|cells.iter().any(|cell|matches!(cell["status"].as_str(),Some("pending"|"running")) && !cell["startedAt"].is_null()))
}

impl maho_tui::tui::Component for EvalCard {
    fn dispose(&mut self) {self.ticker.stop_live_ticker();}
    fn render(&mut self, width: usize) -> Vec<String> {
        self.ticker.rendered();
        let context=EvalRenderContext {expanded:self.expanded,has_result:self.has_result,is_partial:self.is_partial,is_error:self.is_error,show_images:self.show_images,image_protocol:self.image_protocol.clone(),spinner_frame:self.spinner_frame,
            now:std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0,|elapsed|elapsed.as_secs_f64()*1000.0),theme:self.theme.clone(),cwd:self.cwd.clone()};
        let request=super::eval_request::parse_eval_request(&self.args);
        match (&self.result,request) {
            (Some(result),Ok(request))=>render_eval_result(result,&request,&context,width),
            (Some(result),Err(_))=>render_eval_result(result,&EvalToolRequest::List,&context,width),
            (None,Ok(request))=>render_eval_call(&request,&context,width),
            // Streaming arguments are incomplete until the model finishes the call.
            (None,Err(_))=>if context.has_result {vec![]} else {visual(&style(context.theme.as_ref(),"toolTitle","eval"),width)},
        }
    }
}

fn eval_card(args: &Value, result: Option<&AgentToolResult>, theme: &Theme, context: &maho_ext_api::ToolRenderContext<(), Value>) -> Box<dyn maho_tui::tui::Component> {
    // A host theme that exports no colors (print/json modes) renders like senpi's `theme === undefined`.
    let theme=(!theme.colors.is_empty()).then(||theme.clone());
    let live=result.is_some_and(|result|!result.details.get("action").is_some_and(|action|!action.is_null()) && has_live_cell(&result.details));
    let ticker=LiveTicker::new(live,context.invalidate.clone());
    Box::new(EvalCard {ticker,args:args.clone(),result:result.cloned(),expanded:context.expanded,has_result:context.has_result.unwrap_or(result.is_some()),
        is_partial:context.is_partial,is_error:context.is_error,show_images:context.show_images,
        image_protocol:context.image_protocol.map(|protocol|format!("{protocol:?}")),spinner_frame:context.spinner_frame,theme,cwd:context.cwd.clone()})
}

/// senpi registers the eval tool with its renderCall/renderResult pair; without it the host draws
/// the generic JSON-argument fallback card.
pub fn eval_tool_renderers() -> maho_ext_api::ToolRenderers<(), Value> {
    maho_ext_api::ToolRenderers {
        render_call:Some(std::sync::Arc::new(|args,theme,context|eval_card(args,None,theme,context))),
        render_result:Some(std::sync::Arc::new(|result,options,theme,context| {context.expanded=options.expanded;context.is_partial=options.is_partial;let args=context.args.clone();eval_card(&args,Some(result),theme,context)})),
    }
}
fn render_cell(cell: &Value, context: &EvalRenderContext, args: &EvalToolRequest, details: &Value, width: usize, first: bool, single: bool) -> Vec<String> {
    let theme=context.theme.as_ref();
    let status=text(cell,"status");let (label,icon,color)=presentation(status,context.spinner_frame);
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
    let mut lines=prefixed(&style(theme,color,&header),width,"╭─ ","│  ","borderAccent",theme);
    if let Some(value)=cell["summary"].as_str() {lines.extend(summary(value,width.saturating_sub(2).max(1),context.expanded).into_iter().map(|line|format!("{}{}",style(theme,"muted","│ "),style(theme,"muted",&line))));}
    let inner_width=width.saturating_sub(2).max(1);
    let code=highlighted_code(text(cell,"code"),parse_language(&cell["language"]),theme);
    let (code_lines,code_skipped)=preview_text(&code,if context.expanded {usize::MAX} else {4},inner_width);
    if code_skipped>0 {lines.extend(prefixed(&format!("{code_skipped} earlier code lines"),width,"│ ","│ ","muted",theme));}
    for line in code_lines {lines.extend(prefixed(&line,width,"│ ","│ ","borderMuted",theme));}
    let output=text(cell,"output").trim_end();
    if !output.is_empty() {
        lines.extend(prefixed("output",width,"├─ ","│  ","dim",theme));
        let output_color=if status=="error" {"error"} else {"toolOutput"};
        let styled_output=output.split('\n').map(|line|style(theme,output_color,line)).collect::<Vec<_>>().join("\n");
        let (output_lines,output_skipped)=preview_text(&styled_output,if context.expanded {usize::MAX} else {8},inner_width);
        if output_skipped>0 {lines.extend(prefixed(&format!("{output_skipped} earlier output lines"),width,"│ ","│ ","muted",theme));}
        for line in output_lines {lines.extend(prefixed(&line,width,"│ ","│ ","borderMuted",theme));}
    }
    if let Some(events)=cell["statusEvents"].as_array() {
        let status=status_lines(events,context);
        if !status.is_empty() {
            lines.extend(prefixed("status",width,"├─ ","│  ","dim",theme));
            for line in status {lines.extend(prefixed(&line,width,"│ ","│ ","borderMuted",theme));}
        }
        lines.push(style(theme,"borderMuted","╰─"));lines.extend(agent_lines(events,context,width));
    } else {lines.push(style(theme,"borderMuted","╰─"));}
    lines
}

pub fn render_eval_call(args: &EvalToolRequest, context: &EvalRenderContext, width: usize) -> Vec<String> {
    let theme=context.theme.as_ref();
    if context.has_result {return vec![];}
    let input=match args {
        EvalToolRequest::List=>return visual(&style(theme,"toolTitle","eval list"),width),
        EvalToolRequest::Peek {cell_id}=>return visual(&style(theme,"toolTitle",&format!("eval peek {cell_id}")),width),
        EvalToolRequest::Stop {cell_id}=>return visual(&style(theme,"toolTitle",&format!("eval stop {cell_id}")),width),
        EvalToolRequest::Run(input)=>input,
    };
    let normalized=normalize_eval_summary(&Value::String(input.summary.clone()));
    if theme.is_some() || context.spinner_frame.is_some() {
        let status=if context.spinner_frame.is_some() {"running"} else {"pending"};
        let cell=serde_json::json!({"language":language(input.language),"code":input.code,"summary":normalized,"status":status,"output":""});
        return render_cell(&cell,context,args,&Value::Null,width,true,true);
    }
    let mut title=format!("eval {}",language(input.language));
    if input.reset==Some(true) {title.push_str(" reset");}
    if let Some(timeout)=input.timeout {title.push_str(&format!(" timeout {timeout}s"));}
    let mut lines=visual(&title,width);
    if let Some(normalized)=normalized {lines.extend(summary(&normalized,width,context.expanded));}
    let code=if input.code.trim().is_empty() {"...".into()} else {display_code(&input.code,input.language)};
    lines.extend(preview(&code,width,if context.expanded {usize::MAX} else {4},"code",theme));lines
}

fn render_json_outputs(values: &[Value], context: &EvalRenderContext, width: usize) -> Vec<String> {
    let theme=context.theme.as_ref();
    let mut lines=vec![];
    for (index,value) in values.iter().enumerate() {
        lines.extend(visual(&style(theme,"dim",&format!("display[{}]",index+1)),width));
        let tree=super::json_tree::render_json_tree_lines(value,if context.expanded {6} else {2},if context.expanded {200} else {6},if context.expanded {2000} else {60});
        for line in tree.lines {lines.extend(visual(&line,width));}
        if tree.truncated {lines.extend(visual(&style(theme,"dim","…"),width));}
    }
    lines
}

fn legacy_tool_call_row(call: &Value, context: &EvalRenderContext, width: usize) -> Vec<String> {
    let theme=context.theme.as_ref();
    let ok=call["ok"]==true;let color=if ok {"success"} else {"error"};
    let title=format!("- tool.{}: {}",text(call,"name"),if ok {"ok"} else {"error"});
    let Some(error)=call["error"].as_str() else {return visual(&style(theme,color,&title),width)};
    if context.expanded {return visual(&style(theme,color,&format!("{title} ({error})")),width);}
    let guarded=code_point_prefix(error,TOOL_CALL_COLLAPSED_ERROR_CODE_POINTS);
    let full=visual(&style(theme,color,&format!("{title} ({guarded})")),width);
    if guarded==error && full.len()<=TOOL_CALL_COLLAPSED_VISUAL_LINES {return full;}
    let marker=visual(&style(theme,"muted","[tool error omitted]"),width);
    let mut row:Vec<_>=visual(&style(theme,color,&title),width).into_iter().take(TOOL_CALL_COLLAPSED_VISUAL_LINES.saturating_sub(marker.len()).max(1)).collect();
    row.extend(visual(&style(theme,color,&format!("  ({guarded})")),width).into_iter().take(TOOL_CALL_COLLAPSED_VISUAL_LINES.saturating_sub(row.len()+marker.len())));
    row.extend(marker.into_iter().take(TOOL_CALL_COLLAPSED_VISUAL_LINES.saturating_sub(row.len())));
    row
}

/// senpi `nestedToolCallBlock` over the legacy `toolCallRows`: with a theme and at least one call
/// carrying captured args, calls with args draw the nested widget; the rest keep the legacy row.
fn tool_call_lines(calls: &[Value], context: &EvalRenderContext, width: usize) -> Vec<String> {
    let theme=context.theme.as_ref();
    let skipped=if context.expanded {0} else {calls.len().saturating_sub(TOOL_CALL_PREVIEW_COUNT)};
    let mut lines=if skipped>0 {visual(&style(theme,"muted",&format!("{skipped} earlier tool {}",if skipped==1 {"call"} else {"calls"})),width)} else {vec![]};
    let nested=theme.is_some() && calls.iter().any(|call|call.get("args").is_some());
    for call in &calls[skipped..] {
        if nested && call.get("args").is_some() {lines.extend(render_tool_call_widget(call,&WidgetOptions {cwd:&context.cwd,theme,expanded:context.expanded,width}));}
        else {lines.extend(legacy_tool_call_row(call,context,width));}
    }
    lines
}

pub fn render_eval_result(result: &AgentToolResult, args: &EvalToolRequest, context: &EvalRenderContext, width: usize) -> Vec<String> {
    let theme=context.theme.as_ref();
    let details=&result.details;
    let fallback=context.show_images && context.image_protocol.is_none();
    let output=result.content.iter().filter_map(|part|match part {
        ContentBlock::Text(part) if part.audience!=Some(maho_ai::types::TextAudience::Model)=>Some(part.text.clone()),
        ContentBlock::Image(part) if fallback=>Some(format!("[image: {}]",crate::host_sdk::sanitize_terminal_label(&part.mime_type))),
        _=>None,
    }).collect::<Vec<_>>().join("\n");
    if details.get("action").is_some() {let mut lines=visual(&style(theme,"toolTitle","eval list"),width);lines.extend(visual(&style(theme,"toolOutput",&output),width));return lines;}
    let mut lines=vec![];
    let json_outputs=details["jsonOutputs"].as_array().filter(|v|!v.is_empty());
    if let Some(cells)=details["cells"].as_array().filter(|cells|!cells.is_empty()) {
        for (index,cell) in cells.iter().enumerate() {
            lines.extend(render_cell(cell,context,args,details,width,index==0,cells.len()==1));
            if index+1<cells.len() {lines.push(String::new());}
        }
        if let Some(values)=json_outputs {
            if !lines.is_empty() {lines.push(String::new());}
            lines.extend(render_json_outputs(values,context,width));
        }
        if fallback {for part in &result.content {if let ContentBlock::Image(part)=part {if !lines.is_empty() {lines.push(String::new());}lines.extend(visual(&format!("[image: {}]",crate::host_sdk::sanitize_terminal_label(&part.mime_type)),width));}}}
        if let Some(phase)=details["phase"].as_str() {lines.extend(visual(&style(theme,"muted",&format!("phase {phase}")),width));}
    } else {
        let status=if context.is_error || details["isError"]==true {"error"} else if context.is_partial {"running"} else {"done"};
        let color=match status {"running"=>"warning","done"=>"success",_=>"error"};
        lines.extend(visual(&style(theme,color,&format!("eval {}{} {status}",details["language"].as_str().unwrap_or("?"),runtime_badge(details))),width));
        if let Some(value)=details["summary"].as_str() {lines.extend(styled_summary(value,width,context.expanded,theme));}
        let mut metadata=vec![];
        if let Some(phase)=details["phase"].as_str().filter(|s|!s.is_empty()) {metadata.push(format!("phase {phase}"));}
        if !context.is_partial && let Some(duration)=details["wallDurationMs"].as_f64().or_else(||details["durationMs"].as_f64()) {metadata.push(format!("took {}",format_duration(duration)));}
        if status=="done" && let Some(badge)=throughput(details) {metadata.push(badge);}
        if !metadata.is_empty() {lines.extend(visual(&style(theme,"muted",&metadata.join(" | ")),width));}
        lines.push(String::new());
        if !output.trim_end().is_empty() {lines.extend(preview(&style(theme,"toolOutput",output.trim_end()),width,if context.expanded {usize::MAX} else {8},"output",theme));}
        else if !(context.show_images && context.image_protocol.is_some() && result.content.iter().any(|part|matches!(part,ContentBlock::Image(_)))) {lines.extend(visual(&style(theme,"muted","(no output)"),width));}
        if let Some(events)=details["statusEvents"].as_array() {
            let mut status=status_lines(events,context);status.extend(agent_lines(events,context,width));
            if !status.is_empty() {lines.push(String::new());for line in status {lines.extend(visual(&line,width));}}
        }
        if let Some(values)=json_outputs {lines.push(String::new());lines.extend(render_json_outputs(values,context,width));}
    }
    if let Some(calls)=details["toolCalls"].as_array().filter(|calls|!calls.is_empty()) {
        lines.push(String::new());
        lines.extend(tool_call_lines(calls,context,width));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn theme() -> Theme {
        let colors=["toolTitle","muted","dim","error","success","warning","borderAccent","borderMuted","toolOutput","mdCodeBlock","accent"].iter().enumerate()
            .map(|(index,key)|((*key).to_owned(),format!("\x1b[38;5;{}m",index+1))).collect();
        Theme {colors,..Default::default()}
    }

    #[test]
    fn themed_result_colors_header_and_legacy_rows() {
        let mut result=AgentToolResult::text("hi");
        result.details=json!({"language":"js","toolCalls":[{"name":"x","ok":true}]});
        let theme=theme();
        let context=EvalRenderContext {theme:Some(theme.clone()),..Default::default()};
        let lines=render_eval_result(&result,&EvalToolRequest::List,&context,80);
        assert!(lines[0].contains(&theme.fg("success","eval js done")));
        assert!(lines.last().is_some_and(|line|line.contains(&theme.fg("success","- tool.x: ok"))));
    }

    #[test]
    fn themed_calls_with_args_render_nested_widgets() {
        let mut result=AgentToolResult::text("");
        result.details=json!({"language":"js","toolCalls":[{"name":"custom","ok":true,"args":{"a":1},"durationMs":1500,"resultPreview":"done"}]});
        let theme=theme();
        let context=EvalRenderContext {theme:Some(theme.clone()),..Default::default()};
        let lines=render_eval_result(&result,&EvalToolRequest::List,&context,80);
        let tail=&lines[lines.len()-3..];
        assert_eq!(tail[0],"  tool.custom({\"a\":1})");
        assert!(tail[1].starts_with("  ") && tail[1].contains(&theme.fg("success","✓")) && tail[1].ends_with(" 1s"));
        assert!(tail[2].starts_with("  ") && tail[2].contains(&theme.fg("muted","done")));
    }

    #[test]
    fn unthemed_calls_with_args_keep_legacy_rows() {
        let mut result=AgentToolResult::text("");
        result.details=json!({"language":"js","toolCalls":[{"name":"custom","ok":true,"args":{"a":1}}]});
        let lines=render_eval_result(&result,&EvalToolRequest::List,&EvalRenderContext::default(),80);
        assert_eq!(lines.last().map(String::as_str),Some("- tool.custom: ok"));
    }

    #[test]
    fn themed_agent_ids_are_bold() {
        let mut result=AgentToolResult::text("");
        result.details=json!({"language":"js","statusEvents":[{"op":"agent","id":"scout","status":"pending"}]});
        let context=EvalRenderContext {theme:Some(theme()),..Default::default()};
        let lines=render_eval_result(&result,&EvalToolRequest::List,&context,80);
        assert!(lines.iter().any(|line|line.contains("\x1b[1mscout\x1b[22m")));
    }
}
