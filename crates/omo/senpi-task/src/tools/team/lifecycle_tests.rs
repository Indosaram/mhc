//! `tools/team/lifecycle.test.ts`

use std::sync::Arc;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use team_core::types::MemberStatus;

use crate::state::{ResolvedModelRecord, ResolvedModelSource};
use crate::team::errors::{SenpiTeamSpecError, SenpiTeamSpecErrorCode};
use crate::team::runtime_types::{
    CreateTeamResult, CreatedMemberInfo, CreatedMemberRole, DeleteTeamResult, SenpiTeamRuntimeError,
    SenpiTeamRuntimeErrorCode,
};
use crate::tools::control::tool_result::{AgentToolResult, ToolResultContent};
use crate::tools::team::lifecycle::{
    TeamCreateDetails, TeamCreateInput, TeamDeleteDetails, TeamDeleteInput, create_team_create_tool,
    create_team_delete_tool, run_team_create, run_team_delete, runtime_error_to_service_error,
    spec_error_to_service_error, team_create_params_schema,
};
use crate::tools::team::team_tool_fakes::{
    FakeTeamService, FakeTeamServiceOverrides, create_fake_team_service, fake_create_result, fake_created_member,
    fake_delete_result,
};
use crate::tools::team::types::{CreateTeamToolInput, DeleteTeamToolInput, TeamToolDeps};

#[allow(irrefutable_let_patterns)]
fn text_of<T>(result: &AgentToolResult<T>) -> String {
    let Some(content) = result.content.first() else {
        return String::new();
    };
    let ToolResultContent::Text { text, .. } = content else {
        return String::new();
    };
    text.clone()
}

fn service_returning(result: CreateTeamResult) -> FakeTeamService {
    create_fake_team_service(FakeTeamServiceOverrides {
        create_team: Some(Box::new(move |_: &CreateTeamToolInput| Ok(result.clone()))),
        ..Default::default()
    })
}

fn delete_service_returning(result: DeleteTeamResult) -> FakeTeamService {
    create_fake_team_service(FakeTeamServiceOverrides {
        delete_team: Some(Box::new(move |_: &DeleteTeamToolInput| Ok(result.clone()))),
        ..Default::default()
    })
}

fn inline_input(spec: Value) -> TeamCreateInput {
    TeamCreateInput {
        team_name: None,
        inline_spec: Some(spec),
    }
}

fn expected_create_arg(spec: Value) -> Value {
    serde_json::to_value(CreateTeamToolInput {
        team_name: None,
        inline_spec: Some(spec),
    })
    .unwrap()
}

fn member_schema() -> Value {
    team_create_params_schema()["properties"]["inline_spec"]["anyOf"][0]["properties"]["members"]["anyOf"][0]
        ["items"]
        .clone()
}

// team_create tool

#[test]
fn schema_exposes_no_lead_session_id_override() {
    let schema = team_create_params_schema();
    let properties = schema["properties"].as_object().unwrap();
    assert!(!properties.contains_key("lead_session_id"));
}

#[test]
fn inline_spec_reports_created_run_and_members() {
    let service = service_returning(fake_create_result());

    let result = run_team_create(&service, &inline_input(json!({ "name": "demo", "members": [] }))).unwrap();

    assert_eq!(result.details.kind(), "created");
    let TeamCreateDetails::Created { team_name, members, .. } = &result.details else {
        panic!("expected created");
    };
    assert_eq!(team_name, "demo");
    let mut names: Vec<String> = members.iter().map(|member| member.name.clone()).collect();
    names.sort();
    assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
    let calls = service.calls();
    assert_eq!(calls[0].method, "createTeam");
    assert_eq!(calls[0].args, vec![expected_create_arg(json!({ "name": "demo", "members": [] }))]);
}

