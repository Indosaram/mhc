//! `tools/team/lifecycle-precedence.test.ts`

use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

use crate::tools::team::lifecycle::{TeamCreateInput, run_team_create};
use crate::tools::team::team_tool_fakes::{FakeTeamServiceOverrides, create_fake_team_service, fake_create_result};

/// Drops `null` entries so optional fields that serialize as `null` compare like absent TS keys.
fn strip_nulls(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, entry) in map {
                if !entry.is_null() {
                    out.insert(key.clone(), entry.clone());
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

#[test]
fn given_both_team_name_and_inline_spec_when_team_create_runs_then_inline_spec_is_authoritative() {
    // given
    let inline_spec = json!({ "name": "inline-team", "members": [] });
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        create_team: Some(Box::new(|_input| Ok(fake_create_result()))),
        ..Default::default()
    });

    // when
    let result = run_team_create(
        &service,
        &TeamCreateInput {
            team_name: Some("stale-named-team".to_string()),
            inline_spec: Some(inline_spec.clone()),
        },
    )
    .expect("team_create should succeed");

    // then
    assert_eq!(result.details.kind(), "created");
    let calls = service.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].method, "createTeam");
    assert_eq!(calls[0].args.len(), 1);
    assert_eq!(strip_nulls(&calls[0].args[0]), json!({ "inlineSpec": inline_spec }));
}
