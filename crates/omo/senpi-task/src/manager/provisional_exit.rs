use super::outcome::{ErrorOutcomeInput, OutcomeTrackerPorts, owns_outcome};
use crate::state::{ProvisionalExit, TaskStatus};
use crate::store::PersistedTaskEvent;
use serde_json::json;

pub const UNEXPECTED_EXIT_CONFIRMATION_MS: i64 = 2_000;

pub fn confirm_exit(
    ports: &dyn OutcomeTrackerPorts,
    input: &ErrorOutcomeInput,
    generation: u64,
) -> bool {
    let Some(exit) = input
        .failure
        .exit
        .as_ref()
        .filter(|exit| exit.kind != "spawn_error")
    else {
        return true;
    };
    if input.handle.pid().is_none() {
        return true;
    }
    let Some(owned) = ports.try_load(&input.task_id) else {
        return false;
    };
    let observed_at = ports.exit_confirmation_now();
    let observed = ProvisionalExit {
        observed_at: crate::shared::iso_from_ms(observed_at),
        run_epoch: input.epoch,
        code: exit.code.map(i64::from),
        signal: exit.signal.clone(),
    };
    let mut applied = false;
    let saved = ports.store().mutate(&input.task_id, |fresh| {
        if fresh.status != TaskStatus::Running
            || fresh.notification.run_epoch != input.epoch
            || fresh.host_pid != owned.host_pid
            || fresh.residency_claim != owned.residency_claim
            || ports.tracking_generation(&input.task_id) != generation
            || !ports
                .live_handle(&input.task_id)
                .is_some_and(|live| std::sync::Arc::ptr_eq(&live, &input.handle))
        {
            return fresh.clone();
        }
        applied = true;
        let mut next = fresh.clone();
        next.provisional_exit = Some(observed.clone());
        next.updated_at.clone_from(&observed.observed_at);
        next
    });
    let persisted = match saved {
        Ok(_) if !applied => return false,
        Ok(_) => true,
        Err(error) => {
            utils::logger::log(
                "senpi-task provisional exit write failed",
                Some(&json!({"taskId":input.task_id,"error":error.to_string()})),
            );
            false
        }
    };
    if persisted
        && let Err(error) = ports.store().append_event(
            &input.task_id,
            &PersistedTaskEvent {
                event_type: "child_exit_provisional".into(),
                payload: json!(observed),
            },
        )
    {
        utils::logger::log(
            "senpi-task provisional exit event failed",
            Some(&json!({"taskId":input.task_id,"error":error.to_string()})),
        );
    }
    loop {
        if ports.tracking_generation(&input.task_id) != generation
            || !owns_outcome(ports, &input.task_id, &input.handle, input.epoch)
        {
            return false;
        }
        let mark = ports.exit_confirmation_mark(&input.task_id);
        let Some(fresh) = ports.try_load(&input.task_id) else {
            return false;
        };
        if fresh.host_pid != owned.host_pid
            || fresh.residency_claim != owned.residency_claim
            || (persisted
                && fresh
                    .provisional_exit
                    .as_ref()
                    .map(|exit| &exit.observed_at)
                    != Some(&observed.observed_at))
        {
            return false;
        }
        let remaining = observed_at
            .saturating_add(UNEXPECTED_EXIT_CONFIRMATION_MS)
            .saturating_sub(ports.exit_confirmation_now());
        let Some(remaining) = u64::try_from(remaining)
            .ok()
            .filter(|remaining| *remaining > 0)
        else {
            return true;
        };
        ports.wait_exit_confirmation(&input.task_id, mark, remaining);
    }
}