#[test]
fn text_lists_every_member_and_keeps_first_line_stable() {
    let alpha_model = ResolvedModelRecord {
        display: "Claude Opus 4.7".to_string(),
        reasoning_effort: Some("high".to_string()),
        ..ResolvedModelRecord::new(ResolvedModelSource::Category, "anthropic", "claude-opus-4-7")
    };
    let service = service_returning(CreateTeamResult {
        members: vec![
            CreatedMemberInfo {
                name: "alpha".to_string(),
                status: MemberStatus::Running,
                role: CreatedMemberRole::Category {
                    category: "deep".to_string(),
                },
                model: Some(alpha_model),
                prompt_excerpt: Some("Refactor the auth module".to_string()),
                ..fake_created_member()
            },
            CreatedMemberInfo {
                name: "beta".to_string(),
                status: MemberStatus::Idle,
                task_id: "st_b".to_string(),
                role: CreatedMemberRole::SubagentType {
                    subagent_type: "sisyphus".to_string(),
                },
                ..fake_created_member()
            },
        ],
        ..fake_create_result()
    });

    let result = run_team_create(&service, &inline_input(json!({ "name": "demo", "members": [] }))).unwrap();

    let text = text_of(&result);
    let first_line = text.split('\n').next().unwrap_or_default();
    assert_eq!(
        first_line,
        "Created team 'demo' (00000000-0000-4000-8000-000000000000) with 2 members."
    );
    assert!(text.contains("- alpha [running] category:deep(anthropic/claude-opus-4-7:high) task:st_a"));
    assert!(!text.contains("Refactor the auth module"));
    assert!(text.contains("- beta [idle] agent:sisyphus task:st_b"));
    assert!(!text.contains("beta [idle] agent:sisyphus("));
    let TeamCreateDetails::Created { members, .. } = &result.details else {
        panic!("expected created");
    };
    assert_eq!(members[0].name, "alpha");
    assert_eq!(members[0].status, "running");
    assert_eq!(members[0].role, "category:deep");
    assert_eq!(members[0].task_id, "st_a");
    assert_eq!(members[0].prompt_excerpt.as_deref(), Some("Refactor the auth module"));
    assert_eq!(members[1].name, "beta");
    assert_eq!(members[1].role, "agent:sisyphus");
    assert_eq!(members[1].task_id, "st_b");
}

#[test]
fn member_schema_task_summary_follows_prompt_with_length_limit() {
    let schema = member_schema();
    let keys: Vec<&String> = schema["properties"].as_object().unwrap().keys().collect();
    let prompt_index = keys.iter().position(|key| key.as_str() == "prompt").unwrap();
    let summary_index = keys.iter().position(|key| key.as_str() == "task_summary").unwrap();

    assert_eq!(summary_index, prompt_index + 1);
    assert_eq!(schema["properties"]["task_summary"]["maxLength"], json!(80));
}

#[test]
fn member_view_carries_task_summary() {
    let service = service_returning(CreateTeamResult {
        members: vec![CreatedMemberInfo {
            name: "alpha".to_string(),
            status: MemberStatus::Running,
            role: CreatedMemberRole::Category {
                category: "deep".to_string(),
            },
            task_summary: Some("Refactor the auth module boundary".to_string()),
            ..fake_created_member()
        }],
        ..fake_create_result()
    });

    let result = run_team_create(&service, &inline_input(json!({ "name": "demo", "members": [] }))).unwrap();

    let TeamCreateDetails::Created { members, .. } = &result.details else {
        panic!("expected created");
    };
    assert_eq!(members[0].name, "alpha");
    assert_eq!(members[0].task_summary.as_deref(), Some("Refactor the auth module boundary"));
}

#[test]
fn reasoning_is_labeled_and_reasoning_effort_wins_over_variant() {
    let alpha_model = ResolvedModelRecord {
        display: "Claude Opus 4.7".to_string(),
        reasoning_effort: Some("high".to_string()),
        variant: Some("xhigh".to_string()),
        ..ResolvedModelRecord::new(ResolvedModelSource::Category, "anthropic", "claude-opus-4-7")
    };
    let beta_model = ResolvedModelRecord {
        display: "gpt-5.6-luna-fast".to_string(),
        variant: Some("max".to_string()),
        ..ResolvedModelRecord::new(ResolvedModelSource::Category, "openai", "gpt-5.6-luna-fast")
    };
    let service = service_returning(CreateTeamResult {
        members: vec![
            CreatedMemberInfo {
                name: "alpha".to_string(),
                status: MemberStatus::Running,
                role: CreatedMemberRole::Category {
                    category: "deep".to_string(),
                },
                model: Some(alpha_model),
                ..fake_created_member()
            },
            CreatedMemberInfo {
                name: "beta".to_string(),
                status: MemberStatus::Running,
                role: CreatedMemberRole::Category {
                    category: "quick".to_string(),
                },
                model: Some(beta_model),
                ..fake_created_member()
            },
        ],
        ..fake_create_result()
    });

    let result = run_team_create(&service, &inline_input(json!({ "name": "demo", "members": [] }))).unwrap();

    let text = text_of(&result);
    assert!(text.contains("category:deep(anthropic/claude-opus-4-7:high)"));
    assert!(text.contains("category:quick(openai/gpt-5.6-luna-fast:max)"));
    assert!(!text.contains("variant:"));
    assert!(!text.contains("undefined"));
}

