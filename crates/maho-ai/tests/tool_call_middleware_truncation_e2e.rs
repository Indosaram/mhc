//! senpi test/tool-call-middleware/truncation-e2e.test.ts (fixtures from truncation-fixtures.ts).
//!
//! Each case streams a faux `openai-completions` response through `compat::stream_simple` with a
//! model whose `compat.toolCallFormat` selects the text tool-call protocol. The faux provider is
//! registered inside a `ProviderScope` (TS registers/unregisters around each case) so parallel
//! tests in this binary never see it.

use maho_ai::compat::{register_faux_provider, stream_simple};
use maho_ai::model::ModelCompat;
use maho_ai::node::provider_scope::{ProviderScope, run_with_provider_scope};
use maho_ai::providers::faux::{
    FauxAssistantMessageOptions, RegisterFauxProviderOptions, faux_assistant_message, faux_text,
};
use maho_ai::types::{
    AssistantMessage, ContentBlock, Context, Message, StopReason, Tool, ToolCall, UserContent,
    UserMessage,
};
use serde_json::{Map, Value, json};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Recovered,
    Incomplete,
    Dropped,
    Text,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FixtureFormat {
    AnthropicXml,
    Hermes,
    YamlXml,
    MorphXml,
    Gemma4Delimiter,
}

struct Fixture {
    title: &'static str,
    input: &'static str,
    tool: &'static str,
    kind: Kind,
    arguments: Value,
}

