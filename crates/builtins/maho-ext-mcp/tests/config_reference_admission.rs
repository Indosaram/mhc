//! senpi#3858 configuration reference admission and refusal for MCP server declarations (census rows
//! senpi-mcp-01, 02, 03, 11, 12). Upstream covers these through 3858-reference-trust/-identities and the
//! config-schema / config-references / config-interpolation / sharing-policy / skill-server units.

use maho_core::config_reference_resolver::ConfigValue;
use maho_ext_mcp::{
    config::{hash_config, interpolate_value, legacy_hash_config, load_mcp_config, validate_raw},
    config_references::{mcp_server_references, plain_mcp_declaration, refuse_mcp_references},
    config_schema::LoadMcpConfigOptions,
    sharing_policy::shareable,
    skills::{SkillLike, parse_skill_mcp_declarations},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

fn declared(server: Value) -> maho_ext_mcp::config_schema::McpServerDeclaredConfig {
    validate_raw(json!({"mcpServers": {"fx": server}}))
        .expect("valid declaration")
        .mcp_servers
        .expect("servers")
        .remove("fx")
        .expect("fx")
}

/// senpi-mcp-01: `ConfigValueSchema` admits a reference object in `args[]`, `env.*` and `headers.*` only.
#[test]
fn schema_admits_references_only_in_args_env_and_headers() {
    let server = declared(json!({
        "command": "fixture",
        "args": ["plain", {"$env": "ARG"}],
        "env": {"TOKEN": {"$file": "token"}},
        "headers": {"Authorization": {"$concat": ["Bearer ", {"$env": "KEY"}]}},
    }));
    assert_eq!(
        server.references.keys().cloned().collect::<Vec<_>>(),
        ["args.1", "env.TOKEN", "headers.Authorization"]
    );
    assert!(server.references.values().all(ConfigValue::is_reference));
    assert_eq!(
        server.args.as_deref(),
        Some(&["plain".to_owned(), String::new()][..])
    );
    let restored = server.declared_json().expect("declared json");
    assert_eq!(restored["args"][1], json!({"$env": "ARG"}));
    assert_eq!(restored["env"]["TOKEN"], json!({"$file": "token"}));

    for (field, server) in [
        ("command", json!({"command": {"$env": "CMD"}})),
        ("url", json!({"type": "http", "url": {"$env": "URL"}})),
        (
            "cwd",
            json!({"command": "fixture", "cwd": {"$file": "dir"}}),
        ),
    ] {
        let error = validate_raw(json!({"mcpServers": {"fx": server}})).expect_err(field);
        assert_eq!(
            error.0,
            format!("Invalid MCP config at mcpServers.fx.{field}: must be string")
        );
    }
    for malformed in [
        json!({"$file": ""}),
        json!({"$env": "1BAD"}),
        json!({"$env": "A", "$file": "b"}),
    ] {
        let error = validate_raw(
            json!({"mcpServers": {"fx": {"command": "fixture", "args": [malformed]}}}),
        )
        .expect_err("a malformed reference is refused");
        assert_eq!(
            error.0,
            "Invalid MCP config at mcpServers.fx.args.0: must be string"
        );
    }
}

/// senpi-mcp-02: a trusted file remembers its own origin on each reference; a refusing source names the first
/// reference (headers, then env, then args) as `forbidden`.
#[test]
fn trusted_files_remember_origins_and_refusal_names_the_first_reference() {
    let agent = tempfile::tempdir().expect("agent dir");
    let project = tempfile::tempdir().expect("project");
    std::fs::write(
        agent.path().join("mcp.json"),
        json!({"mcpServers": {"global": {"command": "fixture", "env": {"TOKEN": {"$file": "global-token"}}}}})
            .to_string(),
    )
    .expect("global config");
    std::fs::create_dir(project.path().join(".omo")).expect(".omo");
    std::fs::write(
        project.path().join(".omo/mcp.json"),
        json!({"mcpServers": {"local": {"command": "fixture", "args": [{"$file": "local-token"}]}}}).to_string(),
    )
    .expect("project config");
    let env = BTreeMap::new();
    let config = load_mcp_config(LoadMcpConfigOptions {
        cwd: project.path(),
        agent_dir: agent.path(),
        env: &env,
        project_trusted: true,
        home_dir: Some(agent.path()),
        claude_config_dir: Some(agent.path()),
    })
    .expect("load");
    let origin = |server: &str, field: &str| match config.servers[server]
        .config
        .as_ref()
        .and_then(|config| config.references.get(field))
    {
        Some(ConfigValue::Reference {
            origin: Some(origin),
            ..
        }) => (origin.origin.clone(), origin.field.clone()),
        other => panic!("{server}.{field}: {other:?}"),
    };
    assert_eq!(
        origin("global", "env.TOKEN"),
        (
            agent.path().join("mcp.json").to_string_lossy().into_owned(),
            "mcpServers.global.env.TOKEN".into()
        )
    );
    assert_eq!(
        origin("local", "args.0"),
        (
            project
                .path()
                .join(".omo/mcp.json")
                .to_string_lossy()
                .into_owned(),
            "mcpServers.local.args.0".into()
        )
    );

    let server = declared(json!({
        "command": "fixture",
        "args": [{"$env": "ARG"}],
        "env": {"TOKEN": {"$env": "TOKEN"}},
        "headers": {"X-Key": {"$env": "KEY"}},
    }));
    let fields = mcp_server_references("fx", &server)
        .into_iter()
        .map(|found| found.field)
        .collect::<Vec<_>>();
    assert_eq!(
        fields,
        [
            "mcpServers.fx.headers.X-Key",
            "mcpServers.fx.env.TOKEN",
            "mcpServers.fx.args.0"
        ]
    );
    let refused = refuse_mcp_references("fx", &server).expect_err("refused");
    assert_eq!(
        refused.to_string(),
        "mcpServers.fx.headers.X-Key: configuration reference forbidden"
    );
    assert_eq!(refused.reference, json!({"$env": "KEY"}));
    assert!(plain_mcp_declaration("fx", &server).is_err());
    let plain = plain_mcp_declaration(
        "fx",
        &declared(json!({"command": "fixture", "args": ["a"]})),
    )
    .expect("a plain declaration passes");
    assert!(plain.references.is_empty());
}

/// senpi-mcp-03: legacy `${NAME}` interpolation treats a reference as one opaque leaf, including its literal
/// `$concat` parts, and the keyed identity covers the reference declaration.
#[test]
fn interpolation_keeps_references_opaque_and_identity_covers_them() {
    let env = BTreeMap::from([("HOME".to_owned(), "/home/fixture".to_owned())]);
    let reference = json!({"$concat": ["${HOME}/", {"$env": "TOKEN"}]});
    let raw = json!({"args": ["${HOME}", reference.clone()]});
    let interpolated = interpolate_value(&raw, "mcp", &env).expect("interpolates");
    assert_eq!(interpolated, json!({"args": ["/home/fixture", reference]}));

    let agent = tempfile::tempdir().expect("agent dir");
    let other = tempfile::tempdir().expect("other agent dir");
    let with_reference = declared(json!({"command": "fixture", "env": {"TOKEN": {"$env": "A"}}}));
    let other_reference = declared(json!({"command": "fixture", "env": {"TOKEN": {"$env": "B"}}}));
    let plain = declared(json!({"command": "fixture", "env": {"TOKEN": ""}}));
    let keyed = hash_config(&with_reference, Some(agent.path())).expect("hash");
    assert_ne!(
        keyed,
        hash_config(&other_reference, Some(agent.path())).expect("hash")
    );
    assert_ne!(
        keyed,
        hash_config(&plain, Some(agent.path())).expect("hash")
    );
    assert_ne!(
        keyed,
        hash_config(&with_reference, Some(other.path())).expect("hash"),
        "keyed per installation"
    );
    assert_ne!(
        keyed,
        legacy_hash_config(&with_reference).expect("legacy hash")
    );
    assert_eq!(
        legacy_hash_config(&with_reference).expect("legacy hash"),
        legacy_hash_config(&with_reference).expect("legacy hash")
    );
}

/// senpi-mcp-11: a reference slot is not a session string, so it never makes a stdio server session-bound.
/// A session-named env key still does.
#[test]
fn sharing_policy_ignores_reference_slots() {
    let cwd = "/work/session-3858";
    let with_reference = declared(json!({
        "command": "fixture",
        "args": [{"$concat": ["${cwd}", {"$env": "TOKEN"}]}],
        "env": {"TOKEN": {"$file": "/work/session-3858/token"}},
    }));
    assert!(shareable(&with_reference, Some(cwd)));
    let session_key =
        declared(json!({"command": "fixture", "env": {"SESSION_ID": {"$env": "TOKEN"}}}));
    assert!(!shareable(&session_key, Some(cwd)));
    let session_string = declared(json!({"command": "fixture", "args": ["${cwd}"]}));
    assert!(!shareable(&session_string, Some(cwd)));
}

/// senpi-mcp-12: a project skill server refuses a reference before any grant I/O.
#[tokio::test]
async fn project_skill_server_refuses_references_before_grant_io() {
    let skill = tempfile::tempdir().expect("skill dir");
    let agent = tempfile::tempdir().expect("agent dir");
    std::fs::write(
        skill.path().join("mcp.json"),
        json!({"probe": {"command": "fixture", "env": {"TOKEN": {"$file": skill.path().join("unread")}}}})
            .to_string(),
    )
    .expect("skill sidecar");
    let declarations = parse_skill_mcp_declarations(&[SkillLike {
        name: "fixture".into(),
        file_path: skill.path().join("SKILL.md"),
        base_dir: skill.path().into(),
        scope: Some("project".into()),
        ..Default::default()
    }]);
    let error = maho_ext_mcp::project_skill_policy::prepare_project_skill_server(
        "probe",
        &declarations.servers["probe"],
        skill.path(),
        agent.path(),
        true,
        &BTreeMap::new(),
        None,
    )
    .await
    .expect_err("refused");
    assert_eq!(
        error.message,
        "Skill 'fixture' MCP server 'probe' skipped: mcpServers.probe.env.TOKEN: configuration reference forbidden"
    );
    let store = maho_ext_mcp::env_grants_store::resolve_grants_store_path(Some(agent.path()));
    assert!(
        !Path::new(&store).exists(),
        "no grant store I/O before the refusal"
    );
    assert_eq!(
        std::fs::read_dir(agent.path()).expect("agent dir").count(),
        0
    );
}