#[test]
fn neither_team_name_nor_inline_spec_rejects_with_invalid_arguments() {
    let service = create_fake_team_service(FakeTeamServiceOverrides::default());
    let result = run_team_create(&service, &TeamCreateInput::default()).unwrap();
    assert_eq!(result.details.kind(), "invalid_arguments");
    assert!(service.calls().is_empty());
}

#[test]
fn spec_error_surfaces_spec_error_with_code() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        create_team: Some(Box::new(|_: &CreateTeamToolInput| {
            Err(spec_error_to_service_error(&SenpiTeamSpecError::new(
                "bad member",
                SenpiTeamSpecErrorCode::UnknownSubagentType,
                "demo",
            )))
        })),
        ..Default::default()
    });

    let result = run_team_create(
        &service,
        &TeamCreateInput {
            team_name: Some("demo".to_string()),
            inline_spec: None,
        },
    )
    .unwrap();

    let TeamCreateDetails::SpecError { code, .. } = &result.details else {
        panic!("expected spec_error");
    };
    assert_eq!(code, "UNKNOWN_SUBAGENT_TYPE");
}

#[test]
fn bounds_runtime_error_surfaces_runtime_error_with_code() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        create_team: Some(Box::new(|_: &CreateTeamToolInput| {
            Err(runtime_error_to_service_error(&SenpiTeamRuntimeError::new(
                "too many",
                SenpiTeamRuntimeErrorCode::BoundsExceeded,
                "demo",
            )))
        })),
        ..Default::default()
    });

    let result = run_team_create(
        &service,
        &TeamCreateInput {
            team_name: Some("demo".to_string()),
            inline_spec: None,
        },
    )
    .unwrap();

    let TeamCreateDetails::RuntimeError { code, .. } = &result.details else {
        panic!("expected runtime_error");
    };
    assert_eq!(code, "bounds_exceeded");
}

#[test]
fn create_factory_names_the_tool_team_create() {
    let deps = TeamToolDeps {
        service: Arc::new(create_fake_team_service(FakeTeamServiceOverrides::default())),
    };
    let tool = create_team_create_tool(&deps);
    assert_eq!(tool.name, "team_create");
}

// team_delete tool

#[test]
fn delete_reports_deleted_run_and_cancelled_tasks() {
    let service = delete_service_returning(fake_delete_result());

    let result = run_team_delete(
        &service,
        &TeamDeleteInput {
            team_run_id: "run-1".to_string(),
            force: None,
        },
    )
    .unwrap();

    let TeamDeleteDetails::Deleted { cancelled_task_ids, .. } = &result.details else {
        panic!("expected deleted");
    };
    assert_eq!(cancelled_task_ids, &vec!["st_a".to_string()]);
    let calls = service.calls();
    assert_eq!(calls[0].method, "deleteTeam");
    let expected = serde_json::to_value(DeleteTeamToolInput {
        team_run_id: "run-1".to_string(),
        force: None,
    })
    .unwrap();
    assert_eq!(calls[0].args, vec![expected]);
}

#[test]
fn delete_text_names_cancelled_task_ids() {
    let service = delete_service_returning(DeleteTeamResult {
        cancelled_task_ids: vec!["st_a".to_string(), "st_b".to_string()],
        ..fake_delete_result()
    });

    let result = run_team_delete(
        &service,
        &TeamDeleteInput {
            team_run_id: "run-1".to_string(),
            force: None,
        },
    )
    .unwrap();

    let text = text_of(&result);
    assert!(text.contains("Deleted team"));
    assert!(text.contains("st_a"));
    assert!(text.contains("st_b"));
}