type FixtureRow = (&'static str, &'static str, &'static str, Kind);

const fn fixture(
    title: &'static str,
    input: &'static str,
    tool: &'static str,
    kind: Kind,
) -> FixtureRow {
    (title, input, tool, kind)
}

fn fixtures(format: FixtureFormat) -> Vec<Fixture> {
    use Kind::*;
    let seoul = || json!({"city": "Seoul"});
    let rows: Vec<(FixtureRow, Value)> = match format {
        FixtureFormat::AnthropicXml => vec![
            (
                fixture(
                    "R3/R4-pass: closed parameter with only invoke close missing",
                    r#"<invoke name="get_weather"><parameter name="city">Seoul</parameter>"#,
                    "get_weather",
                    Recovered,
                ),
                seoul(),
            ),
            (
                fixture(
                    "R3/R4-pass: proper invoke-close prefix after complete parameters",
                    r#"<invoke name="get_weather"><parameter name="city">Seoul</parameter></inv"#,
                    "get_weather",
                    Recovered,
                ),
                seoul(),
            ),
            (
                fixture(
                    "R3-fail: mid-value cut leaves parameter unclosed",
                    r#"<invoke name="get_weather"><parameter name="city">Seo"#,
                    "get_weather",
                    Incomplete,
                ),
                Value::Null,
            ),
            (
                fixture(
                    "R4-fail: closed parameters violate todowrite minItems",
                    r#"<invoke name="todowrite"><parameter name="todos">[]</parameter>"#,
                    "todowrite",
                    Incomplete,
                ),
                Value::Null,
            ),
            (
                fixture(
                    "R1-fail: nameless invoke prefix is dropped",
                    "<invoke na",
                    "get_weather",
                    Dropped,
                ),
                Value::Null,
            ),
            (
                fixture(
                    "R2-fail: unknown invoke remains ordinary text",
                    r#"<invoke name="unknown_tool"><parameter name="city">Seoul</parameter>"#,
                    "get_weather",
                    Text,
                ),
                Value::Null,
            ),
        ],
        FixtureFormat::Hermes => vec![
            (
                fixture(
                    "R3/R4-pass: complete JSON object with terminator missing",
                    r#"<tool_call>{"name":"get_weather","arguments":{"city":"Seoul"}}"#,
                    "get_weather",
                    Recovered,
                ),
                seoul(),
            ),
            (
                fixture(
                    "R3/R4-pass: complete JSON object with terminator prefix",
                    r#"<tool_call>{"name":"get_weather","arguments":{"city":"Seoul"}}</tool_"#,
                    "get_weather",
                    Recovered,
                ),
                seoul(),
            ),
            (
                fixture(
                    "R3-fail: mid-value JSON cut is incomplete",
                    r#"<tool_call>{"name":"get_weather","arguments":{"city":"Seo"#,
                    "get_weather",
                    Incomplete,
                ),
                Value::Null,
            ),
            (
                fixture(
                    "R4-fail: complete JSON violates todowrite minItems",
                    r#"<tool_call>{"name":"todowrite","arguments":{"todos":[]}}"#,
                    "todowrite",
                    Incomplete,
                ),
                Value::Null,
            ),
            (
                fixture(
                    "Nameless/R2-fail: JSON tool-call fragment has no resolvable name",
                    r#"<tool_call>{"na"#,
                    "get_weather",
                    Dropped,
                ),
                Value::Null,
            ),
        ],
        FixtureFormat::Gemma4Delimiter => vec![
            (
                fixture(
                    "R3/R4-pass: balanced arguments with terminator missing",
                    r#"<|tool_call>call:get_weather{city:<|"|>Seoul<|"|>}"#,
                    "get_weather",
                    Recovered,
                ),
                seoul(),
            ),
            (
                fixture(
                    "R3-fail: unbalanced argument object is incomplete",
                    r#"<|tool_call>call:get_weather{city:<|"|>Seoul<|"|>"#,
                    "get_weather",
                    Incomplete,
                ),
                Value::Null,
            ),
            (
                fixture(
                    "R4-fail: balanced arguments violate todowrite minItems",
                    "<|tool_call>call:todowrite{todos:[]}",
                    "todowrite",
                    Incomplete,
                ),
                Value::Null,
            ),
            (
                fixture(
                    "R1-fail: nameless call prefix is dropped",
                    "<|tool_call>call:",
                    "get_weather",
                    Dropped,
                ),
                Value::Null,
            ),
            (
                fixture(
                    "R2-fail: unknown Gemma tool is dropped",
                    r#"<|tool_call>call:unknown_tool{city:<|"|>Seoul<|"|>}"#,
                    "get_weather",
                    Dropped,
                ),
                Value::Null,
            ),
        ],
        FixtureFormat::MorphXml => vec![
            (
                fixture(
                    "R3/R4-pass: closed child with only tool close missing",
                    "<get_weather><city>NY</city>",
                    "get_weather",
                    Recovered,
                ),
                json!({"city": "NY"}),
            ),
            (
                fixture(
                    "R3-fail: unclosed child makes XML argument body unparseable",
                    "<get_weather><city>NY</city><days>",
                    "get_weather",
                    Incomplete,
                ),
                Value::Null,
            ),
            (
                fixture(
                    "R4-fail: parseable empty todos array violates minItems",
                    "<todowrite><todos></todos>",
                    "todowrite",
                    Incomplete,
                ),
                Value::Null,
            ),
            (
                fixture(
                    "R2-fail: unknown XML tag remains ordinary text",
                    "<unknown_tool><city>Seoul</city>",
                    "get_weather",
                    Text,
                ),
                Value::Null,
            ),
        ],
        FixtureFormat::YamlXml => vec![
            (
                fixture(
                    "R3/R4-pass: parseable YAML mapping with closing tag missing",
                    "<get_weather>\ncity: Seoul",
                    "get_weather",
                    Recovered,
                ),
                seoul(),
            ),
            (
                fixture(
                    "R3/R4-pass: empty YAML body validates empty get_location arguments",
                    "<get_location>\n",
                    "get_location",
                    Recovered,
                ),
                json!({}),
            ),
            (
                fixture(
                    "R3-fail: invalid YAML mapping is incomplete",
                    "<get_weather>\n[invalid: yaml:",
                    "get_weather",
                    Incomplete,
                ),
                Value::Null,
            ),
            (
                fixture(
                    "R4-fail: parseable YAML violates todowrite minItems",
                    "<todowrite>\ntodos: []",
                    "todowrite",
                    Incomplete,
                ),
                Value::Null,
            ),
            (
                fixture(
                    "R2-fail: unknown YAML XML tag remains ordinary text",
                    "<unknown_tool>\ncity: Seoul",
                    "get_weather",
                    Text,
                ),
                Value::Null,
            ),
        ],
    };
    rows.into_iter()
        .map(|((title, input, tool, kind), arguments)| Fixture {
            title,
            input,
            tool,
            kind,
            arguments,
        })
        .collect()
}

fn fixture_tools() -> Vec<Tool> {
    let tool = |name: &str, description: &str, parameters: Value| Tool {
        name: name.into(),
        description: description.into(),
        parameters,
        output_schema: None,
        freeform: None,
        constrained_sampling: None,
    };
    vec![
        tool(
            "get_weather",
            "Get weather",
            json!({
                "type": "object",
                "properties": {"city": {"type": "string"}, "days": {"type": "integer"}},
                "required": ["city"],
            }),
        ),
        tool(
            "todowrite",
            "Write todos",
            json!({
                "type": "object",
                "properties": {"todos": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": {"type": "string"},
                            "status": {"type": "string"},
                            "priority": {"type": "string"},
                        },
                        "required": ["content", "status", "priority"],
                    },
                }},
                "required": ["todos"],
            }),
        ),
        tool(
            "get_location",
            "Get location",
            json!({"type": "object", "properties": {}}),
        ),
    ]
}

