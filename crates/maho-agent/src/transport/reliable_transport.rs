//! Port of senpi `packages/agent/src/transport/reliable-transport.ts`.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use futures::FutureExt;
use maho_ai::types::BoxFuture;
use tokio::sync::oneshot;

use super::transport::{
    CarrierConnection, CarrierMessage, CarrierSide, FrameHandler, Transport, TransportClock,
    TransportClosedError, TransportFrame, TransportSide, TransportTimerCancel,
};

/// The reference transport's wire messages: per-lane sequence numbers from 1, and cumulative ACKs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferencePacket {
    Data {
        lane: String,
        seq: u64,
        payload: Vec<u8>,
    },
    Ack {
        lane: String,
        seq: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodedReferencePacket {
    Data { lane: String, seq: u64 },
    Ack { lane: String, seq: u64 },
    Other,
}

pub fn decode_reference_packet(message: &CarrierMessage) -> DecodedReferencePacket {
    match message.downcast_ref::<ReferencePacket>() {
        Some(ReferencePacket::Data { lane, seq, .. }) => DecodedReferencePacket::Data {
            lane: lane.clone(),
            seq: *seq,
        },
        Some(ReferencePacket::Ack { lane, seq }) => DecodedReferencePacket::Ack {
            lane: lane.clone(),
            seq: *seq,
        },
        None => DecodedReferencePacket::Other,
    }
}

pub type OnHandlerError = Arc<dyn Fn(&str, &TransportFrame) + Send + Sync>;

pub struct ReliableTransportOptions {
    pub clock: Arc<dyn TransportClock>,
    pub window: Option<usize>,
    pub retransmit_ms: Option<i64>,
    pub on_handler_error: OnHandlerError,
}

struct Waiter {
    payload: Vec<u8>,
    tx: oneshot::Sender<Result<(), TransportClosedError>>,
}

struct OutLane {
    next_seq: u64,
    acked: u64,
    transmitted: u64,
    outbox: BTreeMap<u64, Vec<u8>>,
    waiters: VecDeque<Waiter>,
    retransmit: Option<RetransmitTimer>,
    timer_generation: u64,
}

enum RetransmitTimer {
    Arming(u64),
    Armed(u64, TransportTimerCancel),
}

impl OutLane {
    fn new() -> Self {
        Self {
            next_seq: 1,
            acked: 0,
            transmitted: 0,
            outbox: BTreeMap::new(),
            waiters: VecDeque::new(),
            retransmit: None,
            timer_generation: 0,
        }
    }
}

struct InLane {
    received: u64,
    delivered: u64,
    busy: bool,
    inbox: VecDeque<TransportFrame>,
}

impl InLane {
    fn new() -> Self {
        Self {
            received: 0,
            delivered: 0,
            busy: false,
            inbox: VecDeque::new(),
        }
    }
}

struct State {
    out_lanes: HashMap<String, OutLane>,
    in_lanes: HashMap<String, InLane>,
    order: Vec<String>,
    ack_due: BTreeSet<String>,
    next: usize,
    connection: Option<Arc<dyn CarrierConnection>>,
    connection_generation: u64,
    writable: bool,
    writable_generation: u64,
    pumping: bool,
    closed: bool,
    handler: Option<FrameHandler>,
}

impl State {
    fn out_lane(&mut self, lane: &str) -> &mut OutLane {
        if !self.out_lanes.contains_key(lane) {
            self.out_lanes.insert(lane.to_owned(), OutLane::new());
            self.order.push(lane.to_owned());
        }
        self.out_lanes.get_mut(lane).expect("inserted above")
    }

    fn in_lane(&mut self, lane: &str) -> &mut InLane {
        if !self.in_lanes.contains_key(lane) {
            self.in_lanes.insert(lane.to_owned(), InLane::new());
        }
        self.in_lanes.get_mut(lane).expect("inserted above")
    }

    fn admit(state: &mut OutLane, window: usize) {
        while (state.next_seq - 1 - state.acked) < (window as u64) {
            let Some(waiter) = state.waiters.pop_front() else {
                return;
            };
            let seq = state.next_seq;
            state.outbox.insert(seq, waiter.payload);
            state.next_seq += 1;
            let _ = waiter.tx.send(Ok(()));
        }
    }

    fn disarm(state: &mut OutLane) -> Option<TransportTimerCancel> {
        match state.retransmit.take() {
            Some(RetransmitTimer::Armed(_, cancel)) => Some(cancel),
            Some(RetransmitTimer::Arming(_)) | None => None,
        }
    }

    fn disarm_all(&mut self) -> Vec<TransportTimerCancel> {
        self.out_lanes
            .values_mut()
            .filter_map(Self::disarm)
            .collect()
    }
}

#[derive(Clone)]
pub struct ReliableTransport {
    clock: Arc<dyn TransportClock>,
    window: usize,
    retransmit_ms: i64,
    on_handler_error: OnHandlerError,
    state: Arc<Mutex<State>>,
}

impl ReliableTransport {
    fn arm(&self) {
        let lanes: Vec<String> = {
            let guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if guard.closed || guard.connection.is_none() {
                return;
            }
            guard.out_lanes.keys().cloned().collect()
        };
        for lane_name in lanes {
            let generation = {
                let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
                if guard.closed || guard.connection.is_none() {
                    return;
                }
                let Some(lane) = guard.out_lanes.get_mut(&lane_name) else {
                    continue;
                };
                if lane.retransmit.is_some() || lane.outbox.is_empty() {
                    continue;
                }
                lane.timer_generation += 1;
                let generation = lane.timer_generation;
                lane.retransmit = Some(RetransmitTimer::Arming(generation));
                generation
            };
            let transport = self.clone();
            let lane_for_cb = lane_name.clone();
            let cancel = self.clock.set_timer(
                Box::new(move || {
                    let should_pump = {
                        let mut guard = transport.state.lock().unwrap_or_else(|p| p.into_inner());
                        let Some(lane) = guard.out_lanes.get_mut(&lane_for_cb) else {
                            return;
                        };
                        if !matches!(
                            lane.retransmit,
                            Some(RetransmitTimer::Arming(id) | RetransmitTimer::Armed(id, _))
                                if id == generation
                        ) {
                            false
                        } else {
                            lane.retransmit = None;
                            lane.transmitted = lane.acked;
                            true
                        }
                    };
                    if should_pump {
                        transport.pump();
                    }
                }),
                self.retransmit_ms,
            );
            let installed = {
                let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
                let active = !guard.closed && guard.connection.is_some();
                if let Some(lane) = guard.out_lanes.get_mut(&lane_name)
                    && active
                    && !lane.outbox.is_empty()
                    && matches!(lane.retransmit, Some(RetransmitTimer::Arming(id)) if id == generation)
                {
                    lane.retransmit = Some(RetransmitTimer::Armed(generation, Arc::clone(&cancel)));
                    true
                } else {
                    false
                }
            };
            if !installed {
                cancel();
            }
        }
    }

    fn pump(&self) {
        {
            let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if guard.pumping {
                return;
            }
            guard.pumping = true;
        }
        loop {
            let action = {
                let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
                let Some(connection) = guard.connection.as_ref().map(Arc::clone) else {
                    guard.pumping = false;
                    return;
                };
                if guard.closed || !guard.writable {
                    guard.pumping = false;
                    return;
                }
                let packet = if let Some(lane) = guard.ack_due.first().cloned() {
                    guard.ack_due.remove(&lane);
                    let seq = guard.in_lane(&lane).delivered;
                    Some(ReferencePacket::Ack { lane, seq })
                } else {
                    let order_len = guard.order.len();
                    let mut packet = None;
                    for _ in 0..order_len {
                        let lane = guard.order[guard.next % order_len].clone();
                        guard.next += 1;
                        let state = guard.out_lane(&lane);
                        let seq = state.transmitted + 1;
                        if let Some(payload) = state.outbox.get(&seq).cloned() {
                            state.transmitted = seq;
                            packet = Some(ReferencePacket::Data { lane, seq, payload });
                            break;
                        }
                    }
                    packet
                };
                match packet {
                    Some(packet) => Some((
                        connection,
                        guard.connection_generation,
                        guard.writable_generation,
                        packet,
                    )),
                    None => {
                        guard.pumping = false;
                        None
                    }
                }
            };
            let Some((connection, connection_generation, writable_generation, packet)) = action
            else {
                self.arm();
                return;
            };
            if !connection.send(CarrierMessage::new(packet.clone())) {
                let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
                match packet {
                    ReferencePacket::Ack { lane, .. } => {
                        guard.ack_due.insert(lane);
                    }
                    ReferencePacket::Data { lane, seq, .. } => {
                        if let Some(state) = guard.out_lanes.get_mut(&lane) {
                            state.transmitted = state.transmitted.min(seq - 1).max(state.acked);
                        }
                    }
                }
                if guard.connection_generation == connection_generation
                    && guard.writable_generation == writable_generation
                {
                    guard.writable = false;
                }
            }
        }
    }

    fn start_deliver(&self, lane: String) {
        let handler = {
            let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let Some(handler) = guard.handler.as_ref().map(Arc::clone) else {
                return;
            };
            let in_lane = guard.in_lane(&lane);
            if in_lane.busy {
                return;
            }
            in_lane.busy = true;
            handler
        };

        static FALLBACK: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        let rt = tokio::runtime::Handle::try_current().unwrap_or_else(|_| {
            FALLBACK
                .get_or_init(|| {
                    tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(1)
                        .enable_all()
                        .build()
                        .expect("create transport delivery runtime")
                })
                .handle()
                .clone()
        });
        let transport = self.clone();
        rt.spawn(async move {
            transport.deliver_loop(lane, handler).await;
        });
    }

    async fn deliver_loop(self, lane: String, handler: FrameHandler) {
        struct BusyGuard {
            state: Arc<Mutex<State>>,
            lane: String,
            armed: bool,
        }
        impl Drop for BusyGuard {
            fn drop(&mut self) {
                if self.armed {
                    let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(lane) = state.in_lanes.get_mut(&self.lane) {
                        lane.busy = false;
                    }
                }
            }
        }
        let mut busy = BusyGuard {
            state: Arc::clone(&self.state),
            lane: lane.clone(),
            armed: true,
        };
        loop {
            let frame = {
                let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
                if guard.closed {
                    if let Some(in_lane) = guard.in_lanes.get_mut(&lane) {
                        in_lane.busy = false;
                    }
                    busy.armed = false;
                    return;
                }
                let next_frame = guard
                    .in_lanes
                    .get_mut(&lane)
                    .and_then(|in_lane| in_lane.inbox.pop_front());
                match next_frame {
                    Some(frame) => frame,
                    None => {
                        if let Some(in_lane) = guard.in_lanes.get_mut(&lane) {
                            in_lane.busy = false;
                        }
                        busy.armed = false;
                        return;
                    }
                }
            };
            let result = std::panic::AssertUnwindSafe(async { handler(frame.clone()).await })
                .catch_unwind()
                .await;
            if let Err(error) = result {
                let message = error
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| error.downcast_ref::<&str>().copied())
                    .unwrap_or("frame handler panicked");
                (self.on_handler_error)(message, &frame);
            }
            {
                let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
                guard.in_lane(&lane).delivered += 1;
                guard.ack_due.insert(lane.clone());
            }
            self.pump();
        }
    }

    fn on_data(&self, guard: &mut State, lane: String, seq: u64, payload: Vec<u8>) -> bool {
        {
            let state = guard.in_lane(&lane);
            if seq == state.received + 1 {
                state.received = seq;
                state.inbox.push_back(TransportFrame { lane, payload });
                true
            } else {
                guard.ack_due.insert(lane);
                false
            }
        }
    }

    fn on_ack(
        &self,
        guard: &mut State,
        lane: String,
        ack_seq: u64,
    ) -> Option<TransportTimerCancel> {
        let state = guard.out_lanes.get_mut(&lane)?;
        if ack_seq <= state.acked || ack_seq >= state.next_seq {
            return None;
        }
        for seq in (state.acked + 1)..=ack_seq {
            state.outbox.remove(&seq);
        }
        state.acked = ack_seq;
        state.transmitted = state.transmitted.max(state.acked);
        let cancel = State::disarm(state);
        State::admit(state, self.window);
        cancel
    }
}

