//! Ports of senpi packages/ai/test/provider-error-redaction.test.ts and
//! provider-error-redaction-scaling.test.ts (redactor and sanitizer cases; the capture cases are
//! in assistant_capture_tests.rs). Fixture secrets are assembled at runtime and low-entropy so
//! secret scanners never read them as real keys.

use super::*;
use crate::utils::secret_lexer::READS;
use serde_json::json;

fn secret() -> String {
    ["zzfixture", "4283", "valueQ"].concat()
}
fn ghp() -> String {
    format!("ghp_{}0008", "FIXTURE".repeat(3))
}
fn sk() -> String {
    "sk-projFIXTURE0006".to_owned()
}
fn jwt() -> String {
    [
        "eyJhbGciOiJub25lIn0",
        "eyJzdWIiOiJmaXh0dXJlIn0",
        "Zml4dHVyZS1zaWcx",
    ]
    .join(".")
}
fn json_text(value: serde_json::Value) -> String {
    value.to_string()
}
fn encode_uri_component(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

type Redact = fn(&str) -> String;
const MODES: [(&str, Redact); 2] = [
    ("full", redact_secrets),
    ("shapes", redact_credential_shapes),
];

struct Shape {
    label: String,
    value: String,
    marker: String,
}

fn suffix_shapes() -> Vec<Shape> {
    let mut rules: Vec<(&str, usize)> = vec![("sk-", 12)];
    for prefix in [
        "sk_live_", "sk_test_", "rk_live_", "rk_test_", "gsk_", "xai-", "hf_",
    ] {
        rules.push((prefix, 24));
    }
    for prefix in ["xoxa-", "xoxb-", "xoxo-", "xoxp-", "xoxr-", "xoxs-"] {
        rules.push((prefix, 24));
    }
    for prefix in ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"] {
        rules.push((prefix, 24));
    }
    rules.push(("AKIA", 16));
    rules.push(("ASIA", 16));
    rules.push(("AIza", 32));
    let mut shapes: Vec<Shape> = rules
        .into_iter()
        .map(|(prefix, length)| Shape {
            label: prefix.to_owned(),
            value: format!("{prefix}{}", "F".repeat(length)),
            marker: "F".repeat(12),
        })
        .collect();
    shapes.push(Shape {
        label: "JWT".into(),
        value: jwt(),
        marker: "eyJzdWIiOiJmaXh0dXJlIn0".into(),
    });
    shapes
}

#[test]
fn credential_suffix_boundaries() {
    let seconds: Vec<(String, String)> = ["ghp_", "sk-proj-", "xoxb-"]
        .iter()
        .map(|prefix| (format!("{prefix}{}", "G".repeat(36)), "G".repeat(12)))
        .collect();
    let separators = ["/", ".", ":", "?", "&", "=", "_", "-", "", "z"];
    let templates = [
        ("rejected ", " while loading configuration"),
        ("오류: ", " 구성 실패"),
        ("/repo/", " next"),
        ("https://example.test/", "?model=faux-1&stream=true"),
        ("https://example.test/?value=", "&model=faux-1"),
        ("https://", "@example.test/repo"),
        ("https://user:", "@example.test/repo"),
    ];
    let mut cases = 0;
    for (mode, redact) in MODES {
        for shape in suffix_shapes() {
            for separator in separators {
                for second in std::iter::once(None).chain(seconds.iter().map(Some)) {
                    for (before, after) in templates {
                        let second_value = second.map_or("", |(value, _)| value.as_str());
                        let text =
                            format!("{before}{}{separator}{second_value}{after}", shape.value);
                        let result = redact(&text);
                        let label = format!("{mode} {} {separator:?} {text}", shape.label);
                        assert!(!result.contains(&shape.value), "{label}: {result}");
                        assert!(!result.contains(&shape.marker), "{label}: {result}");
                        if let Some((value, marker)) = second {
                            assert!(!result.contains(value.as_str()), "{label}: {result}");
                            assert!(!result.contains(marker.as_str()), "{label}: {result}");
                        }
                        assert!(result.starts_with(before), "{label}: {result}");
                        assert!(result.ends_with(after), "{label}: {result}");
                        cases += 1;
                    }
                }
            }
        }
    }
    assert!(cases > 0);
}