const FORMAT_ROWS: [(&str, &str, FixtureFormat); 6] = [
    (
        "anthropic-xml",
        "anthropic-xml",
        FixtureFormat::AnthropicXml,
    ),
    ("hermes", "hermes", FixtureFormat::Hermes),
    ("yaml-xml", "yaml-xml", FixtureFormat::YamlXml),
    ("morph-xml alias xml", "xml", FixtureFormat::MorphXml),
    ("morph-xml canonical", "morph-xml", FixtureFormat::MorphXml),
    (
        "gemma4-delimiter",
        "gemma4-delimiter",
        FixtureFormat::Gemma4Delimiter,
    ),
];

fn fixture_rows(kind: Kind) -> Vec<(&'static str, &'static str, FixtureFormat, Fixture)> {
    FORMAT_ROWS
        .iter()
        .flat_map(|&(label, format, fixture_format)| {
            fixtures(fixture_format)
                .into_iter()
                .filter(move |fixture| fixture.kind == kind)
                .map(move |fixture| (label, format, fixture_format, fixture))
        })
        .collect()
}

fn opening_marker(format: FixtureFormat) -> &'static str {
    match format {
        FixtureFormat::AnthropicXml => "<invoke",
        FixtureFormat::Hermes => "<tool_call>",
        FixtureFormat::YamlXml | FixtureFormat::MorphXml => "<",
        FixtureFormat::Gemma4Delimiter => "<|tool_call>",
    }
}

fn complete_call(format: FixtureFormat) -> &'static str {
    match format {
        FixtureFormat::AnthropicXml => {
            r#"<invoke name="get_weather"><parameter name="city">Seoul</parameter></invoke>"#
        }
        FixtureFormat::Hermes => {
            r#"<tool_call>{"name":"get_weather","arguments":{"city":"Seoul"}}</tool_call>"#
        }
        FixtureFormat::YamlXml => "<get_weather>\ncity: Seoul\n</get_weather>",
        FixtureFormat::MorphXml => "<get_weather><city>Seoul</city></get_weather>",
        FixtureFormat::Gemma4Delimiter => {
            r#"<|tool_call>call:get_weather{city:<|"|>Seoul<|"|>}<tool_call|>"#
        }
    }
}

fn text_blocks(message: &AssistantMessage) -> Vec<&str> {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

fn tool_calls(message: &AssistantMessage) -> Vec<&ToolCall> {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall(call) => Some(call),
            _ => None,
        })
        .collect()
}

fn arguments(call: &ToolCall) -> Value {
    Value::Object(call.arguments.clone())
}

