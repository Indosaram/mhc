use std::sync::{Arc, Mutex, OnceLock};
use crate::errors::SdkErrorKind;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderAccountEvent {
    AccountsChanged { provider: String },
    Failover { provider: String, from: String, to: String, reason: SdkErrorKind },
}
type Listener = Arc<dyn Fn(&ProviderAccountEvent) + Send + Sync>;
#[derive(Default)]
pub struct ProviderAccountEvents { listeners: Mutex<Vec<Listener>> }
impl ProviderAccountEvents {
    pub fn subscribe(self: &Arc<Self>, listener: Listener) -> impl FnOnce() + Send + 'static {
        let mut listeners = self.listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !listeners.iter().any(|existing| Arc::ptr_eq(existing, &listener)) { listeners.push(listener.clone()); }
        let events = self.clone();
        move || events.listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|existing| !Arc::ptr_eq(existing, &listener))
    }
    pub fn emit(&self, event: ProviderAccountEvent) {
        let listeners = self.listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        for listener in listeners {
            if self.listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().any(|existing| Arc::ptr_eq(existing, &listener)) { listener(&event); }
        }
    }
}
pub fn provider_account_events() -> &'static Arc<ProviderAccountEvents> {
    static EVENTS: OnceLock<Arc<ProviderAccountEvents>> = OnceLock::new();
    EVENTS.get_or_init(|| Arc::new(ProviderAccountEvents::default()))
}
pub fn emit_provider_accounts_changed(provider: &str) {
    provider_account_events().emit(ProviderAccountEvent::AccountsChanged { provider: provider.into() });
    maho_core::provider_account_events::emit_provider_accounts_changed(provider);
}
pub fn emit_provider_account_failover(provider: &str, from: &str, to: &str, reason: SdkErrorKind) {
    provider_account_events().emit(ProviderAccountEvent::Failover { provider:provider.into(),from:from.into(),to:to.into(),reason });
    maho_core::provider_account_events::emit_provider_account_failover(provider, from, to, sdk_error_kind_name(reason));
}
fn sdk_error_kind_name(kind: SdkErrorKind) -> &'static str {
    match kind {
        SdkErrorKind::RateLimit => "rate_limit", SdkErrorKind::Overloaded => "overloaded",
        SdkErrorKind::AuthError => "auth_error", SdkErrorKind::Billing => "billing",
        SdkErrorKind::OrgNotAllowed => "org_not_allowed", SdkErrorKind::Entitlement => "entitlement",
        SdkErrorKind::Other => "other",
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn subscription_is_ordered_unique_and_explicitly_removed() {
        let events=Arc::new(ProviderAccountEvents::default());let observed=Arc::new(Mutex::new(Vec::new()));
        let listener: Listener={let observed=observed.clone();Arc::new(move |event|observed.lock().expect("events").push(event.clone()))};
        let unsubscribe=events.subscribe(listener.clone());let duplicate=events.subscribe(listener);
        let changed=ProviderAccountEvent::AccountsChanged {provider:"provider".into()};events.emit(changed.clone());
        assert_eq!(observed.lock().expect("events").as_slice(),std::slice::from_ref(&changed));unsubscribe();events.emit(changed);duplicate();assert_eq!(observed.lock().expect("events").len(),1);
    }
    #[test]
    fn listener_can_unsubscribe_before_a_later_listener_runs() {
        let events=Arc::new(ProviderAccountEvents::default());let later=Arc::new(Mutex::new(None::<Box<dyn FnOnce()+Send>>));let calls=Arc::new(Mutex::new(Vec::new()));
        let first={let later=later.clone();let calls=calls.clone();Arc::new(move |_:&ProviderAccountEvent| {calls.lock().expect("calls").push(1);if let Some(remove)=later.lock().expect("remove").take() {remove();}}) as Listener};
        let remove_first=events.subscribe(first);let second={let calls=calls.clone();Arc::new(move |_:&ProviderAccountEvent|calls.lock().expect("calls").push(2)) as Listener};
        *later.lock().expect("remove")=Some(Box::new(events.subscribe(second)));events.emit(ProviderAccountEvent::Failover {provider:"p".into(),from:"a".into(),to:"b".into(),reason:SdkErrorKind::RateLimit});
        assert_eq!(*calls.lock().expect("calls"),[1]);remove_first();
    }
    #[test]
    fn the_builtin_emitters_forward_into_the_core_registry() {
        use maho_core::provider_account_events::ProviderAccountEvent as CoreEvent;
        let observed=Arc::new(Mutex::new(Vec::new()));
        let sink=observed.clone();
        let unsubscribe=maho_core::provider_account_events::subscribe_provider_account_events(Arc::new(move |event| {
            if matches!(&event,CoreEvent::AccountsChanged { provider } if provider=="forward-fixture")||matches!(&event,CoreEvent::Failover { provider,.. } if provider=="forward-fixture") { sink.lock().expect("observed").push(event); }
        }));
        emit_provider_accounts_changed("forward-fixture");
        emit_provider_account_failover("forward-fixture","a","b",SdkErrorKind::RateLimit);
        unsubscribe();
        assert_eq!(observed.lock().expect("observed").as_slice(),[
            CoreEvent::AccountsChanged { provider:"forward-fixture".into() },
            CoreEvent::Failover { provider:"forward-fixture".into(),from:"a".into(),to:"b".into(),reason:"rate_limit".into() },
        ]);
    }
}
