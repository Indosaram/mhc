use super::types::*;
use super::native::provider_name;
use super::providers::{build_search_request,normalize_search_response};
use std::time::{Duration,SystemTime,UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
fn now_milliseconds()->f64{SystemTime::now().duration_since(UNIX_EPOCH).map_or_else(|error|-(error.duration().as_millis() as f64),|duration|duration.as_millis() as f64)}
#[derive(Debug,Default)]
pub struct SearchRoutingState{pub round_robin_cursor:usize,pub success_counts:Vec<f64>}
pub fn create_search_routing_state(provider_count:usize)->SearchRoutingState{SearchRoutingState{round_robin_cursor:0,success_counts:vec![0.0;provider_count]}}
pub fn provider_entry_label(provider:&str,id:Option<&str>,entry_id:Option<&str>)->String{
    let Some(id)=entry_id.or(id).filter(|id|!id.is_empty()&&*id!=provider)else{return provider.into();};
    format!("{provider}/{}",if id.starts_with(&format!("native-{provider}-")){"native"}else{id})
}
pub fn select_order(strategy:RoutingStrategy,providers:&[SearchProviderEntry],state:&mut SearchRoutingState)->Vec<usize>{
    match strategy{
        RoutingStrategy::Priority=>{
            let mut indices=(0..providers.len()).collect::<Vec<_>>();
            indices.sort_by(|left,right|{let difference=providers[*left].priority.unwrap_or(*left as f64)-providers[*right].priority.unwrap_or(*right as f64);if difference.is_nan()||difference==0.0{left.cmp(right)}else{difference.partial_cmp(&0.0).unwrap_or(std::cmp::Ordering::Equal)}});indices
        }
        RoutingStrategy::RoundRobin=>{
            let mut indices=Vec::new();
            for (index,provider) in providers.iter().enumerate(){let raw=provider.weight.unwrap_or(1.0);let weight=if raw.is_nan(){f64::NAN}else{raw.trunc().max(1.0)};let mut count=0.0;while count<weight{indices.push(index);count+=1.0;}}
            if indices.is_empty(){indices.extend(0..providers.len());}
            let mut order=Vec::new();
            if !indices.is_empty(){let start=state.round_robin_cursor%indices.len();for offset in 0..indices.len(){let index=indices[(start+offset)%indices.len()];if !order.contains(&index){order.push(index);}}state.round_robin_cursor=(state.round_robin_cursor+1)%indices.len();}
            for index in 0..providers.len(){if !order.contains(&index){order.push(index);}}order
        }
        RoutingStrategy::FillFirst=>{
            let mut selected=0;let mut selected_count=state.success_counts.first().copied().unwrap_or(0.0);
            for index in 1..providers.len(){let count=state.success_counts.get(index).copied().unwrap_or(0.0);if count<selected_count{selected=index;selected_count=count;}}
            std::iter::once(selected).chain((0..providers.len()).filter(|index|*index!=selected)).collect()
        }
    }
}
pub fn format_search_text(details:&SearchDetails)->String{
    if let Some(error)=details.error.as_ref().filter(|error|!error.is_empty()){return error.clone();}
    if details.results.is_empty(){return format!("No web search results found for \"{}\".",details.query);}
    let provider=provider_name(details.provider);
    let route=if let Some(strategy)=details.strategy{let strategy=match strategy{RoutingStrategy::Priority=>"priority",RoutingStrategy::RoundRobin=>"round-robin",RoutingStrategy::FillFirst=>"fill-first"};format!(" via {} ({strategy})",provider_entry_label(provider,None,details.entry_id.as_deref()))}else{format!(" via {provider}")};
    let mut lines=vec![format!("Web search results for \"{}\"{route}:",details.query),String::new()];
    if let Some(attempts)=details.attempts.as_ref().filter(|attempts|!attempts.is_empty()){
        let attempts=attempts.iter().map(|attempt|{let label=provider_entry_label(provider_name(attempt.provider),None,attempt.entry_id.as_deref());let result=if let Some(error)=attempt.error.as_ref().filter(|error|!error.is_empty()){format!("failed: {error}")}else{format!("{} result{}",attempt.results_count,if attempt.results_count==1.0{""}else{"s"})};format!("{label} {result}")}).collect::<Vec<_>>().join(" -> ");
        lines.push(format!("Routing attempts: {attempts}"));lines.push(String::new());
    }
    for (index,item) in details.results.iter().enumerate(){lines.push(format!("{}. {}",index+1,item.title));lines.push(format!("   {}",item.url));if let Some(snippet)=item.snippet.as_ref().filter(|snippet|!snippet.is_empty()){lines.push(format!("   {snippet}"));}}
    lines.push(String::new());lines.push("REMINDER: Include relevant sources from the URLs above in the final answer.".into());lines.join("\n")
}
pub async fn perform_provider_search(client:&reqwest::Client,config:&SearchProviderEntry,request:&SearchRequest,signal:Option<&CancellationToken>)->Result<SearchDetails,String>{
    let started=now_milliseconds();let built=build_search_request(&config.config,request).map_err(|error|error.to_string())?;
    if signal.is_some_and(CancellationToken::is_cancelled){return Err("The operation was aborted.".into());}
    let timeout_ms=config.config.timeout_ms.unwrap_or(60_000.0).trunc().max(1.0);
    let mut details=SearchDetails{provider:config.config.provider,entry_id:config.config.id.clone(),query:request.query.clone(),results:Vec::new(),duration_ms:0.0,truncated:false,strategy:None,attempts:None,answer:None,error:None};
    let operation=async{
        let mut builder=client.request(match built.init.method{HttpMethod::Get=>reqwest::Method::GET,HttpMethod::Post=>reqwest::Method::POST},&built.url);
        for (name,value) in &built.init.headers{builder=builder.header(name,value);}
        if let Some(body)=&built.body{builder=builder.json(body);}
        let response=builder.send().await.map_err(|error|error.to_string())?;let status=response.status();let text=response.text().await.map_err(|error|error.to_string())?;Ok::<_,String>((status,text))
    };
    let cancellation=async{match signal{Some(signal)=>signal.cancelled().await,None=>std::future::pending::<()>().await}};
    let response=tokio::select!{biased;()=cancellation=>return Err("The operation was aborted.".into()),result=tokio::time::timeout(Duration::from_secs_f64(timeout_ms/1000.0),operation)=>result.unwrap_or_else(|_|Err(format!("Search timed out after {timeout_ms}ms")))};
    let (status,text)=match response{Ok(response)=>response,Err(error)=>{details.duration_ms=now_milliseconds()-started;details.error=Some(error);return Ok(details);}};
    let payload=if config.config.provider==SearchProvider::DuckduckgoHtml{serde_json::json!({"html":text})}else{serde_json::from_str(&text).unwrap_or_else(|_|serde_json::json!({}))};
    if !status.is_success(){
        details.duration_ms=now_milliseconds()-started;
        let detail=payload.get("error").and_then(|value|value.as_str().or_else(||value.get("message").and_then(serde_json::Value::as_str))).filter(|value|!value.is_empty()).or_else(||payload.get("message").and_then(serde_json::Value::as_str).filter(|value|!value.is_empty())).unwrap_or_else(||text.trim_matches(|ch|matches!(ch,'\u{0009}'..='\u{000d}'|' '|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')));
        let units=detail.encode_utf16().collect::<Vec<_>>();let detail=if units.len()>500{format!("{}…",String::from_utf16_lossy(&units[..499]))}else{detail.into()};
        details.error=Some(if detail.is_empty(){format!("Search failed with HTTP {}",status.as_u16())}else{format!("Search failed with HTTP {}: {detail}",status.as_u16())});return Ok(details);
    }
    let mut results=normalize_search_response(config.config.provider,&payload);details.truncated=results.len() as f64>request.max_results;
    let end=if request.max_results.is_nan(){0}else if request.max_results<0.0{(results.len() as f64+request.max_results.trunc()).max(0.0) as usize}else{request.max_results.trunc() as usize};results.truncate(end);details.results=results;
    details.duration_ms=now_milliseconds()-started;
    if details.results.is_empty(){details.error=Some(format!("Search provider {} returned no results for \"{}\".",provider_entry_label(provider_name(config.config.provider),config.config.id.as_deref(),None),request.query));}Ok(details)
}
pub type SearchAttemptListener<'a>=dyn FnMut(&str,&[SearchAttempt],&[String])+Send+'a;
pub async fn perform_search(client:&reqwest::Client,config:&WebsearchConfig,request:&SearchRequest,signal:Option<&CancellationToken>,state:&mut SearchRoutingState,mut on_attempt:Option<&mut SearchAttemptListener<'_>>)->Result<SearchDetails,String>{
    let started=now_milliseconds();let order=select_order(config.strategy,&config.providers,state);let mut attempts=Vec::new();
    let labels=order.iter().filter_map(|index|config.providers.get(*index)).map(|entry|provider_entry_label(provider_name(entry.config.provider),entry.config.id.as_deref(),None)).collect::<Vec<_>>();
    let mut collected:Vec<SearchResultItem>=Vec::new();let mut selected=None;
    for index in order{
        let Some(provider)=config.providers.get(index)else{continue;};
        if let Some(listener)=on_attempt.as_mut(){listener(&provider_entry_label(provider_name(provider.config.provider),provider.config.id.as_deref(),None),&attempts,&labels);}
        let mut details=perform_provider_search(client,provider,request,signal).await?;
        attempts.push(SearchAttempt{provider:details.provider,entry_id:details.entry_id.clone().filter(|id|!id.is_empty()),duration_ms:details.duration_ms,results_count:details.results.len() as f64,error:details.error.clone().filter(|error|!error.is_empty())});
        if details.error.as_ref().is_some_and(|error|!error.is_empty()){
            if !config.fallback{details.strategy=Some(config.strategy);details.attempts=Some(attempts);return Ok(details);}selected=Some(details);continue;
        }
        if state.success_counts.len()<=index{state.success_counts.resize(index+1,0.0);}state.success_counts[index]+=1.0;
        if config.strategy!=RoutingStrategy::FillFirst{details.strategy=Some(config.strategy);details.attempts=Some(attempts);return Ok(details);}
        for item in &details.results{
            if collected.len() as f64>=request.max_results{break;}
            if let Some(existing)=collected.iter_mut().find(|existing|existing.url==item.url){*existing=item.clone();}else{collected.push(item.clone());}
        }
        selected=Some(details);if collected.len() as f64>=request.max_results{break;}
    }
    if !collected.is_empty() && let Some(mut details)=selected.take(){details.results=collected;details.duration_ms=now_milliseconds()-started;details.truncated=details.results.len() as f64>=request.max_results;details.strategy=Some(config.strategy);details.attempts=Some(attempts);details.error=None;return Ok(details);}
    let mut failed=selected.unwrap_or_else(||SearchDetails{provider:config.providers.first().map_or(SearchProvider::Exa,|entry|entry.config.provider),entry_id:None,query:request.query.clone(),results:Vec::new(),duration_ms:0.0,truncated:false,strategy:None,attempts:None,answer:None,error:None});
    failed.duration_ms=now_milliseconds()-started;failed.strategy=Some(config.strategy);failed.error=Some(format!("All configured search providers failed: {}",attempts.iter().map(|attempt|format!("{} {}",provider_entry_label(provider_name(attempt.provider),None,attempt.entry_id.as_deref()),attempt.error.as_deref().unwrap_or("failed"))).collect::<Vec<_>>().join("; ")));failed.attempts=Some(attempts);Ok(failed)
}