#[test]
fn delete_forwards_force_true() {
    let service = delete_service_returning(fake_delete_result());
    run_team_delete(
        &service,
        &TeamDeleteInput {
            team_run_id: "run-1".to_string(),
            force: Some(true),
        },
    )
    .unwrap();
    let expected = serde_json::to_value(DeleteTeamToolInput {
        team_run_id: "run-1".to_string(),
        force: Some(true),
    })
    .unwrap();
    assert_eq!(service.calls()[0].args[0], expected);
}

#[test]
fn illegal_delete_state_surfaces_invalid_state() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        delete_team: Some(Box::new(|_: &DeleteTeamToolInput| {
            Err(runtime_error_to_service_error(&SenpiTeamRuntimeError::new(
                "cannot delete",
                SenpiTeamRuntimeErrorCode::InvalidDeleteState,
                "run-1",
            )))
        })),
        ..Default::default()
    });

    let result = run_team_delete(
        &service,
        &TeamDeleteInput {
            team_run_id: "run-1".to_string(),
            force: None,
        },
    )
    .unwrap();

    assert_eq!(result.details.kind(), "invalid_state");
    let TeamDeleteDetails::InvalidState { team_run_id, .. } = &result.details else {
        panic!("expected invalid_state");
    };
    assert_eq!(team_run_id, "run-1");
}

#[test]
fn delete_factory_names_the_tool_team_delete() {
    let deps = TeamToolDeps {
        service: Arc::new(create_fake_team_service(FakeTeamServiceOverrides::default())),
    };
    let tool = create_team_delete_tool(&deps);
    assert_eq!(tool.name, "team_delete");
}

// team_create inline_spec schema shape

#[test]
fn inline_spec_schema_exposes_object_shape_with_members() {
    let mut structural = team_create_params_schema()["properties"]["inline_spec"].clone();
    structural.as_object_mut().unwrap().remove("description");
    let serialized = structural.to_string();

    assert_ne!(serialized, "{}");
    assert!(serialized.contains("members"));
}

#[test]
fn json_stringified_inline_spec_reaches_service_parsed() {
    let service = service_returning(fake_create_result());
    let spec = json!({ "name": "demo", "members": [{ "name": "alpha", "kind": "category", "category": "deep" }] });
    let payload = spec.to_string();

    let result = run_team_create(&service, &inline_input(Value::String(payload))).unwrap();

    let TeamCreateDetails::Created { team_name, .. } = &result.details else {
        panic!("expected created");
    };
    assert_eq!(team_name, "demo");
    let calls = service.calls();
    assert_eq!(calls[0].method, "createTeam");
    assert_eq!(calls[0].args, vec![expected_create_arg(spec)]);
}

#[test]
fn schema_accepts_single_member_object_inline_spec() {
    // Structural stand-in for `Value.Check`: the members anyOf must admit a bare member object.
    let schema = team_create_params_schema();
    let members = &schema["properties"]["inline_spec"]["anyOf"][0]["properties"]["members"];
    let variants = members["anyOf"].as_array().unwrap();
    let object_variant = variants
        .iter()
        .find(|variant| variant["type"] == json!("object"))
        .expect("object member variant");
    let properties = object_variant["properties"].as_object().unwrap();
    for key in ["name", "kind", "category"] {
        assert!(properties.contains_key(key), "missing {key}");
    }
    assert_eq!(object_variant["additionalProperties"], json!(true));
    let kinds: Vec<Value> = properties["kind"]["anyOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["const"].clone())
        .collect();
    assert!(kinds.contains(&json!("category")));
}

#[test]
fn single_object_members_inline_spec_reaches_service() {
    let service = service_returning(fake_create_result());

    let result = run_team_create(
        &service,
        &inline_input(json!({ "name": "demo", "members": { "name": "alpha", "kind": "category", "category": "deep" } })),
    )
    .unwrap();

    assert_eq!(result.details.kind(), "created");
    assert_eq!(service.calls()[0].method, "createTeam");
}

#[test]
fn malformed_json_string_inline_spec_rejects_without_calling_service() {
    let service = create_fake_team_service(FakeTeamServiceOverrides::default());

    let result = run_team_create(&service, &inline_input(Value::String("{not json".to_string()))).unwrap();

    let text = text_of(&result);
    assert!(text.contains("inline_spec"));
    assert!(text.contains("JSON"));
    assert_eq!(result.details.kind(), "invalid_arguments");
    assert_eq!(service.calls().len(), 0);
}
