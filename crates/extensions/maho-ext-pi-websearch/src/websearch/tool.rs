use super::{native::{build_native_entries,provider_name,NativeModelInfo,NativeModelRegistry},search::{create_search_routing_state,format_search_text,perform_search,provider_entry_label,SearchRoutingState},types::*};
#[derive(Default)]
pub struct WebSearchTool{routing_state:Option<SearchRoutingState>,routing_key:String}
pub struct SearchParams{pub query:String,pub allowed_domains:Option<Vec<String>>,pub blocked_domains:Option<Vec<String>>}
pub struct WebSearchToolContext<'a>{pub model:Option<&'a NativeModelInfo>,pub model_registry:Option<&'a dyn NativeModelRegistry>}
pub struct SearchToolResult{pub text:String,pub details:SearchRenderDetails}
pub type SearchToolUpdate<'a>=dyn FnMut(SearchToolResult)+'a;
fn result_error(query:&str,error:&str,reason:Option<ConfigLoadFailureReason>)->SearchToolResult{SearchToolResult{text:error.into(),details:SearchRenderDetails::Error(SearchErrorDetails{phase:ErrorPhase::Error,query:query.into(),error:error.into(),reason})}}
fn progress_text(details:&SearchProgressDetails)->String{let route=details.current_provider.clone().unwrap_or_else(||if details.provider_labels.is_empty(){"configured providers".into()}else{details.provider_labels.join(" -> ")});format!("Searching \"{}\" via {route} (max {})",details.query,details.max_results)}
impl WebSearchTool{
    pub async fn execute(&mut self,client:&reqwest::Client,loaded:ConfigLoadResult,params:SearchParams,context:Option<WebSearchToolContext<'_>>,signal:Option<&tokio_util::sync::CancellationToken>,mut on_update:Option<&mut SearchToolUpdate<'_>>)->Result<SearchToolResult,String>{
        if params.allowed_domains.as_ref().is_some_and(|domains|!domains.is_empty())&&params.blocked_domains.as_ref().is_some_and(|domains|!domains.is_empty()){return Ok(result_error(&params.query,"Error: Cannot specify both allowed_domains and blocked_domains in the same request",None));}
        let mut config=match loaded{ConfigLoadResult::Success{config,..}=>config,ConfigLoadResult::Failure{reason,message,..}=>return Ok(result_error(&params.query,&message,Some(reason)))};
        let max_results=config.providers.first().and_then(|entry|entry.config.max_results).unwrap_or(10.0);
        if config.auto{let mut native=build_native_entries(context.as_ref().and_then(|context|context.model),context.as_ref().and_then(|context|context.model_registry)).await?;native.append(&mut config.providers);config.providers=native;}
        let progress=SearchProgressDetails{phase:SearchingPhase::Searching,query:params.query.clone(),provider_labels:config.providers.iter().map(|entry|provider_entry_label(provider_name(entry.config.provider),entry.config.id.as_deref(),None)).collect(),max_results,current_provider:None,attempts:None,route_labels:None,strategy:Some(config.strategy),allowed_domains:params.allowed_domains.clone(),blocked_domains:params.blocked_domains.clone()};
        if let Some(update)=on_update.as_mut(){update(SearchToolResult{text:progress_text(&progress),details:SearchRenderDetails::Progress(progress.clone())});}
        let strategy=match config.strategy{RoutingStrategy::Priority=>"priority",RoutingStrategy::RoundRobin=>"round-robin",RoutingStrategy::FillFirst=>"fill-first"};let key=format!("{strategy}:{}",config.providers.iter().map(|entry|entry.config.id.as_deref().unwrap_or(provider_name(entry.config.provider))).collect::<Vec<_>>().join("|"));
        if self.routing_state.as_ref().is_none_or(|state|state.success_counts.len()!=config.providers.len())||self.routing_key!=key{self.routing_state=Some(create_search_routing_state(config.providers.len()));self.routing_key=key;}
        let request=SearchRequest{query:params.query,max_results,allowed_domains:params.allowed_domains,blocked_domains:params.blocked_domains};
        let mut listener=|label:&str,attempts:&[SearchAttempt],routes:&[String]|{if let Some(update)=on_update.as_mut(){let mut progress=progress.clone();progress.current_provider=Some(label.into());progress.attempts=Some(attempts.to_vec());progress.route_labels=Some(routes.to_vec());update(SearchToolResult{text:progress_text(&progress),details:SearchRenderDetails::Progress(progress)});}};
        let state=self.routing_state.as_mut().ok_or("Search routing state unavailable")?;let details=perform_search(client,&config,&request,signal,state,Some(&mut listener)).await?;Ok(SearchToolResult{text:format_search_text(&details),details:SearchRenderDetails::Result(details)})
    }
}
