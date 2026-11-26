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

#[test]
fn skill_timeout_is_numeric_bounded_and_secondary_to_native_policy() {
    for (value, expected, effective_ms) in [
        (json!(1e-20), Some(1e-20), 1.0),
        (json!(9.75), Some(9.75), 9.0),
        (
            json!(i32::MAX),
            Some(f64::from(i32::MAX)),
            f64::from(i32::MAX),
        ),
        (json!(2147483648_u64), None, 30000.0),
        (json!(-1), None, 30000.0),
        (json!(0), None, 30000.0),
        (json!(true), None, 30000.0),
        (json!("$(invalid)"), None, 30000.0),
        (json!({"bad":1}), None, 30000.0),
    ] {
        let raw = json!({"command":"fixture","timeout":value});
        assert_eq!(
            maho_ext_mcp::skill_server::skill_request_timeout_ms(&raw),
            expected
        );
        let (resolved, _) = resolve("fixture", raw, "skill", true, &BTreeMap::new());
        assert_eq!(
            resolved.unwrap().config.unwrap().request_timeout_ms,
            Some(effective_ms)
        );
    }
    let (resolved, _) = resolve(
        "fixture",
        json!({"command":"fixture","timeout":2,"requestTimeoutMs":0.25}),
        "skill",
        true,
        &BTreeMap::new(),
    );
    assert_eq!(
        resolved.unwrap().config.unwrap().request_timeout_ms,
        Some(0.25)
    );
}

#[test]
fn pooled_catalog_deadline_is_scoped_to_explicit_skill_policy() {
    use maho_ext_mcp::{config::normalize_server, sharing_policy::shared_mcp_key};
    let url = "http://127.0.0.1:1/mcp";
    let key = |config: &_| shared_mcp_key("fixture", config, None, None, "agent", "cwd");
    let dir = tempfile::tempdir().unwrap();
    let env = BTreeMap::new();
    let skill = |raw| {
        resolve_skill_mcp_server(
            "fixture",
            &raw,
            dir.path(),
            &SkillServerSource {
                skill_name: "skill",
                trusted: true,
                env: &env,
            },
            Some(dir.path()),
        )
        .unwrap()
        .server
        .unwrap()
    };
    let implicit = skill(json!({"url":url}));
    let explicit_default = skill(json!({"url":url,"timeout":30000}));
    assert_ne!(implicit.config_hash, explicit_default.config_hash);
    let skill_key = |raw| key(skill(raw).config.as_ref().unwrap());
    let native_key = |raw| key(&normalize_server(serde_json::from_value(raw).unwrap()));
    let base = native_key(json!({"url":url}));
    assert_eq!(base, native_key(json!({"url":url,"requestTimeoutMs":123})));
    assert_eq!(base, skill_key(json!({"url":url,"timeout":0})));
    let short = skill_key(json!({"url":url,"timeout":25}));
    assert_ne!(base, short);
    assert_ne!(short, skill_key(json!({"url":url,"timeout":50})));
    assert_eq!(short, skill_key(json!({"url":url,"timeout":25.9})));
    assert_eq!(
        short,
        skill_key(json!({"url":url,"timeout":50,"requestTimeoutMs":25}))
    );
}

#[test]
fn only_an_explicit_skill_sse_declaration_has_a_distinct_transport_identity() {
    use maho_ext_mcp::{
        config::normalize_server, config_schema::ServerConfigWire, sharing_policy::shared_mcp_key,
    };
    let dir = tempfile::tempdir().unwrap();
    let url = "http://127.0.0.1:1/mcp";
    let env = BTreeMap::new();
    let source = SkillServerSource {
        skill_name: "skill",
        trusted: true,
        env: &env,
    };
    let resolve =
        |raw| resolve_skill_mcp_server("server", &raw, dir.path(), &source, Some(dir.path()));
    let implicit = resolve(json!({"url":url})).unwrap().server.unwrap();
    let http = resolve(json!({"type":"http","url":url}))
        .unwrap()
        .server
        .unwrap();
    let sse = resolve(json!({"type":"sse","url":url}))
        .unwrap()
        .server
        .unwrap();
    assert_eq!(http.config_hash, implicit.config_hash);
    assert_ne!(http.config_hash, sse.config_hash);
    let key = |config| shared_mcp_key("server", config, None, None, "agent", "cwd");
    let native = normalize_server(serde_json::from_value(json!({"type":"sse","url":url})).unwrap());
    assert_eq!(key(&native), key(http.config.as_ref().unwrap()));
    assert_ne!(key(&native), key(sse.config.as_ref().unwrap()));
    assert!(serde_json::from_value::<ServerConfigWire>(json!({"url":url,"timeout":10})).is_err());
    assert!(resolve(json!({"url":url,"timeout":10,"skillSse":true})).is_err());
    assert!(resolve(json!({"url":url,"timeout":10,"requestTimeoutMs":"bad"})).is_err());
}

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
