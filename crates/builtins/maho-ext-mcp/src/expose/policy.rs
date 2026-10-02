use crate::config_schema::{DirectTools,Exposure,McpServerConfig,McpSettings};
#[derive(Debug,Clone,PartialEq)]
pub struct McpExposurePolicyResult<T> {pub active_entries:Vec<T>,pub filtered_entries:Vec<T>,pub registered_entries:Vec<T>,pub mode:Exposure,pub reason:ExposureReason,pub warnings:Vec<String>}
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum ExposureReason {Explicit,Threshold,DirectTools}
pub trait CatalogIdentity {fn server(&self)->&str;fn tool(&self)->&str;}
pub fn compute_mcp_exposure_policy<T:CatalogIdentity+Clone>(entries:&[T],config:&McpServerConfig,settings:&McpSettings)->McpExposurePolicyResult<T> {
    let matches = |patterns:&Option<Vec<String>>,tool:&str| patterns.as_ref().is_some_and(|p|p.iter().any(|pattern|safe_match(pattern,tool)));
    let mut filtered:Vec<T> = entries.iter().filter(|e| (config.include_tools.as_ref().is_none_or(Vec::is_empty) || matches(&config.include_tools,e.tool())) && !matches(&config.exclude_tools,e.tool())).cloned().collect();
    filtered.sort_by(|a,b|a.server().cmp(b.server()).then_with(||a.tool().cmp(b.tool())));
    if filtered.is_empty() {return McpExposurePolicyResult {active_entries:Vec::new(),registered_entries:Vec::new(),filtered_entries:filtered,mode:Exposure::Direct,reason:ExposureReason::Explicit,warnings:vec![format!("MCP server {} has 0 exposed tools after includeTools/excludeTools filters.",entries.first().map_or("<unknown>",CatalogIdentity::server))]};}
    let (mode,reason) = if config.direct_tools == Some(DirectTools::All(true)) {(Exposure::Direct,ExposureReason::DirectTools)} else {match config.exposure.unwrap_or(Exposure::Auto) {
        Exposure::Direct=>(Exposure::Direct,ExposureReason::Explicit),Exposure::Search=>(Exposure::Search,ExposureReason::Explicit),Exposure::Proxy=>(Exposure::Proxy,ExposureReason::Explicit),
        Exposure::Auto=>{let count = f64::from(u32::try_from(filtered.len()).unwrap_or(u32::MAX));(if count <= settings.search_threshold.unwrap_or(10.0) {Exposure::Direct} else {Exposure::Search},ExposureReason::Threshold)}
    }};
    let active = match mode {Exposure::Direct=>filtered.clone(),Exposure::Search=>filtered.iter().filter(|e| match &config.direct_tools {Some(DirectTools::Patterns(p))=>p.iter().any(|pattern|safe_match(pattern,e.tool())),_=>false}).cloned().collect(),Exposure::Proxy|Exposure::Auto=>Vec::new()};
    let registered = if mode==Exposure::Proxy {Vec::new()} else {filtered.clone()};
    McpExposurePolicyResult {active_entries:active,filtered_entries:filtered,registered_entries:registered,mode,reason,warnings:Vec::new()}
}
pub fn matches_mcp_tool_pattern(pattern:&str,tool:&str)->bool {
    let mut pattern=pattern;let mut negate=false;
    while pattern.starts_with('!') && !pattern.starts_with("!("){negate = !negate;pattern=&pattern[1..];}
    let matched=match globset::GlobBuilder::new(pattern).literal_separator(false).build() {Ok(glob)=>glob.compile_matcher().is_match(tool) || pattern==tool,Err(_)=>pattern==tool};
    matched!=negate
}
fn safe_match(pattern:&str,tool:&str)->bool {matches_mcp_tool_pattern(pattern,tool)}
