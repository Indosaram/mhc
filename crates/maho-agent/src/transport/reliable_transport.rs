//! Port of senpi `packages/agent/src/transport/reliable-transport.ts`.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

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
    cancel_retransmit: Option<TransportTimerCancel>,
}

impl OutLane {
    fn new() -> Self {
        Self {
            next_seq: 1,
            acked: 0,
            transmitted: 0,
            outbox: BTreeMap::new(),
            waiters: VecDeque::new(),
            cancel_retransmit: None,
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
    writable: bool,
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

    fn disarm(state: &mut OutLane) {
        if let Some(cancel) = state.cancel_retransmit.take() {
            cancel();
        }
    }

    fn disarm_all(&mut self) {
        for state in self.out_lanes.values_mut() {
            Self::disarm(state);
        }
    }

    fn emit(&mut self, packet: ReferencePacket) -> bool {
        let Some(conn) = self.connection.as_ref().map(Arc::clone) else {
            return false;
        };
        if !self.writable {
            return false;
        }
        if conn.send(CarrierMessage::new(packet)) {
            true
        } else {
            self.writable = false;
            false
        }
    }
}

#[derive(Clone)]
pub struct ReliableTransport {
    clock: Arc<dyn TransportClock>,
    window: usize,
    retransmit_ms: i64,
    #[allow(dead_code)]
    on_handler_error: OnHandlerError,
    state: Arc<Mutex<State>>,
}

impl ReliableTransport {
    fn arm(&self, guard: &mut State) {
        if guard.connection.is_none() {
            return;
        }
        let lanes: Vec<String> = guard
            .out_lanes
            .iter()
            .filter(|(_, lane)| lane.cancel_retransmit.is_none() && !lane.outbox.is_empty())
            .map(|(name, _)| name.clone())
            .collect();
        for lane_name in lanes {
            let transport = self.clone();
            let lane_for_cb = lane_name.clone();
            let cancel = self.clock.set_timer(
                Box::new(move || {
                    let mut guard = transport.state.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(lane) = guard.out_lanes.get_mut(&lane_for_cb) {
                        lane.cancel_retransmit = None;
                        lane.transmitted = lane.acked;
                    }
                    transport.pump(&mut guard);
                }),
                self.retransmit_ms,
            );
            if let Some(lane) = guard.out_lanes.get_mut(&lane_name) {
                lane.cancel_retransmit = Some(cancel);
            }
        }
    }

    fn pump(&self, guard: &mut State) {
        if guard.connection.is_none() || !guard.writable {
            return;
        }
        let ack_lanes: Vec<String> = guard.ack_due.iter().cloned().collect();
        for lane in ack_lanes {
            let seq = guard.in_lane(&lane).delivered;
            if !guard.emit(ReferencePacket::Ack {
                lane: lane.clone(),
                seq,
            }) {
                return;
            }
            guard.ack_due.remove(&lane);
        }
        let mut idle = 0usize;
        let order_len = guard.order.len();
        while idle < order_len {
            let lane = guard.order[guard.next % order_len].clone();
            let (seq, payload) = {
                let state = guard.out_lane(&lane);
                let seq = state.transmitted + 1;
                (seq, state.outbox.get(&seq).cloned())
            };
            let Some(payload) = payload else {
                guard.next += 1;
                idle += 1;
                continue;
            };
            if !guard.emit(ReferencePacket::Data {
                lane: lane.clone(),
                seq,
                payload,
            }) {
                return;
            }
            if let Some(state) = guard.out_lanes.get_mut(&lane) {
                state.transmitted = seq;
            }
            guard.next += 1;
            idle = 0;
        }
        self.arm(guard);
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

        let transport = self.clone();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                transport.deliver_loop(lane, handler).await;
            });
        }
    }

    async fn deliver_loop(self, lane: String, handler: FrameHandler) {
        loop {
            let frame = {
                let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
                if guard.closed {
                    if let Some(in_lane) = guard.in_lanes.get_mut(&lane) {
                        in_lane.busy = false;
                    }
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
                        return;
                    }
                }
            };
            handler(frame).await;
            let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            guard.in_lane(&lane).delivered += 1;
            guard.ack_due.insert(lane.clone());
            self.pump(&mut guard);
        }
    }

    fn on_data(&self, guard: &mut State, lane: String, seq: u64, payload: Vec<u8>) -> bool {
        let should_deliver = {
            let state = guard.in_lane(&lane);
            if seq == state.received + 1 {
                state.received = seq;
                state.inbox.push_back(TransportFrame { lane, payload });
                true
            } else {
                guard.ack_due.insert(lane);
                false
            }
        };
        self.pump(guard);
        should_deliver
    }

    fn on_ack(&self, guard: &mut State, lane: String, ack_seq: u64) {
        let Some(state) = guard.out_lanes.get_mut(&lane) else {
            return;
        };
        if ack_seq <= state.acked || ack_seq >= state.next_seq {
            return;
        }
        for seq in (state.acked + 1)..=ack_seq {
            state.outbox.remove(&seq);
        }
        state.acked = ack_seq;
        state.transmitted = state.transmitted.max(state.acked);
        State::disarm(state);
        State::admit(state, self.window);
        self.pump(guard);
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
            writable: false,
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
                self.pump(&mut guard);
                rx
            };
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
        let current = {
            let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if guard.closed {
                return;
            }
            guard.closed = true;
            guard.disarm_all();
            for state in guard.out_lanes.values_mut() {
                while let Some(waiter) = state.waiters.pop_front() {
                    let _ = waiter.tx.send(Err(TransportClosedError));
                }
            }
            guard.connection.take()
        };
        if let Some(conn) = current {
            conn.close();
        }
    }
}

impl CarrierSide for ReliableTransport {
    fn attached(&self, new_connection: Arc<dyn CarrierConnection>) {
        let closed = {
            let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if guard.closed {
                true
            } else {
                guard.connection = Some(Arc::clone(&new_connection));
                guard.writable = true;
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
                guard.disarm_all();
                self.pump(&mut guard);
                false
            }
        };
        if closed {
            new_connection.close();
        }
    }

    fn message(&self, message: &CarrierMessage) {
        let Some(packet) = message.downcast_ref::<ReferencePacket>().cloned() else {
            return;
        };
        let deliver_lane = {
            let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            match packet {
                ReferencePacket::Data { lane, seq, payload } => {
                    if self.on_data(&mut guard, lane.clone(), seq, payload) {
                        Some(lane)
                    } else {
                        None
                    }
                }
                ReferencePacket::Ack { lane, seq } => {
                    self.on_ack(&mut guard, lane, seq);
                    None
                }
            }
        };
        if let Some(lane) = deliver_lane {
            self.start_deliver(lane);
        }
    }

    fn writable(&self) {
        let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if guard.connection.is_none() {
            return;
        }
        guard.writable = true;
        self.pump(&mut guard);
    }

    fn detached(&self) {
        let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
        guard.connection = None;
        guard.writable = false;
        guard.disarm_all();
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
