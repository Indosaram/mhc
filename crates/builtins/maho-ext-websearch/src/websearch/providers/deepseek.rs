use super::{anthropic::build_anthropic_messages_search_request,shared::{BuildContext,BuiltSearchRequest}};
use crate::websearch::provider_endpoints::SearchProvider;
pub fn build_request(ctx:&BuildContext<'_>,model:Option<&str>)->BuiltSearchRequest { build_anthropic_messages_search_request(ctx,SearchProvider::Deepseek,model,"deepseek-v4-flash") }
pub use super::anthropic::normalize_anthropic_messages_search_payload as normalize_response;
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn official_endpoint_and_default_model() { let req=build_request(&BuildContext{query:"q",max_results:1.,api_key:None,base_url:None,allowed_domains:None,blocked_domains:None},None); assert_eq!(req.url,"https://api.deepseek.com/anthropic/v1/messages"); assert_eq!(req.body["model"],"deepseek-v4-flash"); }
}