async fn stream_fixture(format: &str, input: &str, stop_reason: StopReason) -> AssistantMessage {
    let scope = ProviderScope::new();
    let (faux, stream) = run_with_provider_scope(&scope, || {
        let faux = register_faux_provider(RegisterFauxProviderOptions {
            api: Some("openai-completions".into()),
            ..RegisterFauxProviderOptions::default()
        })
        .expect("register faux provider");
        faux.set_responses(vec![
            faux_assistant_message(
                vec![faux_text(input)],
                FauxAssistantMessageOptions {
                    stop_reason: Some(stop_reason),
                    error_message: (stop_reason == StopReason::Error)
                        .then(|| "transport error injected by faux provider".to_owned()),
                    ..FauxAssistantMessageOptions::default()
                },
            )
            .into(),
        ]);
        let mut model = faux.get_model(None).expect("faux model");
        model.compat = Some(ModelCompat(Map::from_iter([(
            "toolCallFormat".to_owned(),
            json!(format),
        )])));
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                content: UserContent::Text("Use the available tool.".into()),
                timestamp: 0,
            })],
            tools: Some(fixture_tools()),
            active_tool_names: None,
        };
        let stream = stream_simple(&model, &context, None);
        (faux, stream)
    })
    .expect("provider scope");
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), stream.result())
        .await
        .expect("bounded stream")
        .expect("stream result");
    assert_eq!(faux.get_pending_response_count(), 0);
    result
}

fn assert_no_marker(result: &AssistantMessage, format: FixtureFormat, context: &str) {
    assert!(
        text_blocks(result)
            .iter()
            .all(|text| !text.contains(opening_marker(format))),
        "{context}: {:?}",
        text_blocks(result)
    );
}

async fn assert_recovered_rows(stop_reason: StopReason) {
    for (label, format, fixture_format, fixture) in fixture_rows(Kind::Recovered) {
        let context = format!("{label}: {}", fixture.title);
        let result = stream_fixture(format, fixture.input, stop_reason).await;
        assert_eq!(result.stop_reason, StopReason::ToolUse, "{context}");
        let calls = tool_calls(&result);
        assert_eq!(calls.len(), 1, "{context}");
        assert_eq!(calls[0].name, fixture.tool, "{context}");
        assert_eq!(arguments(calls[0]), fixture.arguments, "{context}");
        assert_eq!(calls[0].incomplete, None, "{context}");
        assert_no_marker(&result, fixture_format, &context);
    }
}

#[tokio::test]
async fn recovers_eof_fixtures() {
    assert_recovered_rows(StopReason::Stop).await;
}

#[tokio::test]
async fn converts_length_to_tool_use_for_recovered_fixtures() {
    assert_recovered_rows(StopReason::Length).await;
}

#[tokio::test]
async fn flags_unrecoverable_eof_fixtures() {
    for (label, format, fixture_format, fixture) in fixture_rows(Kind::Incomplete) {
        let context = format!("{label}: {}", fixture.title);
        let result = stream_fixture(format, fixture.input, StopReason::Stop).await;
        let calls = tool_calls(&result);
        assert_eq!(calls.len(), 1, "{context}");
        assert_eq!(calls[0].name, fixture.tool, "{context}");
        assert_eq!(calls[0].incomplete, Some(true), "{context}");
        assert_no_marker(&result, fixture_format, &context);
    }
}

#[tokio::test]
async fn drops_nameless_or_unknown_eof_fixtures() {
    for (label, format, fixture_format, fixture) in fixture_rows(Kind::Dropped) {
        let context = format!("{label}: {}", fixture.title);
        let result = stream_fixture(format, fixture.input, StopReason::Stop).await;
        assert!(tool_calls(&result).is_empty(), "{context}");
        assert!(
            text_blocks(&result)
                .iter()
                .all(|text| !text.contains(fixture.input)),
            "{context}"
        );
        assert_no_marker(&result, fixture_format, &context);
    }
}

