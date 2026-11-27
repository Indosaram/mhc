//! Port of senpi `packages/agent/src/execution/testing/kill-hook.ts`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::testing::manual_timers::{ManualTimers, create_manual_timers};
use crate::durable::testing::store_harness::expect_equal;
use crate::execution::loop_hook_host::{LoopHookError, LoopHookHostOptions, create_loop_hook_host};
use crate::execution::loop_hook_host_hand::{
    LoopHookHand, LoopHookHandOptions, LoopHookPoint, LoopHookPointCallback, serve_loop_hooks,
};
use crate::transport::in_process_carrier::{InProcessNetworkOptions, create_in_process_network};
use crate::transport::local_endpoint::local_endpoint;
use crate::transport::testing::transport_harness::TransportCaseAdapters;
use crate::transport::transport::{CarrierSide, TransportClock};

use super::hand_harness::{DirectCarrierSide, drain_microtasks};

const LANE: &str = "loop-hooks:session";
const TICK_MS: i64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKillAt {
    Received,
    Started,
    Ran,
    Replied,
    Committed,
}

impl HookKillAt {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Received => "received",
            Self::Started => "started",
            Self::Ran => "ran",
            Self::Replied => "replied",
            Self::Committed => "committed",
        }
    }

    fn into_point(self) -> Option<LoopHookPoint> {
        match self {
            Self::Received => Some(LoopHookPoint::Received),
            Self::Started => Some(LoopHookPoint::Started),
            Self::Ran => Some(LoopHookPoint::Ran),
            Self::Replied => Some(LoopHookPoint::Replied),
            Self::Committed => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookOutcomeKind {
    Result,
    Restarted,
}

#[derive(Debug, Clone, Copy)]
struct KillHookCase {
    at: HookKillAt,
    outcome: HookOutcomeKind,
    runs: usize,
}

const KILLS: &[KillHookCase] = &[
    KillHookCase {
        at: HookKillAt::Received,
        outcome: HookOutcomeKind::Result,
        runs: 1,
    },
    KillHookCase {
        at: HookKillAt::Started,
        outcome: HookOutcomeKind::Restarted,
        runs: 0,
    },
    KillHookCase {
        at: HookKillAt::Ran,
        outcome: HookOutcomeKind::Restarted,
        runs: 1,
    },
    KillHookCase {
        at: HookKillAt::Replied,
        outcome: HookOutcomeKind::Result,
        runs: 1,
    },
    KillHookCase {
        at: HookKillAt::Committed,
        outcome: HookOutcomeKind::Result,
        runs: 1,
    },
];

fn where_desc(at: HookKillAt) -> &'static str {
    match at {
        HookKillAt::Received => "after the hand received the hook call, before it started it",
        HookKillAt::Started => "after the hand said it started the hooks, before they ran",
        HookKillAt::Ran => "after the hooks ran, before the hand sent the reply",
        HookKillAt::Replied => "after the hand sent the reply, before the loop host committed it",
        HookKillAt::Committed => {
            "after the loop host committed the reply, before the provider request"
        }
    }
}

#[derive(Debug, Clone)]
pub struct KillHookDeclaration {
    pub id: &'static str,
    pub at: HookKillAt,
    pub expected: &'static str,
}

fn expected_of(case: &KillHookCase) -> String {
    let result = match case.outcome {
        HookOutcomeKind::Result => {
            "the turn gets the hook result and issues exactly one provider request with it"
        }
        HookOutcomeKind::Restarted => {
            "the call ends restarted (uncertain), is never sent again, and no provider request is issued"
        }
    };
    let ran = if case.runs == 0 { "never" } else { "once" };
    format!(
        "cross-machine and hosted: kill {}; {result}; the hooks ran {ran}",
        where_desc(case.at)
    )
}

async fn run_kill_case(
    adapters: TransportCaseAdapters,
    kill: KillHookCase,
    what: String,
) -> ConformanceResult {
    let timers = create_manual_timers(1_000);
    let timers_net = timers.clone();
    let network = create_in_process_network(InProcessNetworkOptions {
        schedule: Some(Arc::new(move |deliver| {
            let _ = timers_net.set_timer_sync(deliver, 0);
        })),
    });
    let clock: Arc<dyn TransportClock> = Arc::new(timers.clone());
    let loop_side = adapters.transport.create_side(Arc::clone(&clock));
    let hand_side = adapters.transport.create_side(clock);
    let endpoint = local_endpoint(
        &adapters.platform,
        "loop-hook-conformance",
        "/run/senpi-conformance",
    )?;
    let _ = network.listen(
        &endpoint,
        Arc::new(DirectCarrierSide(Arc::clone(&hand_side))) as Arc<dyn CarrierSide>,
    );
    network
        .dial(
            &endpoint,
            Arc::new(DirectCarrierSide(Arc::clone(&loop_side))) as Arc<dyn CarrierSide>,
        )?
        .connect();

    let runs = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let did_kill = Arc::new(AtomicBool::new(false));
    let killed_notify = Arc::new(Notify::new());

    let start_hand: Arc<dyn Fn(Option<LoopHookPoint>) -> Arc<LoopHookHand> + Send + Sync> = {
        let hand_side = Arc::clone(&hand_side);
        let runs = Arc::clone(&runs);
        let did_kill = Arc::clone(&did_kill);
        let killed_notify = Arc::clone(&killed_notify);
        Arc::new(move |kill_at: Option<LoopHookPoint>| -> Arc<LoopHookHand> {
            let runs_c = Arc::clone(&runs);
            let did_kill_c = Arc::clone(&did_kill);
            let killed_notify_c = Arc::clone(&killed_notify);
            let on_point: Option<LoopHookPointCallback> = kill_at.map(|target| {
                Arc::new(move |point: LoopHookPoint, _call_id: &str| {
                    if point == target {
                        did_kill_c.store(true, Ordering::SeqCst);
                        killed_notify_c.notify_waiters();
                        return Err(format!("hand killed {point:?}"));
                    }
                    Ok(())
                }) as LoopHookPointCallback
            });
            serve_loop_hooks(LoopHookHandOptions {
                transport: hand_side.as_transport(),
                lane: LANE.to_string(),
                run: Arc::new(move |event, payload| {
                    let runs_inner = Arc::clone(&runs_c);
                    Box::pin(async move {
                        runs_inner
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .push(event);
                        Ok(json!({
                            "context": format!(
                                "hook saw {}",
                                serde_json::to_string(&payload).unwrap_or_default()
                            )
                        }))
                    })
                }),
                instance: None,
                on_point,
                on_fault: None,
            })
        })
    };

    async fn wait_until<F: FnMut() -> bool>(
        timers: &ManualTimers,
        what: &str,
        mut done: F,
        label: &str,
    ) -> Result<(), String> {
        let end = timers.now() + 600_000;
        drain_microtasks().await;
        while !done() && timers.now() < end {
            let _ = timers.advance_to(timers.now() + TICK_MS).await;
            drain_microtasks().await;
        }
        if !done() {
            return Err(format!("{what}: {label} not reached"));
        }
        Ok(())
    }

    let hand_slot: Arc<Mutex<Option<Arc<LoopHookHand>>>> =
        Arc::new(Mutex::new(Some(start_hand(kill.at.into_point()))));
    let initial_hand = hand_slot.lock().unwrap_or_else(|p| p.into_inner()).clone();
    if let Some(h) = initial_hand {
        h.ready().await;
    }

    let host = create_loop_hook_host(LoopHookHostOptions {
        transport: loop_side.as_transport(),
        lane: LANE.to_string(),
        mint_id: None,
    });
    let outcome: Arc<Mutex<Option<Result<Value, LoopHookError>>>> = Arc::new(Mutex::new(None));
    let outcome_c = Arc::clone(&outcome);
    let host_c = Arc::clone(&host);
    let requests_c = Arc::clone(&requests);
    let hand_slot_c = Arc::clone(&hand_slot);
    let did_kill_c = Arc::clone(&did_kill);
    let start_hand_spawn = Arc::clone(&start_hand);
    tokio::spawn(async move {
        let res = host_c
            .run("before_agent_start", json!({ "prompt": "go" }), None)
            .await;
        match res {
            Ok(result) => {
                if kill.at == HookKillAt::Committed {
                    if let Some(prev) = hand_slot_c.lock().unwrap_or_else(|p| p.into_inner()).take()
                    {
                        prev.close();
                    }
                    did_kill_c.store(true, Ordering::SeqCst);
                    let next_hand = start_hand_spawn(None);
                    next_hand.ready().await;
                    *hand_slot_c.lock().unwrap_or_else(|p| p.into_inner()) = Some(next_hand);
                }
                requests_c
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(result.clone());
                *outcome_c.lock().unwrap_or_else(|p| p.into_inner()) = Some(Ok(result));
            }
            Err(err) => {
                *outcome_c.lock().unwrap_or_else(|p| p.into_inner()) = Some(Err(err));
            }
        }
    });

    wait_until(
        &timers,
        &what,
        || did_kill.load(Ordering::SeqCst),
        "the kill point fired",
    )
    .await?;
    if kill.at != HookKillAt::Committed {
        let next_hand = start_hand(None);
        next_hand.ready().await;
        *hand_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(next_hand);
    }
    wait_until(
        &timers,
        &what,
        || outcome.lock().unwrap_or_else(|p| p.into_inner()).is_some(),
        "the turn got the hook call's outcome",
    )
    .await?;

    for _ in 0..40 {
        let _ = timers.advance_to(timers.now() + TICK_MS).await;
        drain_microtasks().await;
    }

    let final_outcome = outcome.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let req_len = requests.lock().unwrap_or_else(|p| p.into_inner()).len();
    match kill.outcome {
        HookOutcomeKind::Result => {
            expect_equal(
                &final_outcome.and_then(|r| r.ok()),
                &Some(json!({ "context": "hook saw {\"prompt\":\"go\"}" })),
                &format!("{what}: the hook result"),
            )?;
            expect_equal(&req_len, &1usize, &format!("{what}: provider requests"))?;
        }
        HookOutcomeKind::Restarted => {
            let is_restarted = matches!(final_outcome, Some(Err(LoopHookError::Restarted { .. })));
            expect_equal(
                &is_restarted,
                &true,
                &format!("{what}: the call ends restarted"),
            )?;
            expect_equal(&req_len, &0usize, &format!("{what}: provider requests"))?;
        }
    }
    expect_equal(
        &runs.lock().unwrap_or_else(|p| p.into_inner()).len(),
        &kill.runs,
        &format!("{what}: hook runs"),
    )?;
    expect_equal(
        &host.waiting(),
        &Vec::<String>::new(),
        &format!("{what}: nothing left waiting"),
    )?;
    Ok(())
}

pub fn kill_hook_declarations() -> Vec<KillHookDeclaration> {
    KILLS
        .iter()
        .enumerate()
        .map(|(index, kill)| {
            let id: &'static str =
                Box::leak(format!("KILL-HOOK-{:02}", index + 1).into_boxed_str());
            let expected: &'static str = Box::leak(expected_of(kill).into_boxed_str());
            KillHookDeclaration {
                id,
                at: kill.at,
                expected,
            }
        })
        .collect()
}

pub fn kill_hook_cases() -> BTreeMap<&'static str, CaseImplementation<TransportCaseAdapters>> {
    let mut map: BTreeMap<&'static str, CaseImplementation<TransportCaseAdapters>> =
        BTreeMap::new();
    for (index, &kill) in KILLS.iter().enumerate() {
        let id: &'static str = Box::leak(format!("KILL-HOOK-{:02}", index + 1).into_boxed_str());
        let what = format!("{id} ({})", kill.at.as_str());
        map.insert(
            id,
            Arc::new(move |adapters| {
                let w = what.clone();
                Box::pin(run_kill_case(adapters, kill, w))
            }),
        );
    }
    map
}

pub const KILL_HOOK_DECLARATIONS: fn() -> Vec<KillHookDeclaration> = kill_hook_declarations;
pub const KILL_HOOK_CASES: fn()
    -> BTreeMap<&'static str, CaseImplementation<TransportCaseAdapters>> = kill_hook_cases;