#[test]
fn preserves_suffixes_after_a_single_known_key() {
    for (mode, redact) in MODES {
        for shape in suffix_shapes()
            .into_iter()
            .filter(|shape| shape.label != "JWT")
        {
            for suffix in ["/hooks.json", ".json", ":42", ""] {
                let result = redact(&format!("/repo/{}{suffix}", shape.value));
                assert_eq!(
                    result,
                    format!("/repo/[REDACTED]{suffix}"),
                    "{mode} {}",
                    shape.label
                );
            }
        }
    }
}

#[derive(Clone, Copy)]
enum HexCase {
    Upper,
    Lower,
    Mixed,
}
const HEX_CASES: [HexCase; 3] = [HexCase::Upper, HexCase::Lower, HexCase::Mixed];

fn encoded(separator: &str, depth: usize, hex_case: HexCase) -> String {
    if depth == 0 {
        return separator.to_owned();
    }
    let code = separator.encode_utf16().next().unwrap_or_default();
    let hex = format!("{code:02x}");
    let upper = matches!(hex_case, HexCase::Upper)
        || (matches!(hex_case, HexCase::Mixed) && depth % 2 == 0);
    let digits = if upper { hex.to_uppercase() } else { hex };
    format!("%{}{digits}", "25".repeat(depth - 1))
}

const SEPARATORS: [&str; 12] = ["/", ":", "@", "=", "?", "&", "#", " ", "+", "~", "-", "."];

#[test]
fn encoded_separator_equivalence() {
    let aws = "AKIAFIXTURE000000010".to_owned();
    let mut cells: Vec<(String, String, bool)> = Vec::new();
    for separator in SEPARATORS {
        for depth in 0..4 {
            for hex_case in HEX_CASES {
                for (value, shaped) in [
                    (format!("sk-proj-FIXTURE0006"), true),
                    (ghp(), true),
                    (jwt(), true),
                    (aws.clone(), true),
                    (secret(), false),
                ] {
                    cells.push((
                        format!("x{}{value}", encoded(separator, depth, hex_case)),
                        value,
                        shaped,
                    ));
                }
            }
        }
        for hex_case in HEX_CASES {
            let escape = encoded(separator, 1, hex_case);
            let mut depth_sets: Vec<[usize; 3]> = Vec::new();
            for a in 0..3 {
                for b in 0..3 {
                    for c in 0..3 {
                        depth_sets.push([a, b, c]);
                    }
                }
            }
            depth_sets.push([3, 2, 1]);
            depth_sets.push([1, 3, 2]);
            for depths in depth_sets {
                let boundary: String = escape
                    .chars()
                    .enumerate()
                    .map(|(index, c)| encoded(&c.to_string(), depths[index], hex_case))
                    .collect();
                let mut values = vec![
                    (sk(), true),
                    (ghp(), true),
                    (jwt(), true),
                    (aws.clone(), true),
                    (
                        "example.test/tasks-abc?model=faux-1&stream=true".to_owned(),
                        false,
                    ),
                ];
                if separator == "." {
                    values.push((jwt().split('.').collect::<Vec<_>>().join(&boundary), true));
                }
                for (value, shaped) in values {
                    cells.push((format!("x{boundary}{value}"), value, shaped));
                }
            }
        }
    }
    for (text, value, shaped) in cells {
        let full = redact_secrets(&text);
        assert_eq!(redact_credential_shapes(&text), full, "{text}");
        if shaped {
            assert!(full.contains("[REDACTED]"), "{text}");
            assert!(!full.contains(&value), "{text}");
        } else {
            assert_eq!(full, text);
        }
    }
}

#[test]
fn keeps_literal_percent_diagnostics_unchanged() {
    let mut texts = vec![
        format!("rate 100%% then {}", "x".repeat(62)),
        "Progress: %d%% complete while contacting https://api.example.test/v1/...?model=faux-1 request id req_123 failed".to_owned(),
    ];
    for form in ["%%", "%d", "%s", "100%", "%%%", "% ", "%zz"] {
        for length in [60, 64, 65, 200] {
            texts.push(format!("Progress: {form}{}", "x".repeat(length)));
        }
    }
    for (mode, redact) in MODES {
        for text in &texts {
            assert_eq!(&redact(text), text, "{mode}");
        }
    }
}