#[tokio::test]
async fn preserves_unknown_name_fixtures_verbatim_as_text() {
    for (label, format, _, fixture) in fixture_rows(Kind::Text) {
        let context = format!("{label}: {}", fixture.title);
        let result = stream_fixture(format, fixture.input, StopReason::Stop).await;
        assert!(tool_calls(&result).is_empty(), "{context}");
        assert!(text_blocks(&result).contains(&fixture.input), "{context}");
    }
}

#[tokio::test]
async fn mixed_recovered_calls_preserve_content_order() {
    for (label, format, fixture_format) in FORMAT_ROWS {
        let fixture = fixtures(fixture_format)
            .into_iter()
            .find(|fixture| fixture.kind == Kind::Recovered)
            .expect("recovered fixture");
        let input = format!("{}{}", complete_call(fixture_format), fixture.input);
        let result = stream_fixture(format, &input, StopReason::Stop).await;
        assert_eq!(result.stop_reason, StopReason::ToolUse, "{label}");
        let calls = tool_calls(&result);
        assert_eq!(calls.len(), 2, "{label}");
        assert_eq!(
            calls.iter().map(|call| call.incomplete).collect::<Vec<_>>(),
            [None, None],
            "{label}"
        );
        assert_eq!(
            calls.iter().map(|call| arguments(call)).collect::<Vec<_>>(),
            [json!({"city": "Seoul"}), fixture.arguments],
            "{label}"
        );
    }
}

#[tokio::test]
async fn mixed_incomplete_calls_preserve_content_order() {
    for (label, format, fixture_format) in FORMAT_ROWS {
        let fixture = fixtures(fixture_format)
            .into_iter()
            .find(|fixture| fixture.kind == Kind::Incomplete)
            .expect("incomplete fixture");
        let input = format!("{}{}", complete_call(fixture_format), fixture.input);
        let result = stream_fixture(format, &input, StopReason::Stop).await;
        assert_eq!(result.stop_reason, StopReason::ToolUse, "{label}");
        let calls = tool_calls(&result);
        assert_eq!(calls.len(), 2, "{label}");
        assert_eq!(calls[0].name, "get_weather", "{label}");
        assert_eq!(arguments(calls[0]), json!({"city": "Seoul"}), "{label}");
        assert_eq!(calls[0].incomplete, None, "{label}");
        assert_eq!(calls[1].name, fixture.tool, "{label}");
        assert_eq!(calls[1].incomplete, Some(true), "{label}");
    }
}

#[tokio::test]
async fn recovers_completed_calls_and_flags_the_trailing_incomplete_call_in_an_anthropic_wrapper() {
    let response = concat!(
        r#"<function_calls><invoke name="get_weather"><parameter name="city">Seoul</parameter></invoke>"#,
        r#"<invoke name="get_weather"><parameter name="city">Seo"#,
    );
    let result = stream_fixture("anthropic-xml", response, StopReason::Stop).await;
    assert_eq!(result.stop_reason, StopReason::ToolUse);
    let calls = tool_calls(&result);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].name, "get_weather");
    assert_eq!(arguments(calls[0]), json!({"city": "Seoul"}));
    assert_eq!(calls[0].incomplete, None);
    assert_eq!(calls[1].name, "get_weather");
    assert_eq!(calls[1].incomplete, Some(true));
    assert!(
        text_blocks(&result)
            .iter()
            .all(|text| !text.contains("<function_calls>"))
    );
}

#[tokio::test]
async fn retains_a_finalized_call_when_the_faux_provider_ends_with_a_transport_error() {
    let result = stream_fixture(
        "morph-xml",
        complete_call(FixtureFormat::MorphXml),
        StopReason::Error,
    )
    .await;
    assert_eq!(result.stop_reason, StopReason::ToolUse);
    assert_eq!(
        result.error_message.as_deref(),
        Some("transport error injected by faux provider")
    );
    let calls = tool_calls(&result);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "get_weather");
    assert_eq!(arguments(calls[0]), json!({"city": "Seoul"}));
    assert_eq!(calls[0].incomplete, None);
}
