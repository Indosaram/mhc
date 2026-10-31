use maho_ai::types::{Model,ModelThinkingLevel};
use std::collections::{HashMap,HashSet};
use super::{chains::*,cooldown::{SelectorCooldowns,SelectorFailure},expansion::FallbackAuthTiers,settings::{ResolvedRetryFallbackSettings,FallbackRevertPolicy}};

#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum FallbackReason {Transient,Refusal,HardError,Billing}
#[derive(Debug,Clone)]
pub struct ActiveFallbackState {
    pub chain_key:String,pub original_selector:String,pub original_thinking_level:Option<ModelThinkingLevel>,
    pub last_applied_thinking_level:Option<ModelThinkingLevel>,pub pinned_by_refusal:bool,pub pinned_by_billing:bool,pub pinned:bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Deps {models:Vec<Model>,current:(Model,Option<ModelThinkingLevel>),chains:FallbackChains,events:Vec<FallbackEvent>}
    impl RetryFallbackDeps for Deps {
        fn settings(&self)->ResolvedRetryFallbackSettings{ResolvedRetryFallbackSettings{model_fallback:true,chains:self.chains.clone(),revert_policy:FallbackRevertPolicy::CooldownExpiry}}
        fn models(&self)->Vec<Model>{self.models.clone()}
        fn current(&self)->Option<(Model,Option<ModelThinkingLevel>)>{Some(self.current.clone())}
        fn is_auth_available(&self,_:&str)->bool{true}
        fn switch_model<'a>(&'a mut self,model:Model,thinking:ModelThinkingLevel,_:bool)->maho_ai::types::BoxFuture<'a,Result<(),String>>{Box::pin(async move{self.current=(model,Some(thinking));Ok(())})}
        fn emit(&mut self,event:FallbackEvent){self.events.push(event);}
    }
    fn controller()->RetryFallbackController<Deps>{
        let mut a=maho_ai::providers::all::get_builtin_models("openai").remove(0);a.provider="p".into();a.id="a".into();
        let mut b=a.clone();b.id="b".into();
        let deps=Deps{models:vec![a.clone(),b],current:(a,Some(ModelThinkingLevel::Off)),chains:FallbackChains::from([("p/a".into(),vec!["p/b".into()])]),events:Vec::new()};
        RetryFallbackController::new(deps,SelectorCooldowns::new(||0.0,||0.0).expect("cooldowns"))
    }
    #[tokio::test]
    async fn refusal_pin_releases_on_compaction_and_restores_primary(){
        let mut controller=controller();assert!(controller.try_fallback(FallbackReason::Refusal,Default::default()).await.expect("fallback"));
        assert!(!controller.maybe_restore_primary(FallbackRevertPolicy::CooldownExpiry).await.expect("restore"));
        assert!(controller.notify_compaction_applied());assert!(controller.maybe_restore_primary(FallbackRevertPolicy::CooldownExpiry).await.expect("restore"));assert_eq!(controller.deps.current.0.id,"a");
    }
    #[tokio::test]
    async fn billing_pin_never_releases_on_compaction(){
        let mut controller=controller();assert!(controller.try_fallback(FallbackReason::Billing,Default::default()).await.expect("fallback"));
        assert!(!controller.notify_compaction_applied());assert!(controller.state.as_ref().expect("state").pinned_by_billing);
        controller.cooldowns.clear_all();assert!(!controller.maybe_restore_primary(FallbackRevertPolicy::CooldownExpiry).await.expect("restore"));
    }
    #[tokio::test]
    async fn exhausted_candidates_do_not_repeat_within_turn(){
        let mut controller=controller();assert!(controller.can_try_fallback());assert!(controller.try_fallback(FallbackReason::Transient,Default::default()).await.expect("fallback"));assert!(!controller.can_try_fallback());assert_eq!(controller.exhausted_chain_key.as_deref(),Some("p/a"));
    }
    /// senpi test/suite/regressions/2491-request-refused.test.ts: a refused provider's candidates are skipped
    /// for the rest of the turn.
    #[test]
    fn a_refused_provider_is_skipped_until_the_turn_resets(){
        let mut controller=controller();assert!(!controller.can_try_fallback_refused(Some("p")));
        controller.note_refused_provider("p");assert!(!controller.can_try_fallback());
        controller.reset_turn();assert!(controller.can_try_fallback());
    }
    /// senpi test/suite/regressions/3131-spent-account.test.ts "allows a deliberate manual model selection to
    /// probe an account again": an account limit spends the provider until a manual selection clears it.
    #[test]
    fn an_account_limit_spends_the_provider_until_a_manual_selection(){
        let mut controller=controller();let model=controller.deps.current.0.clone();
        controller.note_unusable(&model,SelectorFailure{retry_after_ms:None,error_message:Some("You have hit your session limit")});
        assert!(controller.is_provider_spent("p"));assert_eq!(controller.account_error("p").as_deref(),Some("You have hit your session limit"));
        controller.note_unusable(&model,SelectorFailure{retry_after_ms:None,error_message:Some("429 rate limit exceeded")});
        controller.clear_for_manual_model_change(&model);assert!(!controller.is_provider_spent("p"));assert_eq!(controller.account_error("p"),None);
    }
}
#[derive(Debug,Clone)]
pub enum FallbackEvent {
    Applied {from:String,to:String,chain_key:String,reason:FallbackReason},
    Reverted {from:String,to:String},
}
pub trait RetryFallbackDeps {
    fn settings(&self)->ResolvedRetryFallbackSettings;
    fn models(&self)->Vec<Model>;
    fn current(&self)->Option<(Model,Option<ModelThinkingLevel>)>;
    fn is_auth_available(&self,provider:&str)->bool;
    fn is_using_oauth(&self,_model:&Model)->bool {false}
    fn is_fallback_eligible(&self,_model:&Model)->bool {true}
    /// senpi `registry.hasConfiguredAuth`: ranks configured-key providers ahead of unconfigured ones.
    fn has_configured_auth(&self,_model:&Model)->bool {false}
    fn switch_model<'a>(&'a mut self,model:Model,thinking:ModelThinkingLevel,revert:bool)->maho_ai::types::BoxFuture<'a,Result<(),String>>;
    fn emit(&mut self,event:FallbackEvent);
    /// senpi `deps.logger`: the fallback decision trail in `logs/fallback.log`.
    fn log(&mut self,_level:FallbackLogLevel,_event:&str,_data:&[(&str,Option<&str>)]) {}
}
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum FallbackLogLevel {Debug,Info}
pub struct RetryFallbackController<D> {
    pub deps:D,pub cooldowns:SelectorCooldowns,pub state:Option<ActiveFallbackState>,pub exhausted_chain_key:Option<String>,
    tried:HashSet<String>,cache:Option<(FallbackChains,FallbackChains)>,
    /// Conservatively skip the refused provider for this turn: this synchronous scan cannot resolve
    /// asynchronously prepared request credentials, including per-model endpoints or keys (senpi#2491).
    refused_providers:HashSet<String>,
    /// senpi `UnusableEntries.accountErrors`: the raw provider error that spent each account.
    account_errors:HashMap<String,String>,
}
impl<D:RetryFallbackDeps> RetryFallbackController<D> {
    pub fn new(deps:D,cooldowns:SelectorCooldowns)->Self {Self{deps,cooldowns,state:None,exhausted_chain_key:None,tried:HashSet::new(),cache:None,refused_providers:HashSet::new(),account_errors:HashMap::new()}}
    pub fn reset_turn(&mut self){self.tried.clear();self.refused_providers.clear();self.exhausted_chain_key=None;}
    pub fn clear(&mut self){self.state=None;self.cache=None;self.reset_turn();}
    fn canonical_chains(&mut self)->FallbackChains {
        let chains=self.deps.settings().chains;
        if let Some((key,value))=&self.cache&&key==&chains{return value.clone();}
        let models=self.deps.models();let oauth=|m:&Model|self.deps.is_using_oauth(m);let eligible=|m:&Model|self.deps.is_fallback_eligible(m);let configured=|m:&Model|self.deps.has_configured_auth(m);
        let canonical=canonicalize_fallback_chains(&chains,&models,&FallbackAuthTiers{is_using_oauth:&oauth,has_configured_auth:Some(&configured),is_fallback_eligible:Some(&eligible)});
        self.cache=Some((chains,canonical.clone()));canonical
    }
    /// senpi `nextCandidate(reserve, logDecision)`: `canTryFallback` is silent, `tryFallback` logs.
    fn next_candidate(&mut self,log_decision:bool,refused_provider:Option<&str>)->Option<(String,FallbackSelector,Model)> {
        if !self.deps.settings().model_fallback{return None;}
        let (current,thinking)=self.deps.current()?;let chains=self.canonical_chains();
        let Some((key,entries))=resolve_chain_key(&current,thinking,&chains).or_else(||self.state.as_ref().map(|s|s.chain_key.clone()))
            .and_then(|key|chains.get(&key).cloned().map(|entries|(key,entries))) else {
            if log_decision{let selector=format_selector(&current,None);self.deps.log(FallbackLogLevel::Debug,"no_chain",&[("selector",Some(selector.as_str()))]);}
            return None;
        };
        let models=self.deps.models();
        for raw in candidates_after(&entries,&format_selector(&current,thinking)) {
            let Some(selector)=parse_fallback_selector(raw,&models) else {self.skip(raw,"unknown");continue;};
            if selector.provider==current.provider&&selector.id==current.id{self.skip(raw,"self");continue;}
            if refused_provider==Some(selector.provider.as_str())||self.refused_providers.contains(&selector.provider){self.skip(raw,"request-refused");continue;}
            let base=base_selector(&selector);
            if self.tried.contains(&base){self.skip(raw,"tried");continue;}
            if self.cooldowns.is_suppressed(&base){self.skip(raw,"suppressed");continue;}
            if !self.deps.is_auth_available(&selector.provider){self.skip(raw,"unauthenticated");continue;}
            let Some(model)=models.iter().find(|m|m.provider==selector.provider&&m.id==selector.id) else {self.skip(raw,"unknown");continue;};
            return Some((key,selector,model.clone()));
        }
        self.exhausted_chain_key=Some(key.clone());
        if log_decision{self.deps.log(FallbackLogLevel::Info,"candidates_exhausted",&[("chainKey",Some(key.as_str()))]);}
        None
    }
    fn skip(&mut self,candidate:&str,skip_reason:&str){self.deps.log(FallbackLogLevel::Debug,"candidate_skipped",&[("candidate",Some(candidate)),("skipReason",Some(skip_reason))]);}
    pub fn can_try_fallback(&mut self)->bool {self.next_candidate(false,None).is_some()}
    /// senpi `canTryFallback(refusedProvider)`: the provider whose request-level refusal is being decided,
    /// before the walk records it.
    pub fn can_try_fallback_refused(&mut self,refused_provider:Option<&str>)->bool {self.next_candidate(false,refused_provider).is_some()}
    /// senpi `tryFallback(.., {refusedProvider})`: records the refused provider for the rest of the turn.
    pub fn note_refused_provider(&mut self,provider:&str){self.refused_providers.insert(provider.to_owned());}
    pub fn has_configured_chain(&mut self)->bool {let Some((model,thinking))=self.deps.current() else{return false;};resolve_chain_key(&model,thinking,&self.canonical_chains()).is_some()}
    pub async fn try_fallback(&mut self,reason:FallbackReason,failure:SelectorFailure<'_>)->Result<bool,String> {
        let Some((current,current_thinking))=self.deps.current() else{return Ok(false);};
        let Some((key,selector,model))=self.next_candidate(true,None) else{return Ok(false);};
        let from=format_selector(&current,None);let to=format_selector(&model,None);
        if matches!(reason,FallbackReason::Transient|FallbackReason::HardError|FallbackReason::Billing){
            self.cooldowns.note(&from,failure);
            self.deps.log(FallbackLogLevel::Info,"cooldown_noted",&[("selector",Some(from.as_str())),("errorMessage",failure.error_message)]);
        }
        let thinking=maho_ai::models::clamp_thinking_level(&model,selector.thinking_level.or(current_thinking).unwrap_or(ModelThinkingLevel::Off));
        self.deps.switch_model(model,thinking,false).await?;self.tried.insert(base_selector(&selector));
        let prior=self.state.take();let refusal=prior.as_ref().is_some_and(|s|s.pinned_by_refusal)||reason==FallbackReason::Refusal;let billing=prior.as_ref().is_some_and(|s|s.pinned_by_billing)||reason==FallbackReason::Billing;
        self.state=Some(ActiveFallbackState{chain_key:key.clone(),original_selector:prior.as_ref().map_or_else(||from.clone(),|s|s.original_selector.clone()),original_thinking_level:prior.as_ref().and_then(|s|s.original_thinking_level).or(current_thinking),last_applied_thinking_level:Some(thinking),pinned_by_refusal:refusal,pinned_by_billing:billing,pinned:refusal||billing});
        let reason_name=match reason{FallbackReason::Transient=>"transient",FallbackReason::Refusal=>"refusal",FallbackReason::HardError=>"hard-error",FallbackReason::Billing=>"billing"};
        self.deps.log(FallbackLogLevel::Info,"fallback_applied",&[("from",Some(from.as_str())),("to",Some(to.as_str())),("chainKey",Some(key.as_str())),("reason",Some(reason_name))]);
        self.deps.emit(FallbackEvent::Applied{from,to,chain_key:key,reason});Ok(true)
    }
    fn unusable(&mut self)->super::unusable::UnusableEntries<'_> {super::unusable::UnusableEntries::new(&mut self.cooldowns,&mut self.account_errors)}
    /// senpi `noteHealthFailure` (unusable half): an account limit spends the whole provider, a model limit
    /// or billing/quota exhaustion parks that entry, on the session's own cooldown clock.
    pub fn note_unusable(&mut self,model:&Model,failure:SelectorFailure<'_>){self.unusable().note(model,failure);}
    /// senpi `isProviderSpent`: the provider's account hit a usage limit that has not yet reset.
    pub fn is_provider_spent(&mut self,provider:&str)->bool {self.unusable().is_provider_spent(provider)}
    /// senpi `accountError`: the raw error that spent a still-spent provider.
    pub fn account_error(&mut self,provider:&str)->Option<String> {self.unusable().account_error(provider)}
    /// The spent accounts as of now, for readers that cannot await this controller.
    pub fn spent_accounts(&mut self)->super::unusable::SpentAccounts {self.unusable().spent_accounts()}
    pub fn note_manual_thinking_level(&mut self){if let Some(state)=&mut self.state{state.last_applied_thinking_level=None;}}
    pub fn notify_compaction_applied(&mut self)->bool {
        let Some(state)=&mut self.state else{return false;};let was=state.pinned;state.pinned_by_refusal=false;state.pinned=state.pinned_by_billing;
        if !was||state.pinned{return false;}
        let (chain_key,original)=(state.chain_key.clone(),state.original_selector.clone());
        self.deps.log(FallbackLogLevel::Info,"refusal_pin_released",&[("chainKey",Some(chain_key.as_str())),("originalSelector",Some(original.as_str())),("trigger",Some("compaction"))]);
        true
    }
    pub fn clear_for_manual_model_change(&mut self,model:&Model){
        let selector=format_selector(model,None);
        if self.state.is_some(){self.deps.log(FallbackLogLevel::Info,"fallback_cleared_manual",&[("selector",Some(selector.as_str()))]);}
        self.state=None;self.cooldowns.clear(&selector);self.unusable().clear(model);
    }
    pub async fn maybe_restore_primary(&mut self,policy:FallbackRevertPolicy)->Result<bool,String> {
        let Some(state)=self.state.clone() else{return Ok(false);};
        if state.pinned||policy!=FallbackRevertPolicy::CooldownExpiry||self.cooldowns.is_suppressed(&state.original_selector){return Ok(false);}
        let models=self.deps.models();let Some(selector)=parse_fallback_selector(&state.original_selector,&models) else{return Ok(false);};
        if !self.deps.is_auth_available(&selector.provider){return Ok(false);}
        let Some(model)=models.into_iter().find(|m|m.provider==selector.provider&&m.id==selector.id) else{return Ok(false);};
        let Some((current,level))=self.deps.current() else{return Ok(false);};
        let thinking=if level==state.last_applied_thinking_level{state.original_thinking_level.or(level)}else{level}.unwrap_or(ModelThinkingLevel::Off);
        self.deps.switch_model(model,thinking,true).await?;let from=format_selector(&current,None);self.state=None;
        self.deps.log(FallbackLogLevel::Info,"fallback_reverted",&[("from",Some(from.as_str())),("to",Some(state.original_selector.as_str()))]);
        self.deps.emit(FallbackEvent::Reverted{from,to:state.original_selector});Ok(true)
    }
}
