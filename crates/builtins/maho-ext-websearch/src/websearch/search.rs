use super::types::{RoutingStrategy,SearchProviderEntry,SearchRequest,SearchDetails,SearchAttempt,WebsearchConfig,SearchProvider};
use maho_tools::definition::{AbortSignal,ToolError};
use serde_json::{Value,json};
use std::time::{Duration,Instant};
use super::providers::{build_search_request,normalize_search_response};
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct SearchRoutingState { pub round_robin_cursor:usize,pub success_counts:Vec<usize> }
pub fn create_search_routing_state(provider_count:usize)->SearchRoutingState { SearchRoutingState{round_robin_cursor:0,success_counts:vec![0;provider_count]} }
pub fn provider_entry_label(provider:&str,id:Option<&str>,entry_id:Option<&str>)->String {
    let Some(id)=entry_id.or(id).filter(|id|!id.is_empty() && *id!=provider) else { return provider.into(); };
    if id.ends_with("/native") { id.into() } else { format!("{provider}/{}",if id.starts_with(&format!("native-{provider}-")) { "native" } else { id }) }
}
pub fn select_order(strategy:RoutingStrategy,providers:&[SearchProviderEntry],state:&mut SearchRoutingState)->Vec<usize> {
    match strategy {
        RoutingStrategy::Priority=>{
            let mut indices:Vec<_>=(0..providers.len()).collect();
            indices.sort_by(|left,right| {
                let a=providers[*left].priority.unwrap_or(*left as f64); let b=providers[*right].priority.unwrap_or(*right as f64);
                (a-b).partial_cmp(&0.).unwrap_or(std::cmp::Ordering::Equal).then(left.cmp(right))
            }); indices
        },
        RoutingStrategy::RoundRobin=>{
            let mut weighted=Vec::new(); for (index,provider) in providers.iter().enumerate() { let weight=provider.weight.unwrap_or(1.).trunc().max(1.) as usize; weighted.extend(std::iter::repeat_n(index,weight)); }
            if weighted.is_empty() { weighted.extend(0..providers.len()); }
            if weighted.is_empty() { state.round_robin_cursor=0; return vec![]; }
            let start=state.round_robin_cursor%weighted.len(); let mut order=Vec::new();
            for offset in 0..weighted.len() { let index=weighted[(start+offset)%weighted.len()]; if !order.contains(&index) { order.push(index); } }
            for index in 0..providers.len() { if !order.contains(&index) { order.push(index); } }
            state.round_robin_cursor=(state.round_robin_cursor+1)%weighted.len(); order
        },
        RoutingStrategy::FillFirst=>{
            let mut selected=0; let mut selected_count=state.success_counts.first().copied().unwrap_or(0);
            for index in 1..providers.len() { let count=state.success_counts.get(index).copied().unwrap_or(0); if count<selected_count { selected=index; selected_count=count; } }
            std::iter::once(selected).chain((0..providers.len()).filter(|index|*index!=selected)).collect()
        },
    }
}
fn error_detail(payload:&Value,body:&str)->String {
    let detail=payload.get("error").and_then(Value::as_str).filter(|s|!s.is_empty()).or_else(||payload.get("error").and_then(|error|error.get("message")).and_then(Value::as_str).filter(|s|!s.is_empty())).or_else(||payload.get("message").and_then(Value::as_str).filter(|s|!s.is_empty())).unwrap_or_else(||body.trim());
    if detail.encode_utf16().count()<=500 { return detail.into(); }
    let mut units=0; let mut end=0; for (index,c) in detail.char_indices() { if units+c.len_utf16()>499 { break; } units+=c.len_utf16(); end=index+c.len_utf8(); }
    format!("{}…",&detail[..end])
}
pub async fn perform_provider_search(config:&SearchProviderEntry,request:&SearchRequest,signal:Option<&AbortSignal>)->Result<SearchDetails,ToolError> {
    let started=Instant::now();
    let built=build_search_request(&config.config,request).map_err(|error|ToolError::Message(error.to_string()))?;
    let signal=signal.cloned().unwrap_or_default(); signal.check()?;
    let timeout_ms=config.config.timeout_ms.unwrap_or(60_000.).trunc().max(1.) as u64;
    let future=async {
        let client=reqwest::Client::new();
        let mut outgoing=if built.method=="GET" { client.get(&built.url) } else { client.post(&built.url) };
        for (key,value) in built.headers { outgoing=outgoing.header(key,value); }
        if !built.body.is_null() { outgoing=outgoing.body(built.body.to_string()); }
        let response=outgoing.send().await.map_err(|error|error.to_string())?;
        let status=response.status(); let text=response.text().await.map_err(|error|error.to_string())?;
        Ok::<_,String>((status,text))
    };
    let fetched=tokio::select! { biased; ()=signal.cancelled()=>return Err(ToolError::Aborted), result=tokio::time::timeout(Duration::from_millis(timeout_ms),future)=>match result { Ok(result)=>result,Err(_)=>Err(format!("Search timed out after {timeout_ms}ms")) } };
    let mut details=SearchDetails{provider:config.config.provider,entry_id:config.config.id.clone(),query:request.query.clone(),results:vec![],duration_ms:started.elapsed().as_secs_f64()*1000.,truncated:false,strategy:None,attempts:None,answer:None,error:None};
    match fetched {
        Err(error)=>details.error=Some(error),
        Ok((status,text))=>{
            let payload=if config.config.provider==SearchProvider::DuckduckgoHtml { json!({"html":text}) } else { serde_json::from_str(&text).unwrap_or_else(|_|json!({})) };
            if !status.is_success() { let detail=error_detail(&payload,&text); details.error=Some(if detail.is_empty() { format!("Search failed with HTTP {}",status.as_u16()) } else { format!("Search failed with HTTP {}: {detail}",status.as_u16()) }); }
            else {
                let results=normalize_search_response(config.config.provider,&payload); details.truncated=results.len() as f64>request.max_results;
                let count=if request.max_results<0. { results.len().saturating_sub((-request.max_results).trunc() as usize) } else { request.max_results.trunc() as usize };
                details.results=results.into_iter().take(count).collect();
                if details.results.is_empty() { details.error=Some(format!("Search provider {} returned no results for \"{}\".",provider_entry_label(config.config.provider.as_str(),config.config.id.as_deref(),None),request.query)); }
            }
        },
    }
    Ok(details)
}
pub type SearchAttemptListener<'a>=dyn Fn(&str,&[SearchAttempt],&[String])+Send+Sync+'a;
pub async fn perform_search(config:&WebsearchConfig,request:&SearchRequest,signal:Option<&AbortSignal>,routing_state:Option<&mut SearchRoutingState>,on_attempt:Option<&SearchAttemptListener<'_>>)->Result<SearchDetails,ToolError> {
    let started=Instant::now(); let mut local=create_search_routing_state(config.providers.len()); let state=routing_state.unwrap_or(&mut local);
    let order=select_order(config.strategy,&config.providers,state); let mut attempts=Vec::new();
    let labels:Vec<_>=order.iter().filter_map(|index|config.providers.get(*index)).map(|entry|provider_entry_label(entry.config.provider.as_str(),entry.config.id.as_deref(),None)).collect();
    let mut collected:Vec<super::types::SearchResultItem>=Vec::new(); let mut selected=None;
    for index in order {
        let Some(provider)=config.providers.get(index) else { continue; };
        if let Some(listener)=on_attempt { listener(&provider_entry_label(provider.config.provider.as_str(),provider.config.id.as_deref(),None),&attempts,&labels); }
        let mut details=perform_provider_search(provider,request,signal).await?;
        attempts.push(SearchAttempt{provider:details.provider,entry_id:details.entry_id.clone().filter(|id|!id.is_empty()),duration_ms:details.duration_ms,results_count:details.results.len(),error:details.error.clone().filter(|error|!error.is_empty())});
        if details.error.is_some() { if !config.fallback { details.strategy=Some(config.strategy); details.attempts=Some(attempts); return Ok(details); } selected=Some(details); continue; }
        if state.success_counts.len()<=index { state.success_counts.resize(index+1,0); } state.success_counts[index]+=1;
        if config.strategy!=RoutingStrategy::FillFirst { details.strategy=Some(config.strategy); details.attempts=Some(attempts); return Ok(details); }
        for item in &details.results {
            if collected.len() as f64>=request.max_results { break; }
            if let Some(existing)=collected.iter_mut().find(|existing|existing.url==item.url) { *existing=item.clone(); } else { collected.push(item.clone()); }
        }
        selected=Some(details); if collected.len() as f64>=request.max_results { break; }
    }
    let mut details=selected.unwrap_or_else(||SearchDetails{provider:config.providers.first().map_or(SearchProvider::Exa,|entry|entry.config.provider),entry_id:None,query:request.query.clone(),results:vec![],duration_ms:0.,truncated:false,strategy:None,attempts:None,answer:None,error:Some("All configured search providers failed.".into())});
    details.duration_ms=started.elapsed().as_secs_f64()*1000.; details.strategy=Some(config.strategy);
    if !collected.is_empty() { details.truncated=collected.len() as f64>=request.max_results; details.results=collected; details.error=None; details.answer=None; }
    else { details.error=Some(format!("All configured search providers failed: {}",attempts.iter().map(|attempt|format!("{} {}",provider_entry_label(attempt.provider.as_str(),attempt.entry_id.as_deref(),None),attempt.error.as_deref().unwrap_or("failed"))).collect::<Vec<_>>().join("; "))); }
    details.attempts=Some(attempts); Ok(details)
}
pub fn format_search_text(details:&SearchDetails)->String {
    if let Some(error)=&details.error { return error.clone(); }
    if details.results.is_empty() { return format!("No web search results found for \"{}\".",details.query); }
    let mut lines=vec![format!("Web search results for \"{}\" via {}:",details.query,provider_entry_label(details.provider.as_str(),None,details.entry_id.as_deref())),String::new()];
    if let Some(attempts)=details.attempts.as_ref().filter(|attempts|!attempts.is_empty()) { lines.push(format!("Routing attempts: {}",attempts.iter().map(|attempt|format!("{} {}",provider_entry_label(attempt.provider.as_str(),None,attempt.entry_id.as_deref()),if let Some(error)=attempt.error.as_ref().filter(|error|!error.is_empty()) { format!("failed: {error}") } else { format!("{} result{}",attempt.results_count,if attempt.results_count==1 { "" } else { "s" }) })).collect::<Vec<_>>().join(" -> "))); lines.push(String::new()); }
    for (index,item) in details.results.iter().enumerate() { lines.push(format!("{}. {}",index+1,item.title)); lines.push(format!("   {}",item.url)); if let Some(snippet)=item.snippet.as_ref().filter(|snippet|!snippet.is_empty()) { lines.push(format!("   {snippet}")); } }
    lines.push(String::new()); lines.push("REMINDER: Include relevant sources from the URLs above in the final answer.".into()); lines.join("\n")
}
#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::{SearchProviderConfig,SearchProvider};
    fn providers()->Vec<SearchProviderEntry> { (0..3).map(|_|SearchProviderEntry{config:SearchProviderConfig::new(SearchProvider::Exa),priority:None,weight:None}).collect() }
    #[test] fn priority_is_stable_with_default_index() { let mut providers=providers(); providers[2].priority=Some(-1.); assert_eq!(select_order(RoutingStrategy::Priority,&providers,&mut create_search_routing_state(3)),[2,0,1]); }
    #[test] fn weighted_round_robin_rotates_unique_providers() { let mut providers=providers(); providers[0].weight=Some(2.); let mut state=create_search_routing_state(3); assert_eq!(select_order(RoutingStrategy::RoundRobin,&providers,&mut state),[0,1,2]); assert_eq!(select_order(RoutingStrategy::RoundRobin,&providers,&mut state),[0,1,2]); assert_eq!(select_order(RoutingStrategy::RoundRobin,&providers,&mut state),[1,2,0]); }
    #[test] fn fill_first_uses_lowest_success_count() { let mut state=create_search_routing_state(3); state.success_counts=vec![2,1,0]; assert_eq!(select_order(RoutingStrategy::FillFirst,&providers(),&mut state),[2,0,1]); }
    #[test] fn native_labels_preserve_explicit_suffix() { assert_eq!(provider_entry_label("openai",Some("native-openai-1"),None),"openai/native"); assert_eq!(provider_entry_label("openai",Some("x"),Some("custom/native")),"custom/native"); }
    #[tokio::test] async fn aborted_provider_never_connects() { let signal=AbortSignal::default(); signal.abort(); let mut config=providers().remove(0); config.config.base_url=Some("http://127.0.0.1:1".into()); let request=SearchRequest{query:"q".into(),max_results:1.,allowed_domains:None,blocked_domains:None}; assert!(matches!(perform_provider_search(&config,&request,Some(&signal)).await,Err(ToolError::Aborted))); }
    #[tokio::test] async fn real_http_fallback_preserves_attempts_and_success_count() {
        use tokio::io::{AsyncReadExt,AsyncWriteExt};
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(); let address=listener.local_addr().unwrap();
        let server=async {
            for (status,body) in [(500,"{\"error\":{\"message\":\"first failed\"}}"),(200,"{\"results\":[{\"title\":\"Found\",\"url\":\"https://example.test\",\"text\":\"snippet\"}]}")] {
                let (mut socket,_)=listener.accept().await.unwrap(); let mut bytes=Vec::new(); let mut chunk=[0;1024];
                loop { let n=socket.read(&mut chunk).await.unwrap(); assert!(n>0); bytes.extend_from_slice(&chunk[..n]); if bytes.windows(4).any(|window|window==b"\r\n\r\n") { break; } }
                assert!(String::from_utf8_lossy(&bytes).starts_with("POST /search"));
                socket.write_all(format!("HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        };
        let mut entries=providers(); entries.truncate(2); for entry in &mut entries { entry.config.base_url=Some(format!("http://{address}/search")); }
        let config=WebsearchConfig{strategy:RoutingStrategy::Priority,fallback:true,auto:false,providers:entries}; let request=SearchRequest{query:"q".into(),max_results:1.,allowed_domains:None,blocked_domains:None}; let mut state=create_search_routing_state(2);
        let client=async { let result=perform_search(&config,&request,None,Some(&mut state),None).await.unwrap(); assert_eq!(result.results[0].title,"Found"); assert!(result.error.is_none()); let attempts=result.attempts.unwrap(); assert_eq!(attempts.len(),2); assert_eq!(attempts[0].error.as_deref(),Some("Search failed with HTTP 500: first failed")); assert_eq!(state.success_counts,[0,1]); };
        tokio::time::timeout(Duration::from_secs(5),async { tokio::join!(server,client); }).await.unwrap();
    }
}
