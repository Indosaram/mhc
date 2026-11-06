use crate::support;
use maho_ext_api::*;
use maho_ext_mcp::elicitation::*;
use serde_json::json;
use std::{sync::Mutex, time::Duration};

type Answer = fn(&str) -> Option<String>;
/// Port of upstream `scriptedUi`: input answers "42" for count fields else "Ada"; select answers "Yes" for
/// the confirmed field else "beta". Records whether confirm was ever used (it must not be).
struct ScriptedUi {
    input: Answer,
    select: Answer,
    hanging: bool,
    confirmed: Mutex<bool>,
}
fn default_input(title: &str) -> Option<String> {
    Some(if title.contains("count") {
        "42".into()
    } else {
        "Ada".into()
    })
}
fn default_select(title: &str) -> Option<String> {
    Some(if title.contains("confirmed") {
        "Yes".into()
    } else {
        "beta".into()
    })
}
fn ui(input: Answer, select: Answer) -> ScriptedUi {
    ScriptedUi {
        input,
        select,
        hanging: false,
        confirmed: Mutex::new(false),
    }
}
impl ExtensionUi for ScriptedUi {
    fn select<'a>(
        &'a self,
        title: &'a str,
        _: &'a [String],
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async move { (self.select)(title) })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        *self.confirmed.lock().unwrap() = true;
        Box::pin(async { true })
    }
    fn input<'a>(
        &'a self,
        title: &'a str,
        _: Option<&'a str>,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async move {
            if self.hanging {
                return std::future::pending().await;
            }
            (self.input)(title)
        })
    }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err("custom UI must not be used".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}
