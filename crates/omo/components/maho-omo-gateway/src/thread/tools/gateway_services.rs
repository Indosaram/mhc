//! Composition of store, engine, relay, and local delivery pipeline (`thread/tools/gateway-services.ts`).

use std::sync::Arc;

use super::super::adapter::GatewayEndpointPort;
use super::super::address_book::to_gateway_address_entries;
use super::super::engine::{
    GatewayEngine, GatewayEngineOptions, GatewayResolve, resolve_from_entries,
};
use super::super::local_transport::LocalTransport;
use super::super::pipeline::DeliveryPipeline;
use super::super::provenance_resolver::{ProvenanceRuntime, resolve_provenance};
use super::super::relay::{GatewayRelay, GatewayRelayOptions, LocateCallback};
use super::super::store_api::GatewayStoreApi;
use super::super::types::Provenance;
use super::internals::{address_book, host_view, send_address_book_async};
use super::ports::{ThreadHostViewRequest, ThreadToolSurfaceOptions};
use super::send_view::send_view;

pub struct GatewayServices {
    pub store: Arc<dyn GatewayStoreApi>,
    pub endpoints: GatewayEndpointPort,
    pub engine: GatewayEngine,
    pub pipeline: Arc<DeliveryPipeline>,
    pub relay: GatewayRelay,
    pub sender_provenance: Arc<dyn Fn() -> Provenance + Send + Sync>,
    pub locate: LocateCallback,
}

pub fn create_gateway_resolver(options: Arc<ThreadToolSurfaceOptions>) -> GatewayResolve {
    Arc::new(move |address: &str, all_scope: bool| {
        let options = options.clone();
        let address = address.to_owned();
        Box::pin(async move {
            let current = host_view(&options, ThreadHostViewRequest { offline: true })
                .await
                .unwrap_or_default();
            let entries = to_gateway_address_entries(
                &send_address_book_async(&options, &current, &address, all_scope).await,
            );
            let root_fn = options.caller_workspace_root.clone();
            resolve_from_entries(Arc::new(move || entries.clone()), root_fn)(&address, all_scope)
                .await
        })
    })
}

pub fn create_gateway_services(options: Arc<ThreadToolSurfaceOptions>) -> GatewayServices {
    let opts_res = options.clone();
    let resolve: GatewayResolve = Arc::new(move |address: &str, all_scope: bool| {
        let options = opts_res.clone();
        let address = address.to_owned();
        Box::pin(async move {
            let current = send_view(&options, &address, true)
                .await
                .unwrap_or_default();
            let entries = to_gateway_address_entries(
                &send_address_book_async(&options, &current, &address, all_scope).await,
            );
            let root_fn = options.caller_workspace_root.clone();
            resolve_from_entries(Arc::new(move || entries.clone()), root_fn)(&address, all_scope)
                .await
        })
    });

    let store = options.store.clone();
    let endpoints = options
        .host
        .gateway()
        .unwrap_or_else(GatewayEndpointPort::inert);

    let opts_prov = options.clone();
    let sender_provenance: Arc<dyn Fn() -> Provenance + Send + Sync> = Arc::new(move || {
        let mut rt = ProvenanceRuntime {
            cwd: std::path::PathBuf::from((opts_prov.caller_workspace_root)()),
            env: std::env::vars().collect(),
            ..Default::default()
        };
        if let Some(loader) = &opts_prov.load_provenance_workspaces {
            rt.workspaces = Some(loader(&rt.cwd));
        }
        resolve_provenance(&rt)
    });

    let engine = GatewayEngine::new(GatewayEngineOptions {
        sender_provenance: Some(sender_provenance.clone()),
        store: store.clone(),
        endpoints: endpoints.clone(),
        resolve,
        now: options.now.clone(),
    });

    let opts_loc = options.clone();
    let locate: LocateCallback = Arc::new(move |durable_id: &str| {
        let options = opts_loc.clone();
        let durable_id = durable_id.to_owned();
        Box::pin(async move {
            let view = host_view(&options, ThreadHostViewRequest { offline: true })
                .await
                .unwrap_or_default();
            let entries = to_gateway_address_entries(&address_book(&options, &view, &[]));
            entries
                .into_iter()
                .find(|e| e.thread_id == durable_id)
                .filter(|e| e.liveness == super::super::adapter::EndpointLiveness::Routable)
                .and_then(|e| e.endpoint)
        })
    });

    let relay = GatewayRelay::new(GatewayRelayOptions {
        store: store.clone(),
        engine: engine.clone(),
        endpoints: endpoints.clone(),
        locate: locate.clone(),
        now: options.now.clone(),
    });

    let local = Arc::new(LocalTransport {
        engine: Arc::new(engine.clone()),
        same_device_wake: options.same_device_wake.clone(),
    });
    let pipeline = Arc::new(DeliveryPipeline::new(local, None));

    GatewayServices {
        store,
        endpoints,
        engine,
        pipeline,
        relay,
        sender_provenance,
        locate,
    }
}
