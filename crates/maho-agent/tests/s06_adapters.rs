//! Shared in-memory / manual-timer / reliable-transport adapters for S06 conformance suites.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use maho_agent::durable::store::{DurableStore, DurableStoreError, DurableStoreOptions};
use maho_agent::durable::store_memory::{
    MemoryStoreMedium, create_memory_store_medium, open_memory_store,
};
use maho_agent::durable::testing::manual_timers::{ManualTimers, create_manual_timers};
use maho_agent::durable::testing::store_harness::StoreConformanceAdapter;
use maho_agent::durable::testing::wake_harness::{
    WakeAlarmSession, WakeCaseAdapters, WakeConformanceAdapter,
};
use maho_agent::durable::wake_host::WakeAlarm;
use maho_agent::durable::wake_host_store_alarm::store_alarm_persistence;
use maho_agent::durable::wake_host_timer::{
    TokioTimerHost, TokioTimerHostOptions, create_tokio_timer_host,
};
use maho_agent::execution::testing::batch_executor::{BatchCaseAdapters, reference_batch_executor};
use maho_agent::execution::testing::hand_harness::HandCaseAdapters;
use maho_agent::transport::reliable_transport::{
    DecodedReferencePacket, ReliableTransportOptions, create_reliable_transport,
    decode_reference_packet,
};
use maho_agent::transport::testing::transport_harness::{
    TransportCaseAdapters, TransportConformanceAdapter, WireView,
};
use maho_agent::transport::transport::{CarrierMessage, TransportClock, TransportSide};
use maho_ai::types::BoxFuture;

pub const DEFAULT_T0: i64 = 1_800_000_000_000;

pub struct MemoryStoreAdapter {
    media: Mutex<HashMap<String, MemoryStoreMedium>>,
}

impl Default for MemoryStoreAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryStoreAdapter {
    pub fn new() -> Self {
        Self {
            media: Mutex::new(HashMap::new()),
        }
    }
}

impl StoreConformanceAdapter for MemoryStoreAdapter {
    fn create_medium(&self) -> BoxFuture<'_, Result<String, DurableStoreError>> {
        Box::pin(async {
            let mut media = self
                .media
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let name = format!("medium-{}", media.len() + 1);
            media.insert(name.clone(), create_memory_store_medium());
            Ok(name)
        })
    }

    fn open(
        &self,
        medium: &str,
        options: DurableStoreOptions,
    ) -> BoxFuture<'_, Result<Arc<dyn DurableStore>, DurableStoreError>> {
        let medium_name = medium.to_owned();
        Box::pin(async move {
            let found = {
                let media = self
                    .media
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                media.get(&medium_name).cloned()
            };
            let Some(found) = found else {
                return Err(DurableStoreError::Other(format!(
                    "no memory medium named {medium_name}"
                )));
            };
            Ok(Arc::new(open_memory_store(found, options)) as Arc<dyn DurableStore>)
        })
    }
}

pub struct ReferenceTransportAdapter;

impl TransportConformanceAdapter for ReferenceTransportAdapter {
    fn create_side(&self, clock: Arc<dyn TransportClock>) -> Arc<dyn TransportSide> {
        create_reliable_transport(ReliableTransportOptions {
            clock,
            window: Some(8),
            retransmit_ms: Some(200),
            on_handler_error: Arc::new(|message, frame| {
                panic!("handler error on {}: {message}", frame.lane);
            }),
        })
    }

    fn decode(&self, message: &CarrierMessage) -> WireView {
        match decode_reference_packet(message) {
            DecodedReferencePacket::Data { lane, seq } => WireView::Data { lane, seq },
            DecodedReferencePacket::Ack { lane, seq } => WireView::Ack { lane, seq },
            DecodedReferencePacket::Other => WireView::Other,
        }
    }
}

pub fn memory_store_adapter() -> Arc<dyn StoreConformanceAdapter> {
    Arc::new(MemoryStoreAdapter::new())
}

pub fn reference_transport_adapter() -> Arc<dyn TransportConformanceAdapter> {
    Arc::new(ReferenceTransportAdapter)
}

pub fn transport_case_adapters(platform: &str) -> TransportCaseAdapters {
    TransportCaseAdapters {
        transport: reference_transport_adapter(),
        platform: platform.to_owned(),
    }
}

pub fn batch_case_adapters() -> BatchCaseAdapters {
    BatchCaseAdapters {
        store: memory_store_adapter(),
        executor: reference_batch_executor(),
    }
}

pub fn hand_case_adapters(platform: &str) -> HandCaseAdapters {
    HandCaseAdapters {
        store: memory_store_adapter(),
        transport: reference_transport_adapter(),
        platform: platform.to_owned(),
    }
}

pub struct MemoryWakeAdapter {
    timers: ManualTimers,
    host: Mutex<TokioTimerHost>,
}

impl MemoryWakeAdapter {
    pub fn new(t0: i64) -> Self {
        let timers = create_manual_timers(t0);
        let host = Self::open_host(&timers);
        Self {
            timers,
            host: Mutex::new(host),
        }
    }

    fn open_host(timers: &ManualTimers) -> TokioTimerHost {
        create_tokio_timer_host(TokioTimerHostOptions {
            now: Some(timers.now_fn()),
            set_timer: Some(timers.set_timer_fn()),
            max_delay_ms: None,
            on_fire_error: Arc::new(|key, error| {
                panic!("wake alarm fire error on {key}: {error}");
            }),
        })
    }
}

impl WakeConformanceAdapter for MemoryWakeAdapter {
    fn now(&self) -> i64 {
        self.timers.now()
    }

    fn advance_to(&self, at: i64) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.timers.advance_to(at).await?;
            Ok(())
        })
    }

    fn open_alarm(
        &self,
        session: WakeAlarmSession,
    ) -> BoxFuture<'_, Result<Arc<dyn WakeAlarm>, DurableStoreError>> {
        let host = self
            .host
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        Box::pin(async move {
            host.open_alarm(
                session.medium,
                store_alarm_persistence(session.store, session.epoch),
                session.on_fire,
                session.options,
            )
            .await
        })
    }

    fn restart(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            let next = Self::open_host(&self.timers);
            let old = {
                let mut guard = self
                    .host
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                std::mem::replace(&mut *guard, next)
            };
            old.close();
            Ok(())
        })
    }
}

pub fn memory_wake_case_adapters(t0: i64) -> WakeCaseAdapters {
    WakeCaseAdapters {
        store: memory_store_adapter(),
        wake: Arc::new(MemoryWakeAdapter::new(t0)),
    }
}
