use serde_json::Value;

use crate::internal::validate::{
    Field, Node, ObjectSpec, array, array_bounded, boolean, defaulted, enumeration, integer,
    literal, literal_number, lower_slug_string, non_empty_string, optional, record, required,
    strict_object, string, union,
};
use crate::issue::{Issues, custom};

fn default_true() -> Value {
    Value::Bool(true)
}

fn default_backend_type() -> Value {
    Value::String("in-process".to_string())
}

fn default_version() -> Value {
    Value::Number(1.into())
}

fn team_member_base_fields() -> Vec<Field> {
    vec![
        required("name", lower_slug_string()),
        optional("cwd", string()),
        optional("worktreePath", string()),
        optional("subscriptions", array(string())),
        defaulted(
            "backendType",
            enumeration(&["in-process", "tmux"]),
            default_backend_type,
        ),
        optional("color", string()),
        defaulted("isActive", boolean(), default_true),
    ]
}

fn with_extra_fields(base: Vec<Field>, extra: Vec<Field>) -> Vec<Field> {
    let mut fields = base;
    fields.extend(extra);
    fields
}

pub fn omo_team_category_member_schema() -> Node {
    strict_object(with_extra_fields(
        team_member_base_fields(),
        vec![
            required("kind", literal("category")),
            required("category", non_empty_string()),
            required("prompt", non_empty_string()),
        ],
    ))
}

pub fn omo_team_subagent_member_schema() -> Node {
    strict_object(with_extra_fields(
        team_member_base_fields(),
        vec![
            required("kind", literal("subagent_type")),
            required("subagent_type", non_empty_string()),
            optional("prompt", non_empty_string()),
        ],
    ))
}

pub fn omo_team_member_schema() -> Node {
    union(vec![
        omo_team_category_member_schema(),
        omo_team_subagent_member_schema(),
    ])
}

fn team_spec_refine(value: &Value, path: &[String], issues: &mut Issues) {
    let lead_agent_id_absent = value.get("leadAgentId").is_none();
    let member_count = value
        .get("members")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    if lead_agent_id_absent && member_count > 1 {
        let mut issue_path = path.to_vec();
        issue_path.push("leadAgentId".to_string());
        issues.push(custom(
            &issue_path,
            "leadAgentId required when a team has multiple members",
        ));
    }
}

pub fn omo_team_spec_base_spec() -> ObjectSpec {
    ObjectSpec {
        fields: vec![
            defaulted("version", literal_number(1), default_version),
            optional("name", lower_slug_string()),
            optional("description", string()),
            optional("createdAt", integer().with_min(1.0)),
            optional("leadAgentId", string()),
            optional("teamAllowedPaths", array(string())),
            optional("sessionPermission", string()),
            required(
                "members",
                array_bounded(omo_team_member_schema(), Some(1), Some(8)),
            ),
        ],
        strict: true,
        preprocess: None,
        refine: None,
    }
}

pub fn omo_team_spec_schema() -> Node {
    let mut spec = omo_team_spec_base_spec();
    spec.refine = Some(team_spec_refine);
    Node::Object(Box::new(spec))
}

pub fn omo_team_spec_layer_schema() -> Node {
    Node::Object(Box::new(omo_team_spec_base_spec().partial()))
}

pub fn omo_teams_config_schema() -> Node {
    record(omo_team_spec_schema())
}

pub fn omo_teams_config_layer_schema() -> Node {
    record(omo_team_spec_layer_schema())
}
