//! Ports `runners/input-effects.test.ts`.

use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;

use super::*;
use crate::state::TaskStatus;
use crate::store::StateDirConfig;
use crate::test_support::base_record;

const TASK: &str = "st_0000e1f0";
const BOUND: Duration = Duration::from_secs(5);

struct Fixture {
    _dir: tempfile::TempDir,
    store: TaskRecordStore,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = TaskRecordStore::new(&StateDirConfig {
        project_dir: dir.path().to_path_buf(),
        task_state_dir: None,
    });
    let mut record = base_record(TASK, "parent");
    record.status = TaskStatus::Running;
    store.save(&record).expect("save");
    Fixture { _dir: dir, store }
}

fn reload(store: &TaskRecordStore) -> TaskRecordStore {
    TaskRecordStore::new(&StateDirConfig {
        project_dir: std::path::PathBuf::from("/nonexistent"),
        task_state_dir: Some(store.state_dir().to_path_buf()),
    })
}

fn events(store: &TaskRecordStore) -> Vec<Value> {
    std::fs::read_to_string(store.state_dir().join("logs").join(format!("{TASK}.jsonl")))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn event_types(store: &TaskRecordStore) -> Vec<String> {
    events(store)
        .iter()
        .filter_map(|event| event["type"].as_str().map(str::to_string))
        .collect()
}

fn steer(message: &str, id: Option<&str>) -> InputCommand {
    InputCommand {
        kind: InputCommandType::Steer,
        message: message.to_string(),
        streaming_behavior: None,
        client_message_id: id.map(str::to_string),
    }
}

fn timeout() -> InputWriteError {
    InputWriteError::Timeout {
        deadline_ms: Some(30_000),
    }
}

fn effects(
    store: &TaskRecordStore,
    send: impl Fn(&IdentifiedInput) -> Result<(), InputWriteError> + Send + Sync + 'static,
    read: Option<InputAdmissionRead>,
) -> InputEffects {
    create_input_effects(InputEffectsOptions {
        task_id: TASK.to_string(),
        store: Some(store.clone()),
        send: Arc::new(send),
        read,
    })
}

fn bump_run_epoch(store: &TaskRecordStore) {
    store
        .mutate(TASK, |record| {
            let mut next = record.clone();
            next.notification.run_epoch += 1;
            next
        })
        .expect("mutate");
}

/// A sender that asserts the id's sequence is already durable before the write.
fn durable_sender(
    store: TaskRecordStore,
    ids: Arc<Mutex<Vec<String>>>,
) -> impl Fn(&IdentifiedInput) -> Result<(), InputWriteError> + Send + Sync + 'static {
    move |command: &IdentifiedInput| {
        let fresh = reload(&store).load(TASK).expect("load").expect("record");
        let seq: u64 = command
            .client_message_id
            .rsplit(':')
            .next()
            .and_then(|seq| seq.parse().ok())
            .expect("seq");
        assert_eq!(fresh.effect_seq, Some(seq));
        ids.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(command.client_message_id.clone());
        Ok(())
    }
}

#[test]
fn ids_are_persisted_before_writes_and_never_reused() {
    let fx = fixture();
    let ids = Arc::new(Mutex::new(Vec::new()));
    effects(
        &fx.store,
        durable_sender(fx.store.clone(), Arc::clone(&ids)),
        None,
    )
    .send(InputCommand {
        kind: InputCommandType::Prompt,
        message: "initial".into(),
        streaming_behavior: None,
        client_message_id: None,
    })
    .expect("send");
    let reloaded = reload(&fx.store);
    effects(
        &reloaded,
        durable_sender(reloaded.clone(), Arc::clone(&ids)),
        None,
    )
    .send(steer("second", None))
    .expect("send");
    assert_eq!(
        *ids.lock().unwrap_or_else(PoisonError::into_inner),
        vec![format!("{TASK}:0:1"), format!("{TASK}:0:2")]
    );
}

#[test]
fn one_background_retry_resolves_uncertainty_with_the_same_id_and_payload() {
    let fx = fixture();
    let commands = Arc::new(Mutex::new(Vec::<IdentifiedInput>::new()));
    let seen = Arc::clone(&commands);
    let effects = effects(
        &fx.store,
        move |command| {
            let mut seen = seen.lock().unwrap_or_else(PoisonError::into_inner);
            seen.push(command.clone());
            if seen.len() == 1 {
                Err(timeout())
            } else {
                Ok(())
            }
        },
        None,
    );
    let error = effects.send(steer("once", None)).expect_err("uncertain");
    assert!(error.is_uncertain());
    assert!(error.to_string().contains("do not resend"));
    effects.join_scheduled();
    let commands = commands.lock().unwrap_or_else(PoisonError::into_inner);
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[1].wire(), commands[0].wire());
    assert_eq!(event_types(&fx.store), vec!["effect_uncertain", "steered"]);
    assert_eq!(
        fx.store
            .load(TASK)
            .expect("load")
            .expect("record")
            .uncertain_inputs,
        None
    );
}

