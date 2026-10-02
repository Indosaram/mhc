use std::{ops::Deref,sync::Arc};
use senpi_task::{store::{TaskRecordStore,StoreError},state::{TaskRecord,TaskTransition,TaskTransitionResult},completion::{ParentState,CompletionNotifier,CompletionRequest}};
pub type TerminalObserver=Arc<dyn Fn(&TaskRecord)+Send+Sync>;
pub struct CompletionBridgeDeps { pub notifier:Arc<CompletionNotifier>,pub parent_state:Arc<dyn Fn()->ParentState+Send+Sync>,pub was_background:Arc<dyn Fn(&str)->bool+Send+Sync>,pub on_terminal:Option<TerminalObserver> }
pub struct CompletionObservingStore { backing:TaskRecordStore,deps:CompletionBridgeDeps }
pub fn create_completion_observing_store(backing:TaskRecordStore,deps:CompletionBridgeDeps)->CompletionObservingStore { CompletionObservingStore { backing,deps } }
impl Deref for CompletionObservingStore { type Target=TaskRecordStore; fn deref(&self)->&Self::Target { &self.backing } }
impl CompletionObservingStore {
    pub fn transition(&self,id:&str,transition:&TaskTransition)->Result<TaskTransitionResult,StoreError> {
        let result=self.backing.transition(id,transition)?;
        if result.applied && matches!(transition,TaskTransition::Complete{..}|TaskTransition::Fail{..}|TaskTransition::Cancel{..}|TaskTransition::Interrupt{..}|TaskTransition::Lose{..}) && result.record.status.is_terminal() {
            self.deps.notifier.notify_terminal(&CompletionRequest { record:result.record.clone(),parent_state:(self.deps.parent_state)(),run_in_background:(self.deps.was_background)(id),tokens:None })?;
            if let Some(terminal)=&self.deps.on_terminal { terminal(&result.record); }
        }
        Ok(result)
    }
}