pub fn create_reliable_transport(options: ReliableTransportOptions) -> Arc<ReliableTransport> {
    Arc::new(ReliableTransport {
        clock: options.clock,
        window: options.window.unwrap_or(32),
        retransmit_ms: options.retransmit_ms.unwrap_or(1000),
        on_handler_error: options.on_handler_error,
        state: Arc::new(Mutex::new(State {
            out_lanes: HashMap::new(),
            in_lanes: HashMap::new(),
            order: Vec::new(),
            ack_due: BTreeSet::new(),
            next: 0,
            connection: None,
            connection_generation: 0,
            writable: false,
            writable_generation: 0,
            pumping: false,
            closed: false,
            handler: None,
        })),
    })
}

impl Transport for ReliableTransport {
    fn send(
        &self,
        lane: &str,
        payload: Vec<u8>,
    ) -> BoxFuture<'_, Result<(), TransportClosedError>> {
        let lane = lane.to_owned();
        Box::pin(async move {
            let rx = {
                let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
                if guard.closed {
                    return Err(TransportClosedError);
                }
                let (tx, rx) = oneshot::channel();
                let window = self.window;
                let state = guard.out_lane(&lane);
                state.waiters.push_back(Waiter { payload, tx });
                State::admit(state, window);
                rx
            };
            self.pump();
            rx.await.unwrap_or(Err(TransportClosedError))
        })
    }

    fn on_frame(&self, handler: FrameHandler) {
        let lanes: Vec<String> = {
            let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            guard.handler = Some(handler);
            guard.in_lanes.keys().cloned().collect()
        };
        for lane in lanes {
            self.start_deliver(lane);
        }
    }

    fn in_flight(&self, lane: &str) -> usize {
        let guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
        guard
            .out_lanes
            .get(lane)
            .map_or(0, |state| (state.next_seq - 1 - state.acked) as usize)
    }

    fn close(&self) {
        let (current, cancels) = {
            let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if guard.closed {
                return;
            }
            guard.closed = true;
            let cancels = guard.disarm_all();
            for state in guard.out_lanes.values_mut() {
                while let Some(waiter) = state.waiters.pop_front() {
                    let _ = waiter.tx.send(Err(TransportClosedError));
                }
            }
            guard.connection_generation += 1;
            (guard.connection.take(), cancels)
        };
        for cancel in cancels {
            cancel();
        }
        if let Some(conn) = current {
            conn.close();
        }
    }
}

