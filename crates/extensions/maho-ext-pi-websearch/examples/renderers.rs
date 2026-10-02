use maho_ext_pi_websearch::websearch::{renderers::*,tool::SearchParams,types::*};
use maho_interactive::theme::{ColorMode,Theme,theme_json::{ColorValue,ThemeJson}};
use maho_tui::tui::Component;
fn main()->Result<(),Box<dyn std::error::Error>>{
    let width=std::env::args().nth(1).map_or(Ok(80),|value|value.parse::<usize>())?;
    let colors=[("toolTitle",6),("accent",6),("muted",7),("dim",8),("error",1),("success",2),("warning",3)].into_iter().map(|(key,value)|(key.into(),ColorValue::Index(value))).collect();
    let theme=Theme::from_json(ThemeJson{name:"fixture".into(),vars:Default::default(),colors,export_colors:Default::default()},ColorMode::Truecolor)?;
    let mut call=render_search_call(&SearchParams{query:"native renderer".into(),allowed_domains:Some(vec!["example.org".into()]),blocked_domains:None},&theme);
    for line in call.render(width){println!("{line}");}
    let result:SearchRenderDetails=serde_json::from_value(serde_json::json!({"provider":"exa","entryId":"primary","query":"native renderer","results":[{"title":"Native result","url":"https://example.org","snippet":"A deterministic snippet"}],"durationMs":1500,"truncated":true,"strategy":"priority","attempts":[{"provider":"exa","entryId":"primary","durationMs":1500,"resultsCount":1}]}))?;
    let progress:SearchRenderDetails=serde_json::from_value(serde_json::json!({"phase":"searching","query":"native renderer","providerLabels":["exa/primary","tavily"],"routeLabels":["exa/primary","tavily"],"currentProvider":"tavily","maxResults":5,"attempts":[{"provider":"exa","entryId":"primary","durationMs":10,"resultsCount":0,"error":"fixture"}]}))?;
    for (details,is_partial) in [(&progress,true),(&result,false)]{let mut text=render_search_result(None,Some(details),&RenderResultOptions{expanded:true,is_partial},&theme);for line in text.render(width){println!("{line}");}}Ok(())
}