#[test]
fn consumes_each_encoding_depth_once_in_urls_and_named_values() {
    for depth in [1, 2, 3, 32, 4096] {
        let e = |c: &str| encoded(c, depth, HexCase::Mixed);
        for text in [
            format!(
                "https{}{}{}{}{}github.com",
                e(":"),
                e("/"),
                e("/"),
                ghp(),
                e("@")
            ),
            format!("redirect{}{}cb{}{}", e("="), e("/"), e("/"), jwt()),
        ] {
            assert_eq!(redact_credential_shapes(&text), redact_secrets(&text));
            assert!(redact_secrets(&text).contains("[REDACTED]"));
        }
        let named = format!("api_key{}{}", e("="), secret());
        assert_eq!(
            redact_secrets(&named),
            format!("api_key{}[REDACTED]", e("="))
        );
        assert_eq!(redact_credential_shapes(&named), named);
    }
}

#[test]
fn keeps_ordinary_encoded_urls_readable() {
    for depth in 0..4 {
        for hex_case in HEX_CASES {
            let e = |c: &str| encoded(c, depth, hex_case);
            let mut texts = vec![format!(
                "https{}{}{}example.test{}tasks-abc{}model{}faux-1{}stream{}true{}cb",
                e(":"),
                e("/"),
                e("/"),
                e("/"),
                e("?"),
                e("="),
                e("&"),
                e("="),
                e("#")
            )];
            for c in ["A", "_", "0"] {
                texts.push(format!("x{}{}", e(c), sk()));
            }
            texts.push(format!("x%GG{}", sk()));
            for text in texts {
                assert_eq!(redact_secrets(&text), text);
                assert_eq!(redact_credential_shapes(&text), text);
            }
        }
    }
}

#[test]
fn interprets_url_passwords_only_in_full_mode() {
    for depth in 0..4 {
        let e = |c: &str| encoded(c, depth, HexCase::Mixed);
        let text = format!(
            "https{}{}{}user{}hunter2pass{}example.test",
            e(":"),
            e("/"),
            e("/"),
            e(":"),
            e("@")
        );
        assert_eq!(
            redact_secrets(&text),
            format!(
                "https{}{}{}user{}[REDACTED]{}example.test",
                e(":"),
                e("/"),
                e("/"),
                e(":"),
                e("@")
            )
        );
        assert_eq!(redact_credential_shapes(&text), text);
    }
    for text in [":x //user:hunter2pass@host", ":x y //user:hunter2pass@host"] {
        assert_eq!(redact_secrets(text), text);
        assert_eq!(redact_credential_shapes(text), text);
    }
}

#[test]
fn redacts_conservatively_on_an_unresolved_window() {
    for (_, redact) in MODES {
        for head in [String::new(), format!("x/{} ", sk()), "api_key".to_owned()] {
            let text = format!("{head}%{}%33%44{}", "%25".repeat(80), secret());
            let result = redact(&text);
            assert!(result.contains("[REDACTED]"));
            assert!(!result.contains(&secret()));
            assert!(!result.contains(&sk()));
            assert_eq!(redact(&result), result);
        }
    }
}

#[test]
fn joins_canonical_depth_to_independently_encoded_hex_digits() {
    for depth in [1, 32, 4096] {
        for suffix in ["%32%46", "%32F", "2%46", "%2532%2546"] {
            let boundary = format!("%{}{suffix}", "25".repeat(depth));
            for (_, redact) in MODES {
                assert_eq!(
                    redact(&format!("x{boundary}{}", sk())),
                    format!("x{boundary}[REDACTED]")
                );
                let ordinary = format!("https://example.test/{boundary}tasks-abc?model=faux-1");
                assert_eq!(redact(&ordinary), ordinary);
            }
        }
    }
}

