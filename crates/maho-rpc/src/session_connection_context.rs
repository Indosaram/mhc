//! Port of senpi `packages/coding-agent/src/modes/rpc/session-connection-context.ts`.
//!
//! Task-local capture of connection registrations before awaits; a routing string is never an
//! incarnation.

use crate::delegation_record::DelegationDescriptor;
use crate::extension_owner::{ExtensionOwner, ExtensionOwnerIdentity, ExtensionTransportOwner};
use std::sync::Arc;

#[derive(Clone)]
pub struct CapturedConnection {
    pub id: String,
    pub owner: Option<Arc<ExtensionTransportOwner>>,
    pub binding: Option<ExtensionOwner>,
    pub delegation: Option<DelegationDescriptor>,
}

tokio::task_local! {
    static CURRENT_CONNECTION: Option<CapturedConnection>;
}

type OwnerLookup = Arc<dyn Fn(&str) -> Option<Arc<ExtensionTransportOwner>> + Send + Sync>;
type DelegationLookup = Arc<dyn Fn(&str) -> Option<DelegationDescriptor> + Send + Sync>;

#[derive(Clone)]
pub struct SessionConnectionContext {
    owner_of: OwnerLookup,
    delegation_of: DelegationLookup,
}

impl SessionConnectionContext {
    pub fn new(owner_of: OwnerLookup, delegation_of: Option<DelegationLookup>) -> Self {
        Self {
            owner_of,
            delegation_of: delegation_of.unwrap_or_else(|| Arc::new(|_| None)),
        }
    }

    pub async fn run<T>(&self, id: &str, fut: impl std::future::Future<Output = T>) -> T {
        let captured = CapturedConnection {
            id: id.to_owned(),
            owner: (self.owner_of)(id),
            binding: None,
            delegation: (self.delegation_of)(id),
        };
        CURRENT_CONNECTION.scope(Some(captured), fut).await
    }

    pub async fn outside<T>(&self, fut: impl std::future::Future<Output = T>) -> T {
        CURRENT_CONNECTION.scope(None, fut).await
    }

    pub async fn run_captured<T>(
        &self,
        id: &str,
        identity: &ExtensionOwnerIdentity,
        fut: impl std::future::Future<Output = T>,
    ) -> T {
        let owner = (self.owner_of)(id);
        let binding = owner.as_ref().and_then(|o| {
            if o.owner_id == identity.owner_id {
                o.peek(&identity.binding_generation)
            } else {
                None
            }
        });
        let captured = CapturedConnection {
            id: id.to_owned(),
            owner: binding.as_ref().and(owner),
            binding,
            delegation: None,
        };
        CURRENT_CONNECTION.scope(Some(captured), fut).await
    }

    pub async fn run_issued<T>(
        &self,
        id: &str,
        binding: ExtensionOwner,
        fut: impl std::future::Future<Output = T>,
    ) -> T {
        let captured = CapturedConnection {
            id: id.to_owned(),
            owner: None,
            binding: Some(binding),
            delegation: None,
        };
        CURRENT_CONNECTION.scope(Some(captured), fut).await
    }

    pub fn current_id(&self) -> Option<String> {
        CURRENT_CONNECTION
            .try_with(|c| c.as_ref().map(|v| v.id.clone()))
            .ok()
            .flatten()
    }

    pub fn current_delegation(&self) -> Option<DelegationDescriptor> {
        CURRENT_CONNECTION
            .try_with(|c| c.as_ref().and_then(|v| v.delegation.clone()))
            .ok()
            .flatten()
    }

    pub fn current_transport(&self) -> Option<Arc<ExtensionTransportOwner>> {
        CURRENT_CONNECTION
            .try_with(|c| c.as_ref().and_then(|v| v.owner.clone()))
            .ok()
            .flatten()
    }

    pub fn current_owner(&self) -> Option<ExtensionOwner> {
        CURRENT_CONNECTION
            .try_with(|c| c.as_ref().and_then(|v| v.binding.clone()))
            .ok()
            .flatten()
    }

    pub fn transport_of(&self, id: &str) -> Option<Arc<ExtensionTransportOwner>> {
        (self.owner_of)(id)
    }

    pub async fn with_binding<T>(
        &self,
        generation: &str,
        fut: impl std::future::Future<Output = T>,
    ) -> T {
        let captured = CURRENT_CONNECTION.try_with(|c| c.clone()).ok().flatten();
        let Some(mut cap) = captured else {
            return fut.await;
        };
        if let Some(ref owner) = cap.owner {
            cap.binding = owner.capture(generation).ok();
        }
        CURRENT_CONNECTION.scope(Some(cap), fut).await
    }

    pub fn is_current(&self) -> bool {
        self.current_guard()()
    }

    pub fn current_guard(&self) -> Box<dyn Fn() -> bool + Send + Sync> {
        let captured = CURRENT_CONNECTION.try_with(|c| c.clone()).ok().flatten();
        let Some(cap) = captured else {
            return Box::new(|| true);
        };
        let owner_of = Arc::clone(&self.owner_of);
        Box::new(move || {
            let Some(ref captured_owner) = cap.owner else {
                return false;
            };
            let current = owner_of(&cap.id);
            current.as_ref().is_some_and(|c| {
                Arc::ptr_eq(c, captured_owner)
                    && !captured_owner.retired()
                    && cap.binding.as_ref().is_none_or(|b| !b.retired())
            })
        })
    }
}