fn schema() -> JsonValue {
    json!({"properties":{"confirmed":{"type":"boolean"},"count":{"type":"integer"},"mode":{"enum":["alpha","beta"],"type":"string"},"name":{"type":"string"}},"required":["name"]})
}
fn bool_schema() -> JsonValue {
    json!({"properties":{"confirmed":{"type":"boolean"}},"required":["confirmed"]})
}
async fn run(
    ui: &ScriptedUi,
    schema: &JsonValue,
) -> Result<ElicitationResponse, ElicitationRejection> {
    run_elicitation_form(ui, "Server asks", schema, Duration::from_secs(5)).await
}
fn cancel() -> ElicitationResponse {
    ElicitationResponse {
        action: ElicitationAction::Cancel,
        content: None,
    }
}
fn assert_processing_error(result: Result<ElicitationResponse, ElicitationRejection>) {
    let rejection = result.expect_err("processing error");
    assert_eq!(rejection.code, INTERNAL_ERROR);
    assert_eq!(rejection.to_json().get("data"), None);
}
#[test]
fn capability_is_an_empty_object() {
    assert_eq!(
        mcp_client_elicitation_capability(),
        json!({"elicitation":{}})
    );
    assert_eq!(mcp_client_capabilities(false), json!({}));
}
#[tokio::test]
async fn cancels_an_automated_task_parents_dialog_without_fabricating_boolean_values() {
    let ui = ui(default_input, |_| None);
    assert_eq!(run(&ui, &bool_schema()).await.unwrap(), cancel());
    assert!(!*ui.confirmed.lock().unwrap());
}
#[tokio::test]
async fn accepts_an_explicit_human_boolean_answer() {
    for (answer, expected) in [
        ("Yes", true),
        ("No", false),
        (" yes ", true),
        ("y", true),
        ("true", true),
        (" NO ", false),
        ("n", false),
        ("false", false),
    ] {
        thread_local!(static ANSWER:std::cell::RefCell<String>=const{std::cell::RefCell::new(String::new())});
        ANSWER.with(|cell| *cell.borrow_mut() = answer.into());
        let ui = ui(default_input, |_| {
            ANSWER.with(|cell| Some(cell.borrow().clone()))
        });
        let response = run(&ui, &bool_schema()).await.unwrap();
        assert_eq!(response.action, ElicitationAction::Accept, "{answer}");
        assert_eq!(response.content.unwrap()["confirmed"], expected, "{answer}");
    }
}
#[tokio::test]
async fn rejects_an_unrecognized_boolean_selection_without_field_values() {
    assert_processing_error(
        run(
            &ui(default_input, |_| Some("not an answer".into())),
            &json!({"properties":{"confirmed":{"type":"boolean"}}}),
        )
        .await,
    );
}
#[tokio::test]
async fn cancels_a_dismissed_dialog_even_without_required_fields() {
    for schema in [
        json!({"properties":{"name":{"type":"string"}}}),
        json!({"properties":{}}),
    ] {
        assert_eq!(
            run(&ui(|_| None, |_| None), &schema).await.unwrap(),
            cancel(),
            "{schema}"
        );
    }
}
#[tokio::test]
async fn cancels_a_dismissed_enum_dialog_without_returning_earlier_field_values() {
    let response=run_elicitation_form(&ui(default_input,|_|None),"Ask",&json!({"properties":{"name":{"type":"string"},"mode":{"type":"string","enum":["alpha","beta"]}}}),Duration::from_secs(5)).await;
    assert_eq!(response.unwrap(), cancel());
}
#[tokio::test]
async fn accepts_a_humans_explicit_submission_of_a_form_with_no_field_values() {
    let response = run(
        &ui(default_input, |_| Some("Submit".into())),
        &json!({"properties":{}}),
    )
    .await
    .unwrap();
    assert_eq!(
        response,
        ElicitationResponse {
            action: ElicitationAction::Accept,
            content: Some(Default::default())
        }
    );
}
#[tokio::test]
async fn accepts_with_typed_values_from_scripted_ui_answers() {
    let response = run(&ui(default_input, default_select), &schema())
        .await
        .unwrap();
    assert_eq!(response.action, ElicitationAction::Accept);
    let content = response.content.expect("accepted content");
    assert_eq!(content["count"], 42);
    assert_eq!(content["confirmed"], true);
    assert_eq!(content["mode"], "beta");
    assert_eq!(content["name"], "Ada");
}
#[tokio::test]
async fn numeric_answers_accept_javascript_radix_forms() {
    for (answer, expected) in [("0x10", 16.0), ("0b10", 2.0), ("0o10", 8.0)] {
        thread_local!(static ANSWER:std::cell::RefCell<String>=const{std::cell::RefCell::new(String::new())});
        ANSWER.with(|cell| *cell.borrow_mut() = answer.into());
        let response = run(
            &ui(
                |_| ANSWER.with(|cell| Some(cell.borrow().clone())),
                default_select,
            ),
            &json!({"properties":{"count":{"type":"number"}}}),
        )
        .await
        .unwrap();
        assert_eq!(response.action, ElicitationAction::Accept);
        assert_eq!(response.content.unwrap()["count"].as_f64(), Some(expected));
    }
}
#[tokio::test]
async fn cancels_when_a_required_dialog_is_dismissed_and_rejects_invalid_numbers() {
    assert_eq!(
        run_elicitation_form(
            &ui(|_| None, default_select),
            "Ask",
            &schema(),
            Duration::from_secs(5)
        )
        .await
        .unwrap(),
        cancel()
    );
    assert_processing_error(
        run_elicitation_form(
            &ui(
                |title| {
                    Some(if title.contains("count") {
                        "not-a-number".into()
                    } else {
                        "Ada".into()
                    })
                },
                default_select,
            ),
            "Ask",
            &schema(),
            Duration::from_secs(5),
        )
        .await,
    );
}
#[tokio::test]
async fn rejects_an_empty_required_answer_rather_than_fabricating_a_refusal() {
    assert_processing_error(
        run_elicitation_form(
            &ui(|_| Some(String::new()), default_select),
            "Ask",
            &json!({"properties":{"name":{"type":"string"}},"required":["name"]}),
            Duration::from_secs(5),
        )
        .await,
    );
}
#[tokio::test(start_paused = true)]
async fn cancels_via_the_bounded_timeout_without_wedging() {
    let ui = ScriptedUi {
        input: default_input,
        select: default_select,
        hanging: true,
        confirmed: Mutex::new(false),
    };
    assert_eq!(
        run_elicitation_form(&ui, "Ask", &schema(), Duration::from_millis(50))
            .await
            .unwrap(),
        cancel()
    );
}
#[tokio::test]
async fn headless_and_url_mode_requests_are_processing_errors() {
    let ui = ui(default_input, default_select);
    assert_processing_error(
        handle_mcp_elicitation(
            None,
            &json!({"requestedSchema":schema()}),
            Duration::from_secs(5),
        )
        .await,
    );
    assert_processing_error(
        handle_mcp_elicitation(
            Some(&ui),
            &json!({"url":"https://example.test"}),
            Duration::from_secs(5),
        )
        .await,
    );
}
#[tokio::test]
async fn stdio_server_request_with_colliding_client_id_reaches_native_form() {
    use maho_ext_mcp::{
        config_schema::{McpServerConfig, Transport},
        log::McpLogger,
        transport::*,
    };
    use std::sync::Arc;
    let root = tempfile::tempdir().unwrap();
    let script = r#"const readline=require('node:readline'); let initialize; const send=x=>process.stdout.write(JSON.stringify(x)+'\n'); readline.createInterface({input:process.stdin}).on('line',line=>{const x=JSON.parse(line); if(x.method==='server/discover'){send({jsonrpc:'2.0',id:x.id,error:{code:-32601,message:'Method not found'}});}else if(x.method==='initialize'){initialize=x.id;send({jsonrpc:'2.0',id:1,method:'elicitation/create',params:{message:'Ask',requestedSchema:{type:'object',properties:{name:{type:'string'}},required:['name']}}});}else if(x.id===1 && x.result){send({jsonrpc:'2.0',id:initialize,result:{protocolVersion:'2025-11-25',capabilities:{},serverInfo:{name:x.result.action+':'+x.result.content.name,version:'1'}}});}});"#;
    let config = McpServerConfig {
        transport: Some(Transport::Stdio),
        command: Some(support::node()),
        args: Some(vec!["-e".into(), script.into()]),
        ..Default::default()
    };
    let mut transport = create_mcp_transport(
        "elicitation",
        &config,
        None,
        Arc::new(std::sync::Mutex::new(
            McpLogger::new("elicitation", root.path(), None).unwrap(),
        )),
    )
    .unwrap();
    transport.elicitation_ui = Some(Arc::new(ui(default_input, default_select)));
    let client = connect_mcp_transport(&transport).await.unwrap();
    assert_eq!(client.server_info.read().await["name"], "accept:Ada");
    shutdown_mcp_transport(&transport).await.unwrap();
}