fn redacted_shapes() -> Vec<(&'static str, String, String)> {
    let s = secret();
    let fixture3 = "FIXTURE".repeat(3);
    vec![
        ("header line", format!("x-api-key: {s}"), s.clone()),
        (
            "camelCase JSON field",
            json_text(json!({ "apiKey": s })),
            s.clone(),
        ),
        (
            "sessionToken query",
            format!("https://gateway.example.test/v1?sessionToken={s}"),
            s.clone(),
        ),
        (
            "session as the whole name",
            format!("session: {s}"),
            s.clone(),
        ),
        (
            "typographic quotes",
            format!("\u{201C}api_key\u{201D}: \u{201C}{s}\u{201D}"),
            s.clone(),
        ),
        (
            "full-width colon",
            format!("api_key\u{FF1A} {s}"),
            s.clone(),
        ),
        ("XML element", format!("<apiKey>{s}</apiKey>"), s.clone()),
        (
            "HTML-escaped XML element",
            format!("&lt;apiKey&gt;{s}&lt;/apiKey&gt;"),
            s.clone(),
        ),
        (
            "HTML-escaped JSON",
            format!("&quot;api_key&quot;: &quot;{s}&quot;"),
            s.clone(),
        ),
        (
            "nested credentials object",
            json_text(json!({ "error": { "credentials": { "type": "service", "value": s } } })),
            s.clone(),
        ),
        (
            "escaped nested credentials object",
            json_text(json!(json_text(json!({ "credentials": { "value": s } })))),
            s.clone(),
        ),
        (
            "URL userinfo",
            format!("https://user:{s}@gateway.example.test/v1"),
            s.clone(),
        ),
        (
            "short URL userinfo password",
            "https://user:pw@gateway.example.test/v1".into(),
            ":pw@".into(),
        ),
        (
            "Azure SAS signature",
            format!("https://acct.blob.example.test/c/b?sv=2024-11-04&sr=b&sig={s}"),
            s.clone(),
        ),
        (
            "second cookie pair after a short first",
            format!("Cookie: a=1; __cf_bm={s}"),
            s.clone(),
        ),
        (
            "short cookie value",
            "Cookie: a=1; sid=s3cr3t".into(),
            "s3cr3t".into(),
        ),
        (
            "set-cookie array",
            json_text(json!({ "set-cookie": ["a=1", format!("sid={s}")] })),
            s.clone(),
        ),
        (
            "escaped set-cookie array",
            json_text(json!(json_text(
                json!({ "set-cookie": ["a=1", format!("sid={s}")] })
            ))),
            s.clone(),
        ),
        (
            "URL-encoded cookie",
            format!(
                "model=faux-1&Cookie={}&stream=true",
                encode_uri_component(&format!("a=1; sid={s}"))
            ),
            s.clone(),
        ),
        (
            "gsk_ prefix",
            format!("rejected gsk_{fixture3}0012"),
            format!("gsk_{fixture3}0012"),
        ),
        (
            "xai- prefix",
            format!("rejected xai-{fixture3}0013"),
            format!("xai-{fixture3}0013"),
        ),
        (
            "hf_ prefix",
            format!("rejected hf_{fixture3}0014"),
            format!("hf_{fixture3}0014"),
        ),
        (
            "rk_live_ prefix",
            format!("rejected rk_live_{fixture3}0015"),
            format!("{fixture3}0015"),
        ),
        (
            "xoxb- prefix",
            format!("rejected xoxb-{}-0016", "FIXTURE".repeat(2)),
            format!("{}-0016", "FIXTURE".repeat(2)),
        ),
        (
            "numbered environment name",
            format!("OPENAI_API_KEY_2={s}"),
            s.clone(),
        ),
        (
            "digits glued to the word",
            format!("token2: {s}"),
            s.clone(),
        ),
        (
            "numbered header name",
            format!("x-api-key-1: {s}"),
            s.clone(),
        ),
        (
            "value qualifier",
            json_text(json!({ "api_key_value": s })),
            s.clone(),
        ),
        (
            "camelCase id qualifier",
            json_text(json!({ "apiKeyId": s })),
            s.clone(),
        ),
        (
            "a field named bearer",
            json_text(json!({ "bearer": "abcdefgh1234" })),
            "abcdefgh1234".into(),
        ),
        (
            "a 130-character name",
            json_text(json!({ format!("{}_api_key", "a".repeat(130)): s })),
            s.clone(),
        ),
        (
            "a long dotted path",
            format!("request.headers.{}.authorization: {s}", "x".repeat(120)),
            s.clone(),
        ),
        (
            "a short password",
            json_text(json!({ "password": "hunter2-shrt" })),
            "hunter2-shrt".into(),
        ),
        (
            "a short Basic credential",
            format!("Authorization: Basic {}", ["dXNl", "cjpw", "YXNz"].concat()),
            "dXNlcjpwYXNz".into(),
        ),
        (
            "a JWT ending a sentence",
            format!("rejected {}.", jwt()),
            "eyJhbGci".into(),
        ),
        (
            "a token as git remote userinfo",
            format!("https://{}@github.com/o/r.git", ghp()),
            ghp(),
        ),
        (
            "a token after git remote user:",
            format!("https://x-access-token:{}@github.com/o/r.git", ghp()),
            ghp(),
        ),
        (
            "a key in a URL path segment",
            format!("/v1/keys/{}", sk()),
            sk(),
        ),
        ("a key joined with +", format!("x+{}", sk()), sk()),
        ("a key after ~/", format!("~/{}", sk()), sk()),
        (
            "an AWS key id after /",
            "see /AKIAFIXTURE000000010".into(),
            "FIXTURE000000010".into(),
        ),
        (
            "a JWT in a URL path",
            format!("/cb/{}", jwt()),
            "eyJzdWIi".into(),
        ),
        (
            "a JWT followed by a path",
            format!("/cb/{}/more", jwt()),
            "eyJzdWIi".into(),
        ),
        (
            "a key as proxy userinfo",
            format!("https://{}@proxy.example.test", sk()),
            sk(),
        ),
        ("a token after user@", format!("deploy@{}", ghp()), ghp()),
        (
            "a token in an encoded git remote",
            format!("https%3A%2F%2F{}%40github.com", ghp()),
            ghp(),
        ),
        (
            "a key after an encoded path separator",
            format!("/v1%2F{}", sk()),
            sk(),
        ),
        (
            "a JWT after an encoded path separator",
            format!("redirect=%2Fcb%2F{}", jwt()),
            "eyJzdWIi".into(),
        ),
        ("a key after encoded +", format!("x%2B{}", sk()), sk()),
        ("a key after encoded ~", format!("x%7E{}", sk()), sk()),
        ("a key after encoded -", format!("x%2D{}", sk()), sk()),
        ("a key after encoded .", format!("x%2e{}", sk()), sk()),
        (
            "a key after lowercase encoded /",
            format!("x%2f{}", sk()),
            sk(),
        ),
        (
            "a quoted password with punctuation",
            "password: \"Hunter2!zzTailA01\"".into(),
            "zzTailA01".into(),
        ),
        (
            "a bare password with @ and :",
            "password=p@zzTailA02:word123".into(),
            "zzTailA02".into(),
        ),
        (
            "a non-ASCII password",
            json_text(json!({ "password": "p\u{00e4}sswzzTailA03" })),
            "zzTailA03".into(),
        ),
        (
            "a quoted password with spaces",
            json_text(json!({ "password": "abc def zzTailA04" })),
            "zzTailA04".into(),
        ),
        (
            "an escaped quoted authorization",
            json_text(json!(json_text(
                json!({ "authorization": "Bearer abc zzTailA05" })
            ))),
            "zzTailA05".into(),
        ),
        (
            "an XML password with punctuation",
            "<password>ab!zzTailA06</password>".into(),
            "zzTailA06".into(),
        ),
    ]
}

