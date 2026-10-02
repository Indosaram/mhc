use maho_ext_compaction::{idle_retry::*,summarization_retry::*};
#[test] fn retry_budget_is_half_single_attempt() {assert!((summarization_retry_total_budget_ms(120000.0)-60000.0).abs()<f64::EPSILON);}
#[test] fn retry_budget_has_exclusive_deadline() {assert!(allow_summarization_retry(59999.0,None));assert!(!allow_summarization_retry(60000.0,None));}
#[test] fn retry_budget_scales_with_attempt_budget() {assert!(allow_summarization_retry(119999.0,Some(240000.0)));assert!(!allow_summarization_retry(120000.0,Some(240000.0)));}
#[test] fn default_policy_has_three_fast_retries() {let policy=&DEFAULT_SUMMARIZATION_RETRY_POLICY;assert!(policy.enabled);assert_eq!(policy.max_retries,3);assert_eq!(policy.base_delay_ms,1000);}
#[test] fn idle_warmup_retries_are_bounded_and_fenced() {
    let mut d=IdleWarmupRetryDecision {attempt:1,transient:true,is_idle:true,breaker_tripped:false,still_warm_eligible:true};assert!(should_retry_idle_warmup(&d));
    d.attempt=2;assert!(!should_retry_idle_warmup(&d));d.attempt=0;d.transient=false;assert!(!should_retry_idle_warmup(&d));
    d.transient=true;d.is_idle=false;assert!(!should_retry_idle_warmup(&d));d.is_idle=true;d.breaker_tripped=true;assert!(!should_retry_idle_warmup(&d));
    d.breaker_tripped=false;d.still_warm_eligible=false;assert!(!should_retry_idle_warmup(&d));
}
