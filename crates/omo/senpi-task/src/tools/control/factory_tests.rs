//! `tools/control/factory.test.ts`

use std::sync::Arc;

use pretty_assertions::assert_eq;

use crate::tools::control::caller_session::default_resolve_caller_session_id;
use crate::tools::control::cancel::{TaskCancelDeps, create_task_cancel_tool, task_cancel_params_schema};
use crate::tools::control::send::{
    MemberScopedTaskSendDeps, TaskSendDeps, create_member_scoped_task_send_tool, create_task_send_tool,
};
use crate::tools::control::send_schema::{member_scoped_task_send_params_schema, task_send_params_schema};
use crate::tools::control::types::{
    CancelManager, CancelOutcome, ControlListScope, ControlSendInput, ControlTaskRecord, SendManager, SendOutcome,
    SessionIdCarrier, SessionIdSource,
};
use crate::tools::team::types::TeamToolsService;

// The shared team fakes live in `crate::tools::team::team_tool_fakes` (TS:
// ../team/__fixtures__/team-tool-fakes); reuse that module so its source is not compiled twice.
use crate::tools::team::team_tool_fakes::{FakeTeamServiceOverrides, create_fake_team_service};

struct UnusedSendManager;

impl SendManager for UnusedSendManager {
    fn send_to_task(&self, _input: &ControlSendInput) -> Result<SendOutcome, String> {
        Err("unused".to_string())
    }

    fn list(&self, _scope: &ControlListScope) -> Vec<ControlTaskRecord> {
        Vec::new()
    }
}

struct UnusedCancelManager;

impl CancelManager for UnusedCancelManager {
    fn cancel_task(&self, _id_or_name: &str, _reason: Option<&str>) -> CancelOutcome {
        CancelOutcome::NotFound {
            reason: "unused".to_string(),
        }
    }

    fn get_status(&self, _task_id: &str) -> Option<crate::state::TaskStatus> {
        None
    }
}

struct LiveSession;

impl SessionIdSource for LiveSession {
    fn get_session_id(&self) -> String {
        "session-live".to_string()
    }
}

struct Carrier {
    session: LiveSession,
}

impl SessionIdCarrier for Carrier {
    fn session_manager(&self) -> &dyn SessionIdSource {
        &self.session
    }
}

#[test]
fn given_the_control_factories_when_built_then_names_labels_and_typebox_params_are_wired() {
    // given / when
    let send_manager: Arc<dyn SendManager> = Arc::new(UnusedSendManager);
    let cancel_manager: Arc<dyn CancelManager> = Arc::new(UnusedCancelManager);
    let service: Arc<dyn TeamToolsService> =
        Arc::new(create_fake_team_service(FakeTeamServiceOverrides::default()));

    let send = create_task_send_tool(TaskSendDeps {
        manager: Arc::clone(&send_manager),
        team_routing: None,
        resolve_caller_session_id: None,
    });
    let cancel = create_task_cancel_tool(TaskCancelDeps {
        manager: cancel_manager,
    });
    let member_send = create_member_scoped_task_send_tool(MemberScopedTaskSendDeps {
        manager: send_manager,
        service,
        team_run_id: "run-1".to_string(),
        from: "alpha".to_string(),
        resolve_caller_session_id: None,
    });

    // then
    assert_eq!(send.name, "task_send");
    assert_eq!(send.parameters, task_send_params_schema());
    assert_eq!(member_send.name, "task_send");
    assert_eq!(member_send.parameters, member_scoped_task_send_params_schema());
    assert_eq!(cancel.name, "task_cancel");
    assert_eq!(cancel.parameters, task_cancel_params_schema());
    for (description, label) in [
        (send.description, send.label),
        (member_send.description, member_send.label),
        (cancel.description, cancel.label),
    ] {
        assert!(!description.is_empty());
        assert!(!label.is_empty());
    }
}

#[test]
fn given_the_default_resolver_when_a_session_carrier_is_passed_then_the_current_session_id_is_read() {
    // given
    let carrier = Carrier { session: LiveSession };

    // when
    let resolved = default_resolve_caller_session_id(&carrier);

    // then
    assert_eq!(resolved.as_deref(), Some("session-live"));
}

#[test]
fn given_no_resolver_override_when_send_tool_built_then_default_resolver_reads_carrier() {
    // given
    let send = create_task_send_tool(TaskSendDeps {
        manager: Arc::new(UnusedSendManager),
        team_routing: None,
        resolve_caller_session_id: None,
    });
    let carrier = Carrier { session: LiveSession };

    // when
    let resolved = (send.resolve_caller)(&carrier);

    // then
    assert_eq!(resolved.as_deref(), Some("session-live"));
}