#[test]
fn redact_secrets_masks_every_credential_shape_idempotently() {
    for (label, text, leaked) in redacted_shapes() {
        let redacted = redact_secrets(&text);
        assert!(redacted.contains("[REDACTED]"), "{label}: {redacted}");
        assert!(!redacted.contains(&leaked), "{label}: {redacted}");
        assert_eq!(redact_secrets(&redacted), redacted, "{label}");
    }
}

#[test]
fn redact_secrets_keeps_diagnostics_readable() {
    let readable = [
        "\"monkey\": \"abcdefghijklmnopqrstuvwxyz\"".to_owned(),
        "turkey=abcdefghijklmnopqrstuv".into(),
        "\"keyword\": \"abcdefghijklmnopqrstuvwxyz\"".into(),
        "\"tokenizer\": \"cl100k_base_tokenizer_v2\"".into(),
        "primary_key_column=customer_identifier_column".into(),
        "max_tokens: 1234567890123456789".into(),
        "session_id: 0f8fad5b-d9cb-469f-a165-70867728950e".into(),
        "token_type: bearer_access_token_v2_long".into(),
        "authorization_endpoint=api.example.com/oauth2/authorize".into(),
        "token_count: 12345678901234567890".into(),
        "https://gateway.example.test:8443/v1/chat?model=faux-1&stream=true".into(),
        "{\"type\":\"invalid_request_error\",\"param\":\"model\",\"max_tokens\":4096} request id: req_011CUxFIXTURE".into(),
        "model us.anthropic.claude-opus-4-5-20251101-v1:0, gpt-6.1-sol, task-12345678".into(),
        "tasks-abcdefghijk /srv/risk-assessment-v2 disk-cache-directory-1234 /opt/AKIA_notes".into(),
        format!("x%41{0} x%5F{0} x%GG{0}", sk()),
    ];
    for text in readable {
        assert_eq!(redact_secrets(&text), text);
    }
}