impl CarrierSide for ReliableTransport {
    fn attached(&self, new_connection: Arc<dyn CarrierConnection>) {
        let (closed, cancels) = {
            let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if guard.closed {
                (true, Vec::new())
            } else {
                guard.connection = Some(Arc::clone(&new_connection));
                guard.connection_generation += 1;
                guard.writable = true;
                guard.writable_generation += 1;
                for state in guard.out_lanes.values_mut() {
                    state.transmitted = state.acked;
                }
                let ack_lanes: Vec<String> = guard
                    .in_lanes
                    .iter()
                    .filter(|(_, s)| s.delivered > 0)
                    .map(|(l, _)| l.clone())
                    .collect();
                for lane in ack_lanes {
                    guard.ack_due.insert(lane);
                }
                let cancels = guard.disarm_all();
                (false, cancels)
            }
        };
        for cancel in cancels {
            cancel();
        }
        if closed {
            new_connection.close();
        } else {
            self.pump();
        }
    }

    fn message(&self, message: &CarrierMessage) {
        let Some(packet) = message.downcast_ref::<ReferencePacket>().cloned() else {
            return;
        };
        let (deliver_lane, cancel) = {
            let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            match packet {
                ReferencePacket::Data { lane, seq, payload } => {
                    if self.on_data(&mut guard, lane.clone(), seq, payload) {
                        (Some(lane), None)
                    } else {
                        (None, None)
                    }
                }
                ReferencePacket::Ack { lane, seq } => (None, self.on_ack(&mut guard, lane, seq)),
            }
        };
        if let Some(cancel) = cancel {
            cancel();
        }
        self.pump();
        if let Some(lane) = deliver_lane {
            self.start_deliver(lane);
        }
    }