#[test]
fn a_rebound_id_fails_with_an_action_and_no_delivery_event() {
    let fx = fixture();
    let calls = Arc::new(Mutex::new(0));
    let counter = Arc::clone(&calls);
    let effects = effects(
        &fx.store,
        move |_| {
            let mut calls = counter.lock().unwrap_or_else(PoisonError::into_inner);
            *calls += 1;
            if *calls == 1 {
                Err(timeout())
            } else {
                Err(InputWriteError::Refused {
                    code: Some("client_message_id_conflict".into()),
                    message: "client_message_id_conflict".into(),
                })
            }
        },
        None,
    );
    let error = effects.send(steer("once", None)).expect_err("uncertain");
    assert!(error.is_uncertain());
    effects.join_scheduled();
    let failed = events(&fx.store)
        .into_iter()
        .find(|event| event["type"] == "effect_failed")
        .expect("failed");
    assert_eq!(failed["payload"]["reason"], EFFECT_ID_CONFLICT_REASON);
    assert_eq!(failed["payload"]["action"], EFFECT_ID_CONFLICT_ACTION);
    assert_eq!(
        event_types(&fx.store),
        vec!["effect_uncertain", "effect_failed"]
    );
}

#[test]
fn a_reloaded_sender_reconciles_with_the_original_identity() {
    let fx = fixture();
    let (retried, retry) = mpsc::channel();
    let commands = Arc::new(Mutex::new(Vec::<IdentifiedInput>::new()));
    let seen = Arc::clone(&commands);
    let first = effects(
        &fx.store,
        move |command| {
            let mut seen = seen.lock().unwrap_or_else(PoisonError::into_inner);
            seen.push(command.clone());
            if seen.len() == 2 {
                let _ = retried.send(());
            }
            Err(timeout())
        },
        None,
    );
    assert!(
        first
            .send(steer("once", None))
            .expect_err("uncertain")
            .is_uncertain()
    );
    retry.recv_timeout(BOUND).expect("retry");
    first.join_scheduled();
    let pending = fx
        .store
        .load(TASK)
        .expect("load")
        .expect("record")
        .uncertain_inputs;
    assert_eq!(pending.map(|inputs| inputs.len()), Some(1));
    let seen = Arc::clone(&commands);
    let reloaded = reload(&fx.store);
    effects(
        &reloaded,
        move |command| {
            seen.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(command.clone());
            Ok(())
        },
        None,
    )
    .reconcile();
    let commands = commands.lock().unwrap_or_else(PoisonError::into_inner);
    assert_eq!(commands.len(), 3);
    assert_eq!(commands[2].wire(), commands[0].wire());
}

#[test]
fn the_uncertain_reason_names_the_deadline_or_the_lost_connection() {
    let fx = fixture();
    let causes = Arc::new(Mutex::new(vec![
        InputWriteError::TransportGone,
        InputWriteError::Timeout {
            deadline_ms: Some(45_000),
        },
    ]));
    let effects = effects(
        &fx.store,
        move |_| {
            Err(causes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop()
                .expect("cause"))
        },
        None,
    );
    effects.stop_retrying();
    let timed_out = effects.send(steer("a", None)).expect_err("uncertain");
    let lost = effects.send(steer("b", None)).expect_err("uncertain");
    assert!(timed_out.is_uncertain() && timed_out.to_string().contains("45 s"));
    assert!(lost.is_uncertain());
    assert!(lost.to_string().contains("connection to the host was lost"));
    assert!(!lost.to_string().contains("45 s"));
}

#[test]
fn an_earlier_run_input_is_not_delivered_into_a_later_run() {
    let fx = fixture();
    let first = effects(&fx.store, |_| Err(timeout()), None);
    first.stop_retrying();
    let prompt = InputCommand {
        kind: InputCommandType::Prompt,
        message: "old run".into(),
        streaming_behavior: None,
        client_message_id: None,
    };
    assert!(first.send(prompt).expect_err("uncertain").is_uncertain());
    bump_run_epoch(&fx.store);
    let sent = Arc::new(Mutex::new(0));
    let counter = Arc::clone(&sent);
    effects(
        &reload(&fx.store),
        move |_| {
            *counter.lock().unwrap_or_else(PoisonError::into_inner) += 1;
            Ok(())
        },
        None,
    )
    .reconcile();
    assert_eq!(*sent.lock().unwrap_or_else(PoisonError::into_inner), 0);
    let failed = events(&fx.store)
        .into_iter()
        .filter(|event| event["type"] == "effect_failed")
        .map(|event| event["payload"]["reason"].clone())
        .collect::<Vec<_>>();
    assert_eq!(failed, vec![Value::from("superseded_by_new_run")]);
    assert_eq!(
        fx.store
            .load(TASK)
            .expect("load")
            .expect("record")
            .uncertain_inputs,
        None
    );
}