#[test]
fn redact_secrets_uses_the_callers_replacement_and_keeps_structure() {
    assert_eq!(
        crate::utils::secret_redaction::redact_secrets_with(
            &format!("api_key={}", secret()),
            "[redacted]"
        ),
        "api_key=[redacted]"
    );
    assert_eq!(
        redact_secrets(&format!("https://{}@github.com/o/r.git", ghp())),
        "https://[REDACTED]@github.com/o/r.git"
    );
    assert_eq!(
        redact_secrets(&format!("/v1/keys/{}", sk())),
        "/v1/keys/[REDACTED]"
    );
    assert_eq!(
        redact_secrets("password: \"Hunter2!xyz9\", next"),
        "password: \"[REDACTED]\", next"
    );
    assert_eq!(
        redact_secrets("password=p@ss:word123&model=faux-1"),
        "password=[REDACTED]&model=faux-1"
    );
    assert_eq!(
        redact_secrets(&json_text(
            json!({ "password": "a\"b c", "model": "faux-1" })
        )),
        json_text(json!({ "password": "[REDACTED]", "model": "faux-1" }))
    );
}

pub(crate) fn failed_message(error_message: &str, stop_reason: StopReason) -> AssistantMessage {
    let mut message: AssistantMessage = serde_json::from_value(json!({
        "role": "assistant", "content": [], "api": "openai-completions", "provider": "faux",
        "model": "faux-1",
        "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } },
        "stopReason": "error", "timestamp": 0
    }))
    .expect("assistant message fixture");
    message.stop_reason = stop_reason;
    message.error_message = Some(error_message.to_owned());
    message
}

pub(crate) fn model_not_found() -> String {
    format!(
        "400 {{\"code\":\"model_not_found\",\"message\":\"unknown provider for model faux-1\"}} x-api-key: {}",
        secret()
    )
}

#[test]
fn classifies_a_model_configuration_error_on_the_raw_text_then_keeps_only_the_redacted_text() {
    let mut message = failed_message(&model_not_found(), StopReason::Error);
    sanitize_provider_error_message(&mut message);
    assert_eq!(
        message.error_cause,
        Some(AssistantErrorCause::ModelConfig {
            model: "faux/faux-1".into()
        })
    );
    let text = message.error_message.expect("error message");
    assert!(text.contains("model_not_found"));
    assert!(!text.contains(&secret()));
}

#[test]
fn does_not_classify_an_aborted_or_stopped_message_that_mentions_model_not_found() {
    for stop_reason in [StopReason::Aborted, StopReason::Stop] {
        let mut message = failed_message(&model_not_found(), stop_reason);
        sanitize_provider_error_message(&mut message);
        assert_eq!(message.error_cause, None);
        assert!(!message.error_message.expect("text").contains(&secret()));
    }
}

