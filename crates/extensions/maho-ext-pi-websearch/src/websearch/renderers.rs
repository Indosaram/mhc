use maho_interactive::theme::{Theme,ThemeColor};
use maho_tui::components::text::Text;
use maho_ai::utils::js::number_to_string;
use super::{types::*,tool::SearchParams,native::provider_name,search::provider_entry_label};

pub fn registered_renderers() -> maho_ext_api::ToolRenderers<(), serde_json::Value> {
    use maho_interactive::theme::{ColorMode, theme_json::{ThemeJson, ColorValue}};
    let theme = |theme: &maho_ext_api::Theme| Theme::from_json(ThemeJson {
        name: theme.name.clone().unwrap_or_default(),
        vars: theme.vars.iter().map(|(key, value)| (key.clone(), ColorValue::Text(value.clone()))).collect(),
        colors: theme.colors.iter().chain(&theme.backgrounds).map(|(key, value)| (key.clone(), ColorValue::Text(value.clone()))).collect(),
        export_colors: Default::default(),
    }, ColorMode::Truecolor).unwrap_or_else(|error| std::panic::panic_any(error));
    maho_ext_api::ToolRenderers {
        render_call: Some(std::sync::Arc::new(move |args, colors, _| {
            let params = SearchParams { query: args["query"].as_str().unwrap_or("").into(),
                allowed_domains: args.get("allowed_domains").map(|value| serde_json::from_value(value.clone()).unwrap_or_else(|error| std::panic::panic_any(error))),
                blocked_domains: args.get("blocked_domains").map(|value| serde_json::from_value(value.clone()).unwrap_or_else(|error| std::panic::panic_any(error))) };
            Box::new(render_search_call(&params, &theme(colors)))
        })),
        render_result: Some(std::sync::Arc::new(move |result, options, colors, _| {
            let details = if result.details.is_null() { None } else { Some(serde_json::from_value(result.details.clone()).unwrap_or_else(|error| std::panic::panic_any(error))) };
            let text = result.content.iter().find_map(|block| match block { maho_ext_api::ContentBlock::Text(text) => Some(text.text.as_str()), _ => None });
            Box::new(render_search_result(text, details.as_ref(), &RenderResultOptions { expanded: options.expanded, is_partial: options.is_partial }, &theme(colors)))
        })),
    }
}

fn shorten(value:&str,max:usize)->String{
    let units=value.encode_utf16().collect::<Vec<_>>();
    if units.len()<=max{return value.into();}
    format!("{}\u{2026}",String::from_utf16_lossy(&units[..max-1]))
}
fn attempt_label(attempt:&SearchAttempt)->String{
    format!("{}:{}",provider_entry_label(provider_name(attempt.provider),None,attempt.entry_id.as_deref()),if attempt.error.as_ref().is_some_and(|error|!error.is_empty()){"failed".into()}else{number_to_string(attempt.results_count)})
}
#[derive(Default)]
pub struct RenderResultOptions{pub expanded:bool,pub is_partial:bool}
pub fn render_search_call(args:&SearchParams,theme:&Theme)->Text{
    let head=theme.fg(ThemeColor::ToolTitle,&theme.bold("web_search "));
    let query=theme.fg(ThemeColor::Accent,&format!("\"{}\"",shorten(&args.query,90)));
    let domains=args.allowed_domains.as_ref().or(args.blocked_domains.as_ref());
    let filter=domains.filter(|domains|!domains.is_empty()).map_or_else(String::new,|domains|theme.fg(ThemeColor::Muted,&format!(" domains:{}",domains.len())));
    Text::with_padding(format!("{head}{query}{filter}"),0,0)
}
pub fn render_search_result(content:Option<&str>,details:Option<&SearchRenderDetails>,options:&RenderResultOptions,theme:&Theme)->Text{
    let text=if options.is_partial{
        if let Some(SearchRenderDetails::Progress(progress))=details{
            let current=progress.current_provider.as_deref().filter(|value|!value.is_empty());
            let provider=current.map(str::to_owned).unwrap_or_else(||if progress.provider_labels.is_empty(){"configured providers".into()}else{progress.provider_labels.join(" -> ")});
            let line=theme.fg(ThemeColor::Warning,&format!("Searching \"{}\" via {provider} (max {})",shorten(&progress.query,80),number_to_string(progress.max_results)));
            let labels=progress.route_labels.as_ref().unwrap_or(&progress.provider_labels);
            if current.is_some()&&options.expanded&&!labels.is_empty(){
                let attempts=progress.attempts.as_deref().unwrap_or(&[]);
                let route=labels.iter().enumerate().map(|(index,label)|{
                    let state=attempts.get(index).map_or_else(||if index==attempts.len(){"searching".into()}else{"pending".into()},|attempt|if attempt.error.as_ref().is_some_and(|error|!error.is_empty()){"failed".into()}else{number_to_string(attempt.results_count)});
                    format!("{label}:{state}")
                }).collect::<Vec<_>>().join(" -> ");
                format!("{line}\n{}",theme.fg(ThemeColor::Muted,&format!("route {route}")))
            }else{line}
        }else{theme.fg(ThemeColor::Warning,content.unwrap_or("Searching the web..."))}
    }else{match details{
        None|Some(SearchRenderDetails::Progress(_))=>theme.fg(ThemeColor::Muted,content.unwrap_or("")),
        Some(SearchRenderDetails::Error(error))=>theme.fg(ThemeColor::Error,&error.error),
        Some(SearchRenderDetails::Result(result))=>{
            if let Some(error)=result.error.as_deref().filter(|error|!error.is_empty()){theme.fg(ThemeColor::Error,error)}else{
                let count=result.results.len();
                let provider=provider_entry_label(provider_name(result.provider),None,result.entry_id.as_deref());
                let strategy=result.strategy.map_or_else(String::new,|strategy|format!(" ({})",match strategy{RoutingStrategy::Priority=>"priority",RoutingStrategy::RoundRobin=>"round-robin",RoutingStrategy::FillFirst=>"fill-first"}));
                let duration=if result.duration_ms>=1000.0{format!("{}s",number_to_string((result.duration_ms/1000.0).round()))}else{format!("{}ms",number_to_string(result.duration_ms))};
                let summary=format!("{}{}{}",theme.fg(ThemeColor::Success,&format!("{count} result{}",if count==1{""}else{"s"})),theme.fg(ThemeColor::Muted,&format!(" via {provider}{strategy} in {duration}")),if result.truncated{theme.fg(ThemeColor::Warning," (truncated)")}else{String::new()});
                let mut rows=vec![summary];
                if count>0{
                    if options.expanded&&let Some(attempts)=&result.attempts&&!attempts.is_empty(){rows.push(theme.fg(ThemeColor::Muted,&format!("route {}",attempts.iter().map(attempt_label).collect::<Vec<_>>().join(" -> "))));}
                    let limit=if options.expanded{8}else{3};
                    for item in result.results.iter().take(limit){rows.push(format!("{} {}",theme.fg(ThemeColor::Accent,&shorten(&item.title,80)),theme.fg(ThemeColor::Dim,&shorten(&item.url,100))));if let Some(snippet)=item.snippet.as_deref().filter(|snippet|!snippet.is_empty()){rows.push(theme.fg(ThemeColor::Muted,&format!("  {}",shorten(snippet,140))));}}
                    if count>limit{rows.push(theme.fg(ThemeColor::Dim,&format!("\u{2026} {} more sources",count-limit)));}
                }rows.join("\n")
            }
        }
    }};
    Text::with_padding(text,0,0)
}