    fn writable(&self) {
        {
            let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if guard.connection.is_none() {
                return;
            }
            guard.writable = true;
            guard.writable_generation += 1;
        }
        self.pump();
    }

    fn detached(&self) {
        let cancels = {
            let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            guard.connection = None;
            guard.connection_generation += 1;
            guard.writable = false;
            guard.disarm_all()
        };
        for cancel in cancels {
            cancel();
        }
    }
}

impl TransportSide for ReliableTransport {
    fn as_transport(&self) -> Arc<dyn Transport> {
        Arc::new(self.clone())
    }

    fn as_carrier_side(&self) -> Arc<dyn CarrierSide> {
        Arc::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Observations = Arc<Mutex<Vec<(&'static str, bool)>>>;
    struct Probe {
        state: Mutex<std::sync::Weak<Mutex<State>>>,
        observed: Observations,
    }
    impl Probe {
        fn record(&self, action: &'static str) {
            let state = self
                .state
                .lock()
                .expect("probe state")
                .upgrade()
                .expect("transport alive");
            self.observed
                .lock()
                .expect("observations")
                .push((action, state.try_lock().is_ok()));
        }
    }
    impl CarrierConnection for Probe {
        fn send(&self, _: CarrierMessage) -> bool {
            self.record("send");
            true
        }
        fn close(&self) {}
    }
    struct ProbeClock(Arc<Probe>);
    impl TransportClock for ProbeClock {
        fn now(&self) -> i64 {
            0
        }
        fn set_timer(&self, _: Box<dyn FnOnce() + Send + 'static>, _: i64) -> TransportTimerCancel {
            self.0.record("set timer");
            let probe = self.0.clone();
            Arc::new(move || probe.record("cancel timer"))
        }
    }

    #[tokio::test]
    async fn external_carrier_and_timer_callbacks_run_without_state_lock() {
        let probe = Arc::new(Probe {
            state: Mutex::new(std::sync::Weak::new()),
            observed: Arc::new(Mutex::new(Vec::new())),
        });
        let transport = create_reliable_transport(ReliableTransportOptions {
            clock: Arc::new(ProbeClock(probe.clone())),
            window: Some(4),
            retransmit_ms: Some(100),
            on_handler_error: Arc::new(|error, _| panic!("unexpected handler error: {error}")),
        });
        *probe.state.lock().expect("probe state") = Arc::downgrade(&transport.state);
        transport.attached(probe.clone());
        transport
            .send("lane", b"payload".to_vec())
            .await
            .expect("admit frame");
        transport.close();
        assert_eq!(
            *probe.observed.lock().expect("observations"),
            [("send", true), ("set timer", true), ("cancel timer", true)]
        );
    }

    struct ReentrantCarrier {
        transport: Mutex<std::sync::Weak<ReliableTransport>>,
        sends: std::sync::atomic::AtomicUsize,
        acknowledge: bool,
    }
    impl CarrierConnection for ReentrantCarrier {
        fn send(&self, message: CarrierMessage) -> bool {
            let transport = self
                .transport
                .lock()
                .expect("transport")
                .upgrade()
                .expect("alive");
            let count = self.sends.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.acknowledge {
                if let Some(ReferencePacket::Data { lane, seq, .. }) =
                    message.downcast_ref::<ReferencePacket>()
                {
                    transport.message(&CarrierMessage::new(ReferencePacket::Ack {
                        lane: lane.clone(),
                        seq: *seq,
                    }));
                }
                true
            } else if count == 0 {
                transport.writable();
                false
            } else {
                true
            }
        }
        fn close(&self) {}
    }

    #[tokio::test]
    async fn reentrant_ack_and_writable_notifications_preserve_progress() {
        for acknowledge in [true, false] {
            let probe = Arc::new(Probe {
                state: Mutex::new(std::sync::Weak::new()),
                observed: Arc::new(Mutex::new(Vec::new())),
            });
            let transport = create_reliable_transport(ReliableTransportOptions {
                clock: Arc::new(ProbeClock(probe.clone())),
                window: Some(4),
                retransmit_ms: Some(100),
                on_handler_error: Arc::new(|error, _| panic!("unexpected handler error: {error}")),
            });
            *probe.state.lock().expect("probe state") = Arc::downgrade(&transport.state);
            let carrier = Arc::new(ReentrantCarrier {
                transport: Mutex::new(Arc::downgrade(&transport)),
                sends: std::sync::atomic::AtomicUsize::new(0),
                acknowledge,
            });
            transport.attached(carrier.clone());
            transport
                .send("lane", b"payload".to_vec())
                .await
                .expect("admit frame");
            if acknowledge {
                assert_eq!(transport.in_flight("lane"), 0);
                assert_eq!(carrier.sends.load(std::sync::atomic::Ordering::SeqCst), 1);
            } else {
                assert_eq!(transport.in_flight("lane"), 1);
                assert_eq!(carrier.sends.load(std::sync::atomic::Ordering::SeqCst), 2);
            }
            transport.close();
        }
    }
}