#[test]
fn model_config_error_recognizes_each_route_shape() {
    for text in [
        "error code model_not_found",
        "Unknown provider for model x",
        "The model `gpt-9` does not exist or you do not have access",
        r#"{"type":"not_found_error","message":"model: claude-x"}"#,
        "models/gemini-9 is not found for API version v1",
    ] {
        assert!(is_model_config_error(Some(text)), "{text}");
    }
    assert!(!is_model_config_error(Some(
        r#"{"type":"not_found_error","message":"file missing"}"#
    )));
    assert!(!is_model_config_error(None));
}

fn fill(unit: &str, size: usize) -> String {
    unit.repeat(size.div_ceil(unit.len()))
        .chars()
        .take(size)
        .collect()
}

fn reads(run: impl Fn()) -> u64 {
    READS.with(|reads| reads.set(0));
    run();
    READS.with(std::cell::Cell::get)
}

const BASE: usize = 10_000;
// A linear pass grows about 10x per step; a quadratic one grows 100x.
const MAX_GROWTH: f64 = 30.0;

fn growth(run: impl Fn(&str), make: &dyn Fn(usize) -> String) -> Vec<f64> {
    let counts: Vec<u64> = [BASE, BASE * 10, BASE * 100]
        .iter()
        .map(|&size| {
            let text = make(size);
            reads(|| run(&text))
        })
        .collect();
    (1..3)
        .map(|step| counts[step] as f64 / counts[step - 1].max(1) as f64)
        .collect()
}

