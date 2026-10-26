use std::sync::{Arc, Mutex};

use super::TaskManager;
use super::helpers::{
    build_managed_spec, build_record_input, build_respawn_managed_spec, build_spawn_spec_v1,
};
use super::manager_tests::fakes::{FakeRunner, Project};
use super::start_admission::{
    EnterStartAdmissionInput, StartAdmissionEntry, enter_start_admission,
};
use super::types::{
    ManagedRunner, ManagedRunners, ManagerStartSpec, ResolvedChildPlan, TaskManagerOptions,
};
use crate::pause::admission_barrier::AdmissionBarrier;

#[test]
fn admitted_spec_preserves_runtime_scope_but_does_not_serialize_it() {
    let barrier = AdmissionBarrier::new();
    let spec = ManagerStartSpec {
        parent_session_id: "parent".into(),
        parent_model: Some("provider/parent".into()),
        resume_credential: {
            let authority = AdmissionBarrier::new();
            authority.close("parent", 3);
            Some(authority.resume("parent", 3, "resume").unwrap())
        },
        ..Default::default()
    };
    let mut initial = spec.clone();
    initial.resume_credential = None;
    let entered = match enter_start_admission(EnterStartAdmissionInput {
        barrier: &barrier,
        spec: &initial,
        task_id: "child".into(),
        run_ref: None,
        credential: None,
        held: None,
    }) {
        StartAdmissionEntry::Entered(entered) => entered,
        StartAdmissionEntry::Held(_) => panic!("open barrier"),
    };
    let mut admitted = entered.spec;
    admitted.resume_credential = spec.resume_credential;
    assert_eq!(
        admitted
            .admission
            .as_ref()
            .expect("admitted scope")
            .task_id(),
        "child"
    );
    let json = serde_json::to_value(&admitted).expect("serialize spec");
    assert!(json.get("admission").is_none());
    assert!(json.get("resume_credential").is_none());
    let plan = ResolvedChildPlan {
        model: "provider/child".into(),
        ..Default::default()
    };
    let record = crate::state::create_task_record(
        build_record_input(&admitted, &plan, "child", Default::default()),
        None,
    )
    .expect("record");
    assert_eq!(record.parent_model.as_deref(), Some("provider/parent"));
    let managed = build_managed_spec(
        &record,
        &admitted,
        &plan,
        "/tmp",
        std::path::Path::new("/tmp"),
        Some("trace".into()),
    );
    assert!(managed.admission.is_some());
    assert_eq!(managed.traceparent.as_deref(), Some("trace"));
    let mut persisted = record;
    persisted.spawn_spec = Some(crate::state::TaskSpawnSpec::V1(build_spawn_spec_v1(
        &managed,
    )));
    let respawn =
        build_respawn_managed_spec(&persisted, std::path::Path::new("/tmp")).expect("respawn");
    assert!(respawn.admission.is_none());
    assert!(respawn.traceparent.is_none());
    entered.scope.leave();
}

#[test]
fn workpool_preparation_resolves_parent_trace_once_and_runner_receives_it() {
    let project = Project::new();
    let store = project.store();
    let runner = FakeRunner::new();
    let runner_port: Arc<dyn ManagedRunner> = runner.clone();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let sink = calls.clone();
    let barrier = AdmissionBarrier::new();
    let mut options = TaskManagerOptions::new(
        store.clone(),
        ManagedRunners {
            in_process: runner_port.clone(),
            process: runner_port,
        },
        Arc::new(|_| Ok(ResolvedChildPlan::default())),
        project.cwd(),
    );
    options.admission_barrier = Some(barrier.clone());
    options.traceparent_for = Some(Arc::new(move |parent| {
        sink.lock().expect("calls").push(parent.to_string());
        Some("00-0123456789abcdef0123456789abcdef-0123456789abcdef-01".into())
    }));
    let manager = Arc::new(TaskManager::new(options));
    let admission = super::workpool_admission::ManagerWorkpoolAdmission::new(manager.clone());
    barrier.close("root", 7);
    assert!(crate::workpool::ports::WorkpoolAdmission::delivery_held(
        &admission, "root"
    ));
    let spec = crate::workpool::types::WorkpoolSpec {
        start: ManagerStartSpec {
            parent_session_id: "parent".into(),
            root_session_id: Some("root".into()),
            ..Default::default()
        },
        plan: ResolvedChildPlan {
            model: "provider/child".into(),
            ..Default::default()
        },
    };
    let task_id = crate::state::create_task_id(None).expect("id").to_string();
    let (record, managed) = manager
        .allocate_workpool_record(&spec, &task_id, i64::from(std::process::id()))
        .expect("prepare");
    assert_eq!(calls.lock().expect("calls").as_slice(), ["parent"]);
    let trace = managed.traceparent.clone();
    let credential = barrier.resume("root", 7, "resume").expect("resume");
    assert!(barrier.release("root", &credential));
    store.save(&record).expect("save");
    manager.launch_prepared(record, managed).expect("launch");
    let specs = runner.specs();
    assert_eq!(specs.last().expect("runner spec").traceparent, trace);
    manager
        .cancel_task(&task_id, None, Default::default())
        .expect("cleanup");
}

#[tokio::test]
async fn spec_resume_credential_is_forwarded_and_admitted_scope_reaches_body() {
    use super::start_admission::{AdmissionHeld, StartAdmissions, StartOutcome};
    let barrier = AdmissionBarrier::new();
    barrier.close("parent", 4);
    let credential = barrier
        .resume("parent", 4, "resume")
        .expect("resume credential");
    let admissions = StartAdmissions::new(barrier.clone(), None, Arc::new(|| "child".into()));
    let spec = ManagerStartSpec {
        parent_session_id: "parent".into(),
        resume_credential: Some(credential),
        ..Default::default()
    };
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        admissions.run(&spec, None, None, |admitted, scope| async move {
            assert_eq!(
                admitted
                    .admission
                    .as_ref()
                    .expect("scope on spec")
                    .task_id(),
                scope.task_id()
            );
            scope.assert_open()?;
            Ok::<_, AdmissionHeld>(())
        }),
    )
    .await
    .expect("admission settles")
    .expect("admission body");
    assert!(matches!(outcome, StartOutcome::Done(())));
    assert!(barrier.in_flight("parent").is_empty());
}