#[test]
fn an_in_flight_retry_alone_settles_an_earlier_run_input() {
    let fx = fixture();
    let (started_tx, started) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let released = Arc::new(Mutex::new(released));
    let calls = Arc::new(Mutex::new(0));
    let counter = Arc::clone(&calls);
    let effects = effects(
        &fx.store,
        move |_| {
            let call = {
                let mut calls = counter.lock().unwrap_or_else(PoisonError::into_inner);
                *calls += 1;
                *calls
            };
            if call == 1 {
                return Err(timeout());
            }
            let _ = started_tx.send(());
            let _ = released
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .recv_timeout(BOUND);
            Ok(())
        },
        None,
    );
    assert!(
        effects
            .send(steer("once", None))
            .expect_err("uncertain")
            .is_uncertain()
    );
    started.recv_timeout(BOUND).expect("retry started");
    bump_run_epoch(&fx.store);
    let reconciling = effects.clone();
    let reconciled = std::thread::spawn(move || reconciling.reconcile());
    release.send(()).expect("release");
    reconciled.join().expect("reconcile");
    effects.join_scheduled();
    assert_eq!(event_types(&fx.store), vec!["effect_uncertain", "steered"]);
}

#[test]
fn a_callers_own_id_is_sent_as_it_is_and_no_id_is_minted() {
    let fx = fixture();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&sent);
    effects(
        &fx.store,
        move |command| {
            seen.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(command.wire());
            Ok(())
        },
        None,
    )
    .send(steer("queued", Some("ps-1-entry")))
    .expect("send");
    assert_eq!(
        *sent.lock().unwrap_or_else(PoisonError::into_inner),
        vec![
            serde_json::json!({"type": "steer", "message": "queued", "clientMessageId": "ps-1-entry"})
        ]
    );
    let record = fx.store.load(TASK).expect("load").expect("record");
    assert_eq!(record.last_effect_id.as_deref(), Some("ps-1-entry"));
    assert_eq!(record.effect_seq, None);
}

fn read_all(admission: InputAdmission) -> InputAdmissionRead {
    Arc::new(move |ids: &[String]| Ok(Some(ids.iter().map(|id| (id.clone(), admission)).collect())))
}

#[test]
fn an_admitted_lost_acknowledgment_is_settled_by_the_read_and_not_resent() {
    let fx = fixture();
    let sent = Arc::new(Mutex::new(0));
    let counter = Arc::clone(&sent);
    let reads = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let observed = Arc::clone(&reads);
    let read: InputAdmissionRead = Arc::new(move |ids: &[String]| {
        observed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(ids.to_vec());
        Ok(Some(
            ids.iter()
                .map(|id| (id.clone(), InputAdmission::Admitted))
                .collect(),
        ))
    });
    let effects = effects(
        &fx.store,
        move |_| {
            *counter.lock().unwrap_or_else(PoisonError::into_inner) += 1;
            Err(timeout())
        },
        Some(read),
    );
    let error = effects
        .send(steer("once", Some("ps-1-entry")))
        .expect_err("uncertain");
    assert!(error.is_uncertain());
    effects.join_scheduled();
    assert_eq!(*sent.lock().unwrap_or_else(PoisonError::into_inner), 1);
    assert_eq!(
        *reads.lock().unwrap_or_else(PoisonError::into_inner),
        vec![vec!["ps-1-entry".to_string()]]
    );
    assert_eq!(event_types(&fx.store), vec!["effect_uncertain", "steered"]);
    assert_eq!(
        fx.store
            .load(TASK)
            .expect("load")
            .expect("record")
            .uncertain_inputs,
        None
    );
}

#[test]
fn a_never_admitted_lost_acknowledgment_is_sent_again_under_the_same_id() {
    let fx = fixture();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&sent);
    let effects = effects(
        &fx.store,
        move |command| {
            let mut seen = seen.lock().unwrap_or_else(PoisonError::into_inner);
            seen.push(command.wire());
            if seen.len() == 1 {
                Err(timeout())
            } else {
                Ok(())
            }
        },
        Some(read_all(InputAdmission::NotAdmitted)),
    );
    let error = effects
        .send(steer("once", Some("ps-1-entry")))
        .expect_err("uncertain");
    assert!(error.is_uncertain());
    effects.join_scheduled();
    let wire =
        serde_json::json!({"type": "steer", "message": "once", "clientMessageId": "ps-1-entry"});
    assert_eq!(
        *sent.lock().unwrap_or_else(PoisonError::into_inner),
        vec![wire.clone(), wire]
    );
}