#[allow(clippy::type_complexity)]
fn scaling_shapes() -> Vec<(&'static str, Box<dyn Fn(usize) -> String>)> {
    vec![
        (
            "one huge token with no delimiter",
            Box::new(|size| "a".repeat(size)),
        ),
        ("sk- near misses", Box::new(|size| fill("sk-abcd ", size))),
        ("eyJ- runs", Box::new(|size| fill("eyJ-", size))),
        ("eyJ_ runs", Box::new(|size| fill("eyJ_", size))),
        ("1eyJ runs", Box::new(|size| fill("1eyJ", size))),
        (
            "JWT-like tokens one segment short",
            Box::new(|size| fill("eyJaaaaaaaa.bbbbb ", size)),
        ),
        (
            "one dotted token past three segments",
            Box::new(|size| format!("eyJ{}", fill("aaaaa.", size))),
        ),
        ("AKIA near misses", Box::new(|size| fill("AKIA0123 ", size))),
        (
            "a long run after a name separator",
            Box::new(|size| format!("api_key: {}", "a".repeat(size))),
        ),
        (
            "a deep name before a colon",
            Box::new(|size| format!("{}: x", "a".repeat(size))),
        ),
        (
            "a deep separated name before =",
            Box::new(|size| format!("{}=x", fill("a_", size))),
        ),
        (
            "a deep camelCase name",
            Box::new(|size| format!("{}: x", fill("aB", size))),
        ),
        (
            "a keyword repeated",
            Box::new(|size| fill("aaaaakey", size)),
        ),
        (
            "short credential values",
            Box::new(|size| fill("key: a ", size)),
        ),
        ("separators", Box::new(|size| fill("a:", size))),
        (
            "whitespace after a name",
            Box::new(|size| format!("key:{}", " ".repeat(size))),
        ),
        (
            "cookie pairs",
            Box::new(|size| format!("Cookie: {}", fill("a=1; ", size))),
        ),
        ("cookie names", Box::new(|size| fill("Cookie: ", size))),
        (
            "open credential objects",
            Box::new(|size| fill("api_key: {", size)),
        ),
        (
            "XML credential tags",
            Box::new(|size| fill("<apiKey>", size)),
        ),
        (
            "URL userinfo heads",
            Box::new(|size| fill("https://u:", size)),
        ),
        ("bearer schemes", Box::new(|size| fill("bearer ", size))),
        (
            "escaped quotes after a name",
            Box::new(|size| format!("key{}", fill("\\\"", size))),
        ),
        ("percent escapes", Box::new(|size| fill("%41", size))),
        (
            "encoded / and near misses inside one token",
            Box::new(|size| fill("%2Fsk-abcd%2FeyJ", size)),
        ),
        (
            "one arbitrarily deep encoded boundary",
            Box::new(|size| format!("x%{}2Fsk-abcd", "25".repeat(size))),
        ),
        (
            "deep encoded delimiter",
            Box::new(|size| format!("api_key%{}3Dx", "25".repeat(size))),
        ),
        (
            "repeated nested escapes",
            Box::new(|size| fill("%25252Fsk-abcd", size)),
        ),
        (
            "independently encoded hex digits",
            Box::new(|size| fill("%25%32%46sk-abcd", size)),
        ),
        (
            "mixed-depth encoded hex digits",
            Box::new(|size| fill("%2525%32Fsk-abcd", size)),
        ),
        (
            "encoded JWT dots",
            Box::new(|size| fill("/eyJaaaaaaaa%25%32%45bbbbb%2Eccccc", size)),
        ),
        (
            "unresolved percent window",
            Box::new(|size| format!("{}32Fsk-abcd", "%".repeat(size))),
        ),
        (
            "deep encoded hex digit",
            Box::new(|size| format!("x%25%{}32Fsk-abcd", "25".repeat(size))),
        ),
        ("HTML entities", Box::new(|size| fill("&quot;", size))),
        (
            "130-character names",
            Box::new(|size| fill(&format!("{}_api_key: x ", "a".repeat(130)), size)),
        ),
        (
            "model-not-found prose heads",
            Box::new(|size| fill("the model ", size)),
        ),
        (
            "sk- near misses inside one token",
            Box::new(|size| fill("/sk-abcd", size)),
        ),
        (
            "ghp_ near misses inside one token",
            Box::new(|size| fill("/ghp_abc", size)),
        ),
        (
            "AKIA near misses inside one token",
            Box::new(|size| fill("+AKIA0123", size)),
        ),
        (
            "every prefix near miss inside one token",
            Box::new(|size| fill("/sk-a~gsk_a+xai-a/hf_a-xoxb-a/AIza.eyJ.", size)),
        ),
        (
            "eyJ candidates after / inside one token",
            Box::new(|size| fill("/eyJaaaaaaaa.bbbbb", size)),
        ),
        (
            "dotted eyJ segments after hyphens",
            Box::new(|size| fill("-eyJaaaaaaaa.", size)),
        ),
        (
            "eyJ candidates in one dotless segment",
            Box::new(|size| fill("-eyJ", size)),
        ),
        (
            "a long quoted strict value",
            Box::new(|size| format!("password: \"{}\"", fill("ab c!", size))),
        ),
        (
            "a long unclosed quoted strict value",
            Box::new(|size| format!("password: \"{}", fill("ab c!", size))),
        ),
        (
            "a long bare strict value",
            Box::new(|size| format!("password={}", fill("p@ss:", size))),
        ),
        (
            "strict names opening quotes",
            Box::new(|size| fill("password: \"", size)),
        ),
        (
            "escaped quotes inside a strict value",
            Box::new(|size| format!("secret: \"{}\"", fill("a\\\"", size))),
        ),
    ]
}

#[test]
fn redaction_grows_linearly_in_both_modes() {
    for (label, make) in scaling_shapes() {
        for (mode, redact) in MODES {
            for ratio in growth(|text| drop(redact(text)), make.as_ref()) {
                assert!(ratio < MAX_GROWTH, "{mode} on {label}: growth {ratio}");
            }
        }
    }
}

#[test]
fn redaction_grows_linearly_at_the_stream_boundary() {
    let push = |error_message: &str| {
        let stream = crate::utils::event_stream::create_assistant_message_event_stream();
        stream.push(crate::types::AssistantMessageEvent::Error {
            reason: crate::types::ErrorReason::Error,
            error: failed_message(error_message, StopReason::Error),
        });
    };
    for ratio in growth(push, &|size| fill("eyJ-", size)) {
        assert!(ratio < MAX_GROWTH, "growth {ratio}");
    }
}

#[test]
fn the_token_scanner_sources_hold_no_regular_expression() {
    for source in [
        include_str!("secret_lexer.rs"),
        include_str!("secret_classifier.rs"),
        include_str!("secret_redaction.rs"),
    ] {
        assert!(!source.contains(concat!("Regex", "::")));
        assert!(!source.contains(concat!("use ", "regex")));
    }
}
