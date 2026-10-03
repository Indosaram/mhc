use super::types::SearchAttempt;
pub fn duration_text(duration_ms:f64)->String { if duration_ms>=1000.0 { format!("{}s",(duration_ms/1000.0).round()) } else { format!("{duration_ms}ms") } }
fn attempt_status(attempt:&SearchAttempt)->String { if attempt.error.as_ref().is_some_and(|error|!error.is_empty()) { "failed".into() } else { attempt.results_count.to_string() } }
pub fn attempt_label(attempts:Option<&[SearchAttempt]>)->String { attempts.unwrap_or(&[]).iter().map(|attempt|format!("{}:{}",super::search::provider_entry_label(attempt.provider.as_str(),None,attempt.entry_id.as_deref()),attempt_status(attempt))).collect::<Vec<_>>().join(" -> ") }
pub fn route_state_label(provider_labels:&[String],route_labels:Option<&[String]>,attempts:&[SearchAttempt])->String {
    route_labels.unwrap_or(provider_labels).iter().enumerate().map(|(index,label)|format!("{label}:{}",attempts.get(index).map_or_else(||if index==attempts.len() { "searching".into() } else { "pending".into() },attempt_status))).collect::<Vec<_>>().join(" -> ")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn duration_threshold_rounds_seconds() { assert_eq!(duration_text(999.0),"999ms"); assert_eq!(duration_text(1500.0),"2s"); }
    #[test] fn route_override_preserves_empty_array() { let labels=["one".into(),"two".into()]; assert_eq!(route_state_label(&labels,None,&[]),"one:searching -> two:pending"); assert_eq!(route_state_label(&labels,Some(&[]),&[]),""); }
}
