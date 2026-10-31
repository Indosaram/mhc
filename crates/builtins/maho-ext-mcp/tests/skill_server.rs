//! Port of the skill-server.ts cases in senpi test/mcp/skills-sidecar.test.ts ("skill-declared variable expansion").
use maho_ext_mcp::{
    config_schema::{Auth, ResolvedMcpServer},
    skill_server::{SkillServerSource, resolve_skill_mcp_server},
};
use serde_json::json;
use std::{collections::BTreeMap, path::Path};

const TOOLS_EXPR: &str = "${SENPI_2345_TOOLS}";
const SECRET_EXPR: &str = "${SENPI_2345_SECRET}";
const UNSET_EXPR: &str = "${SENPI_2345_UNSET:-fallback}";

fn resolve(
    name: &str,
    raw: serde_json::Value,
    skill: &str,
    trusted: bool,
    env: &BTreeMap<String, String>,
) -> (Option<ResolvedMcpServer>, Option<String>) {
    let agent = tempfile::tempdir().expect("agent dir");
    let result = resolve_skill_mcp_server(
        name,
        &raw,
        Path::new("s"),
        &SkillServerSource {
            skill_name: skill,
            trusted,
            env,
        },
        Some(agent.path()),
    )
    .expect("skill server resolves");
    (result.server, result.warning)
}

#[test]
fn expands_stdio_values_for_a_trusted_skill() {
    let env = BTreeMap::from([("SENPI_2345_TOOLS".to_owned(), "3".to_owned())]);
    let (server, warning) = resolve(
        "exa",
        json!({"command":"node","env":{"EXA_API_KEY":TOOLS_EXPR,"UNSET":UNSET_EXPR}}),
        "user-skill",
        true,
        &env,
    );
    assert_eq!(warning, None);
    let config = server.expect("server").config.expect("config");
    assert_eq!(
        config.env,
        Some(BTreeMap::from([
            ("EXA_API_KEY".to_owned(), "3".to_owned()),
            ("UNSET".to_owned(), "fallback".to_owned())
        ]))
    );
}

#[test]
fn keeps_an_untrusted_skill_literal_and_warns_with_the_variable() {
    let env = BTreeMap::from([("SENPI_2345_SECRET".to_owned(), "parent-secret".to_owned())]);
    let (server, warning) = resolve(
        "fxc",
        json!({"command":"node","env":{"K":SECRET_EXPR}}),
        "cloned-skill",
        false,
        &env,
    );
    let config = server.expect("server").config.expect("config");
    assert_eq!(
        config.env,
        Some(BTreeMap::from([("K".to_owned(), SECRET_EXPR.to_owned())]))
    );
    let warning = warning.expect("literal warning");
    assert!(warning.contains("'cloned-skill'"));
    assert!(warning.contains("SENPI_2345_SECRET"));
    assert!(warning.contains("trust the project"));
    assert!(warning.contains("mcp.json"));
}

#[test]
fn keeps_remote_url_and_headers_literal_for_any_skill_and_warns() {
    let env = BTreeMap::from([("SENPI_2345_SECRET".to_owned(), "parent-secret".to_owned())]);
    let (server, warning) = resolve(
        "remote",
        json!({"url":format!("https://mcp.example.test/{SECRET_EXPR}"),"headers":{"X-API-Key":SECRET_EXPR}}),
        "installed-skill",
        true,
        &env,
    );
    let config = server.expect("server").config.expect("config");
    assert_eq!(
        config.url.as_deref(),
        Some(format!("https://mcp.example.test/{SECRET_EXPR}").as_str())
    );
    assert_eq!(
        config.headers,
        Some(BTreeMap::from([(
            "X-API-Key".to_owned(),
            SECRET_EXPR.to_owned()
        )]))
    );
    let warning = warning.expect("literal warning");
    assert!(warning.contains("'installed-skill'"));
    assert!(warning.contains("SENPI_2345_SECRET"));
    assert!(warning.contains("url/headers"));
}

#[test]
fn never_sends_a_skill_remote_servers_bearer_token_env() {
    let env = BTreeMap::from([("SENPI_2345_TOKEN".to_owned(), "skill-token".to_owned())]);
    let (server, warning) = resolve(
        "fxr",
        json!({"bearerTokenEnv":"SENPI_2345_TOKEN","url":"http://127.0.0.1:1/mcp"}),
        "remote-skill",
        true,
        &env,
    );
    let config = server.expect("server").config.expect("config");
    // The variable is dropped and auth disabled, so the transport attaches no Authorization header.
    assert_eq!(config.bearer_token_env, None);
    assert_eq!(config.auth, Some(Auth::Disabled(false)));
    let spec = maho_ext_mcp::transport::create_mcp_transport_spec("fxr", &config, Some(&env))
        .expect("transport spec");
    let maho_ext_mcp::transport_sdk::McpTransportSpec::Http { headers, .. } = spec else {
        panic!("remote skill server is an HTTP transport");
    };
    assert!(
        !headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case("authorization"))
    );
    let warning = warning.expect("bearer warning");
    assert!(warning.contains("bearerTokenEnv 'SENPI_2345_TOKEN' is ignored"));
    assert!(warning.contains("'remote-skill'"));
    assert!(warning.contains("your own mcp.json"));
}

#[test]
fn skips_a_trusted_skills_server_that_asks_for_command_substitution() {
    let (server, warning) = resolve(
        "sub",
        json!({"command":"node","args":["$(cat ~/.ssh/id_rsa)"]}),
        "user-skill",
        true,
        &BTreeMap::new(),
    );
    assert!(server.is_none());
    assert!(warning.expect("refusal").contains("command substitution"));
}

#[test]
fn refuses_a_reference_where_variables_stay_literal() {
    // skill-server.ts referenceRefusal: remote and untrusted declarations never resolve a reference.
    let reference = json!({"$env":"SENPI_2345_SECRET"});
    for (raw, trusted) in [
        (
            json!({"url":"https://mcp.example.test/mcp","headers":{"X-API-Key":reference}}),
            true,
        ),
        (json!({"command":"node","env":{"K":reference}}), false),
    ] {
        let (server, warning) = resolve("ref", raw, "skill", trusted, &BTreeMap::new());
        assert!(server.is_none());
        assert!(
            warning
                .expect("refusal")
                .starts_with("Skill 'skill' MCP server 'ref' skipped: ")
        );
    }
}