#[test]
fn a_failed_admission_read_keeps_the_input_uncertain_and_sends_nothing() {
    let fx = fixture();
    let (read_tx, read_rx) = mpsc::channel();
    let read_tx = Mutex::new(read_tx);
    let sent = Arc::new(Mutex::new(0));
    let counter = Arc::clone(&sent);
    let effects = effects(
        &fx.store,
        move |_| {
            *counter.lock().unwrap_or_else(PoisonError::into_inner) += 1;
            Err(timeout())
        },
        Some(Arc::new(move |_: &[String]| {
            let _ = read_tx
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .send(());
            Err("host unreachable".to_string())
        })),
    );
    assert!(
        effects
            .send(steer("once", Some("ps-1-entry")))
            .expect_err("uncertain")
            .is_uncertain()
    );
    read_rx.recv_timeout(BOUND).expect("read");
    effects.join_scheduled();
    effects.reconcile();
    assert_eq!(*sent.lock().unwrap_or_else(PoisonError::into_inner), 1);
    let ids: Vec<String> = fx
        .store
        .load(TASK)
        .expect("load")
        .expect("record")
        .uncertain_inputs
        .unwrap_or_default()
        .into_iter()
        .map(|input| input.client_message_id)
        .collect();
    assert_eq!(ids, vec!["ps-1-entry"]);
}

#[test]
fn an_earlier_run_input_the_child_admitted_is_settled_not_superseded() {
    let fx = fixture();
    let first = effects(&fx.store, |_| Err(timeout()), None);
    first.stop_retrying();
    assert!(
        first
            .send(steer("old run", Some("ps-1-entry")))
            .expect_err("uncertain")
            .is_uncertain()
    );
    let record = fx.store.load(TASK).expect("load").expect("record");
    assert_eq!(
        record.uncertain_inputs.expect("uncertain")[0].run_epoch,
        Some(record.notification.run_epoch)
    );
    bump_run_epoch(&fx.store);
    let sent = Arc::new(Mutex::new(0));
    let counter = Arc::clone(&sent);
    effects(
        &reload(&fx.store),
        move |_| {
            *counter.lock().unwrap_or_else(PoisonError::into_inner) += 1;
            Ok(())
        },
        Some(read_all(InputAdmission::Admitted)),
    )
    .reconcile();
    assert_eq!(*sent.lock().unwrap_or_else(PoisonError::into_inner), 0);
    assert_eq!(event_types(&fx.store), vec!["effect_uncertain", "steered"]);
    assert_eq!(
        fx.store
            .load(TASK)
            .expect("load")
            .expect("record")
            .uncertain_inputs,
        None
    );
}

#[test]
fn an_earlier_run_input_with_a_failed_read_stays_uncertain() {
    let fx = fixture();
    let first = effects(&fx.store, |_| Err(timeout()), None);
    first.stop_retrying();
    assert!(
        first
            .send(steer("old run", Some("ps-1-entry")))
            .expect_err("uncertain")
            .is_uncertain()
    );
    bump_run_epoch(&fx.store);
    effects(
        &reload(&fx.store),
        |_| panic!("an earlier run's input is never sent"),
        Some(Arc::new(|_: &[String]| Err("host unreachable".to_string()))),
    )
    .reconcile();
    assert_eq!(event_types(&fx.store), vec!["effect_uncertain"]);
    let ids: Vec<String> = reload(&fx.store)
        .load(TASK)
        .expect("load")
        .expect("record")
        .uncertain_inputs
        .unwrap_or_default()
        .into_iter()
        .map(|input| input.client_message_id)
        .collect();
    assert_eq!(ids, vec!["ps-1-entry"]);
}

#[test]
fn uncertain_inputs_round_trip_through_the_record_parser() {
    let fx = fixture();
    let first = effects(&fx.store, |_| Err(timeout()), None);
    first.stop_retrying();
    assert!(
        first
            .send(steer("kept", Some("ps-9")))
            .expect_err("uncertain")
            .is_uncertain()
    );
    let record = reload(&fx.store).load(TASK).expect("load").expect("record");
    let inputs = record.uncertain_inputs.expect("uncertain");
    assert_eq!(inputs[0].kind, InputCommandType::Steer);
    assert_eq!(inputs[0].message, "kept");
    assert_eq!(inputs[0].client_message_id, "ps-9");
}
