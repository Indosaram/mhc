use super::{native::{build_native_entries,provider_name,NativeModelInfo,NativeModelRegistry},search::{create_search_routing_state,format_search_text,perform_search,provider_entry_label,SearchRoutingState},types::*};
#[derive(Default)]
pub struct WebSearchTool{routing_state:Option<SearchRoutingState>,routing_key:String}
pub struct SearchParams{pub query:String,pub allowed_domains:Option<Vec<String>>,pub blocked_domains:Option<Vec<String>>}
pub struct WebSearchToolContext<'a>{pub model:Option<&'a NativeModelInfo>,pub model_registry:Option<&'a dyn NativeModelRegistry>}
pub struct SearchToolResult{pub text:String,pub details:SearchRenderDetails}
pub type SearchToolUpdate<'a>=dyn FnMut(SearchToolResult)+Send+'a;
fn result_error(query:&str,error:&str,reason:Option<ConfigLoadFailureReason>)->SearchToolResult{SearchToolResult{text:error.into(),details:SearchRenderDetails::Error(SearchErrorDetails{phase:ErrorPhase::Error,query:query.into(),error:error.into(),reason})}}
fn progress_text(details:&SearchProgressDetails)->String{let route=details.current_provider.clone().unwrap_or_else(||if details.provider_labels.is_empty(){"configured providers".into()}else{details.provider_labels.join(" -> ")});format!("Searching \"{}\" via {route} (max {})",details.query,details.max_results)}
pub fn definition() -> maho_ext_api::ToolDefinition {
    use serde_json::json;
    let mut tool = maho_ext_api::ToolDefinition::new("web_search",
        "Search the web for current information and return source URLs for citation.",
        json!({"type":"object","required":["query"],"additionalProperties":false,"properties":{
            "query":{"type":"string","minLength":2,"description":"The search query to use"},
            "allowed_domains":{"type":"array","items":{"type":"string"},"description":"Only include search results from these domains"},
            "blocked_domains":{"type":"array","items":{"type":"string"},"description":"Never include search results from these domains"}}}),
        std::sync::Arc::new(|_| Box::pin(async { Err(maho_ext_api::ToolError::Message("Registered extension context is required".into())) })));
    tool.label = "Web Search".into();
    tool.prompt_snippet = Some("Search the web for current information, documentation, news, or external facts.".into());
    tool.prompt_guidelines = Some(vec!["After using web_search, cite relevant returned URLs in the final answer.".into()]);
    tool
}

#[derive(serde::Deserialize)]
struct RegisteredParams {
    query: String,
    allowed_domains: Option<Vec<String>>,
    blocked_domains: Option<Vec<String>>,
}

pub fn register_search_tool(api: &mut maho_ext_api::ExtensionApi,
    config: std::sync::Arc<std::sync::Mutex<ConfigLoadResult>>,
    registry: Option<std::sync::Arc<dyn NativeModelRegistry>>) -> Result<(), maho_ext_api::ExtensionFailure> {
    let tool = std::sync::Arc::new(tokio::sync::Mutex::new(WebSearchTool::default()));
    let client = reqwest::Client::new();
    api.register_tool_with_extension_context(definition(), std::sync::Arc::new(move |_, params, signal, update, context| {
        let tool = std::sync::Arc::clone(&tool);
        let loaded = config.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let client = client.clone();
        let registry = registry.clone().unwrap_or_else(||std::sync::Arc::new(super::native::ContextModelRegistry(std::sync::Arc::clone(&context.model_registry))));
        Box::pin(async move {
            let params: RegisteredParams = serde_json::from_value(params).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
            let model = context.model.as_ref().map(|model| NativeModelInfo { provider: model.provider.clone(), id: model.id.clone(), base_url: model.base_url.clone() });
            let mut update = |result: SearchToolResult| {
                if let Some(update) = &update {
                    let mut progress = maho_ext_api::AgentToolResult::text(result.text);
                    progress.details = serde_json::to_value(result.details).unwrap_or_else(|error| std::panic::panic_any(error));
                    update(progress);
                }
            };
            let cancelled = async { match &signal { Some(signal) => signal.cancelled().await, None => std::future::pending().await } };
            let result = tokio::select! { biased;
                result = async {
                    tool.lock().await.execute(&client, loaded,
                        SearchParams { query: params.query, allowed_domains: params.allowed_domains, blocked_domains: params.blocked_domains },
                        Some(WebSearchToolContext { model: model.as_ref(), model_registry: Some(registry.as_ref()) }), None, Some(&mut update)).await
                } => result.map_err(maho_ext_api::ExtensionFailure::new)?,
                () = cancelled => return Err(maho_ext_api::ExtensionFailure::new(signal.as_ref().and_then(maho_ai::utils::abort::AbortSignal::reason).map_or_else(|| "The operation was aborted.".into(), |reason| reason.message))),
            };
            let mut output = maho_ext_api::AgentToolResult::text(result.text);
            output.details = serde_json::to_value(result.details).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
            Ok(output)
        })
    }))
}
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
