use std::sync::Arc;
use maho_omo_task::lifecycle_adapters::{TaskLifecycleDestruction,admit_lifecycle};
use senpi_task::{lifecycle::{ResidencyRegistry,ResidentHandle,TaskSettings,LifecycleDeps,create_task_lifecycle,DestroyCause},store::{StateDirConfig,TaskRecordStore},state::{TaskRecordInput,create_task_record,ResidencyState},manager::types::SpawnAdmission};
struct Registry;
impl ResidencyRegistry for Registry { fn get(&self,_:&str)->Option<Arc<dyn ResidentHandle>> { None } fn entries(&self)->Vec<Arc<dyn ResidentHandle>> { vec![] } fn forget(&self,_:&str) {} fn has_pending_sends(&self,_:&str)->bool { false } }
#[test] fn lifecycle_ports_preserve_admission_and_single_writer_residency() {
    let root=tempfile::tempdir().expect("root"); let store=TaskRecordStore::new(&StateDirConfig { project_dir:root.path().into(),task_state_dir:None }); let record=create_task_record(TaskRecordInput { parent_session_id:"parent".into(),..Default::default() },Some(1)).expect("record"); store.save(&record).expect("save");
    let config=TaskSettings::from_resolved(&serde_json::json!({"residency_max_children":1})); let lifecycle=create_task_lifecycle(LifecycleDeps::new(Arc::new(store.clone()),Arc::new(Registry),config));
    assert!(matches!(admit_lifecycle(&lifecycle,"foreign").expect("admit"),SpawnAdmission::Admitted)); assert!(matches!(admit_lifecycle(&lifecycle,"parent").expect("full"),SpawnAdmission::Rejected { .. }));
    let adapter=TaskLifecycleDestruction(lifecycle);
    senpi_task::steering::DestructionPort::destroy_resident_task(&adapter,&record.task_id,DestroyCause::Cancel).expect("destroy"); assert_eq!(store.load(&record.task_id).expect("load").expect("record").residency_state,ResidencyState::Disposed);
    senpi_task::team::runtime_types::TeamMemberDestructionPort::destroy_resident_task(&adapter,"st_00000000",DestroyCause::Cancel).expect("missing noop"); assert!(matches!(admit_lifecycle(&adapter.0,"parent").expect("freed"),SpawnAdmission::Admitted));
    let mut terminal=record.clone(); terminal.status=senpi_task::state::TaskStatus::Completed; terminal.residency_state=ResidencyState::Resident; store.replace(&terminal).expect("terminal resident");
    assert_eq!(admit_lifecycle(&adapter.0,"parent").expect("evict"),SpawnAdmission::Evicted { evicted_task_id:record.task_id.clone() }); assert_eq!(store.load(&record.task_id).expect("load").expect("record").residency_state,ResidencyState::Evicted);
}
