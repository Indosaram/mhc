use maho_ai::types::{Model,Usage};
pub const CACHE_KEEPALIVE_ENTRY_TYPE:&str="cache-keepalive";
pub const CACHE_WARM_PING_EVENT:&str="cache_warm_ping";
pub fn is_warm_supported_model(model:&Model)->bool{model.api=="anthropic-messages"&&maho_ai::utils::prompt_cache_ttl::is_anthropic_api_base_url(&model.base_url)}
pub fn projected_ping_cost(model:&Model,usage:Option<&Usage>)->f64{
    let tokens=usage.map_or(0.0,|usage|usage.input as f64+usage.cache_read as f64+usage.cache_write as f64);
    tokens*model.cost.cache_read.max(model.cost.cache_write)/1_000_000.0
}
pub fn actual_ping_cost(model:&Model,usage:&Usage)->f64{(usage.cache_read as f64*model.cost.cache_read+usage.cache_write as f64*model.cost.cache_write+usage.input as f64*model.cost.input)/1_000_000.0}
pub fn next_delay_ms(last_completed_at_ms:f64,safe_wait_seconds:f64,margin_seconds:f64,now_ms:f64)->f64{(last_completed_at_ms+(safe_wait_seconds-margin_seconds.max(0.0)).max(0.0)*1000.0-now_ms).max(0.0)}

