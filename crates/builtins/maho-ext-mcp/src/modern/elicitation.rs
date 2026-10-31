//! Port of senpi mcp/modern/elicitation.ts: the private MRTR form walked through ephemeral host dialogs.
use super::{
    errors::{McpMrtrError, McpMrtrErrorCode},
    form_schema::{Property, enums, valid_value, validate_mrtr_form},
};
use crate::wrap::McpLinkedSignal;
use maho_core::mcp_mrtr_presentations::McpMrtrPresentation;
use maho_ext_api::{AbortSignal, ExtensionUiDialogOptions};
use serde_json::{Map, Value};

/// `{action:"accept",content}` or `{action:"decline"|"cancel"}` on the wire.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum MrtrInputResponse {
    Accept { content: Map<String, Value> },
    Decline,
    Cancel,
}
impl MrtrInputResponse {
    pub fn accepted(&self) -> bool {
        matches!(self, Self::Accept { .. })
    }
}

static VT_SEQUENCE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?:\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|[\x1b\x{9b}][\[\]()#;?]*(?:\d{1,4}(?:[;:]\d{0,4})*)?[\dA-PR-TZcf-nq-uy=><~])")
        .expect("vt pattern")
});
/// Upstream `inert`: `stripVTControlCharacters` then drop the remaining C0/C1 controls except `\t` and `\n`.
pub fn inert(value: &str) -> String {
    VT_SEQUENCE
        .replace_all(value, "")
        .chars()
        .filter(|c| !matches!(*c as u32, 0x00..=0x08 | 0x0b..=0x1f | 0x7f..=0x9f))
        .collect()
}

fn as_text(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

struct Form<'a> {
    presentation: &'a McpMrtrPresentation,
    signal: AbortSignal,
    presented: maho_ext_api::DialogPresentedKey,
    heading: String,
    required: Vec<String>,
}
impl Form<'_> {
    /// Private dialogs: no generic blocked event or pending marker (core-ext `ephemeral`).
    fn options(&self) -> ExtensionUiDialogOptions {
        ExtensionUiDialogOptions {
            ephemeral: true,
            signal: Some(self.signal.clone()),
            presented: Some(self.presented.clone()),
            ..Default::default()
        }
    }
    async fn select(&self, title: &str, choices: &[String]) -> Option<String> {
        if self.signal.is_aborted() {
            return None;
        }
        let result = self
            .presentation
            .ui
            .select(title, choices, self.options())
            .await;
        if self.signal.is_aborted() {
            None
        } else {
            result
        }
    }
    async fn field(
        &self,
        content: &mut Map<String, Value>,
        name: &str,
        property: &Property,
    ) -> bool {
        let label = property
            .get("title")
            .map_or_else(|| name.to_owned(), as_text);
        let description = property
            .get("description")
            .and_then(Value::as_str)
            .map_or_else(String::new, |text| format!("\n{}", inert(text)));
        let title = format!("{}\n{}{description}", self.heading, inert(&label));
        loop {
            let mut actions = vec!["Enter value".to_owned()];
            if !self.required.iter().any(|key| key == name) {
                actions.push("Leave unset".into());
            }
            if property.contains_key("default") {
                actions.push("Use default".into());
            }
            // Primitive selectors already require an explicit choice; do not conflate false with dismissal.
            let mut action = "Enter value".to_owned();
            if actions.len() > 1 {
                let Some(chosen) = self.select(&title, &actions).await else {
                    return false;
                };
                action = chosen;
            }
            if action == "Leave unset" {
                content.remove(name);
                return true;
            }
            if action == "Use default" {
                content.insert(name.into(), property["default"].clone());
                return true;
            }
            let kind = property.get("type").and_then(Value::as_str);
            let value = if kind == Some("boolean") {
                let Some(chosen) = self
                    .select(&title, &["true".to_owned(), "false".to_owned()])
                    .await
                else {
                    return false;
                };
                Value::Bool(chosen == "true")
            } else if kind == Some("array") || enums(property).is_some() {
                let choices = if kind == Some("array") {
                    property
                        .get("items")
                        .and_then(Value::as_object)
                        .and_then(enums)
                } else {
                    enums(property)
                };
                let Some(choices) = choices else {
                    return false;
                };
                let mut values: Vec<String> = Vec::new();
                loop {
                    let mut labels = choices
                        .iter()
                        .enumerate()
                        .map(|(index, choice)| {
                            let selected = if values.contains(&choice.value) {
                                " [selected]"
                            } else {
                                ""
                            };
                            format!("{}. {}{selected}", index + 1, inert(&choice.label))
                        })
                        .collect::<Vec<_>>();
                    if kind == Some("array") {
                        labels.push("Done".into());
                    }
                    let Some(chosen) = self.select(&title, &labels).await else {
                        return false;
                    };
                    if chosen == "Done" {
                        break Value::from(values);
                    }
                    let Some(choice) = labels
                        .iter()
                        .position(|label| *label == chosen)
                        .and_then(|index| choices.get(index))
                    else {
                        continue;
                    };
                    if kind != Some("array") {
                        break Value::String(choice.value.clone());
                    }
                    match values.iter().position(|value| *value == choice.value) {
                        Some(existing) => {
                            values.remove(existing);
                        }
                        None => values.push(choice.value.clone()),
                    }
                }
            } else {
                if self.signal.is_aborted() {
                    return false;
                }
                let text = self
                    .presentation
                    .ui
                    .input(&title, None, self.options())
                    .await;
                let Some(text) = text.filter(|_| !self.signal.is_aborted()) else {
                    return false;
                };
                if matches!(kind, Some("number" | "integer")) {
                    let trimmed = maho_ai::utils::js::trim(&text);
                    let number = if trimmed.is_empty() {
                        f64::NAN
                    } else {
                        maho_ai::utils::js::string_to_number(&text)
                    };
                    maho_ai::utils::js::json_number(number)
                } else {
                    Value::String(text)
                }
            };
            if valid_value(property, &value) {
                content.insert(name.into(), value);
                return true;
            }
            let retry = self
                .select(
                    &format!("{title}\nValue does not satisfy this field."),
                    &["Try again".to_owned(), "Cancel".to_owned()],
                )
                .await;
            if retry.as_deref() != Some("Try again") {
                return false;
            }
        }
    }
}

/// No model-facing questions, RPC requests, timers, persistence, or network operations.
pub async fn collect_mrtr_form(
    presentation: &McpMrtrPresentation,
    server_label: &str,
    message: &str,
    requested_schema: &Value,
    signal: &AbortSignal,
) -> Result<MrtrInputResponse, McpMrtrError> {
    if !validate_mrtr_form(requested_schema) {
        return Err(McpMrtrError::new(McpMrtrErrorCode::InvalidInput));
    }
    let local = AbortSignal::default();
    let combined = McpLinkedSignal::with_host(
        vec![signal.clone(), local.clone()],
        vec![presentation.signal.clone()],
    );
    // senpi bindMcpFormPresented(opts, ...): the callback is keyed by the dialog options; every dialog of this
    // form carries the same key in `ExtensionUiDialogOptions::presented`, which the host marks once on screen.
    let presented_key: std::sync::Arc<dyn std::any::Any + Send + Sync> = std::sync::Arc::new(());
    if let Some(presented) = presentation.presented.clone() {
        crate::form_types::bind_mcp_form_presented(
            &presented_key,
            Some(Box::new(move || presented())),
        );
    }
    let empty = Map::new();
    let properties = requested_schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let form = Form {
        presentation,
        signal: combined.signal.clone(),
        presented: maho_ext_api::DialogPresentedKey(presented_key.clone()),
        heading: format!("MCP server: {}\n{}", inert(server_label), inert(message)),
        required: requested_schema
            .get("required")
            .and_then(Value::as_array)
            .map(|keys| {
                keys.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
    };
    let finish = |result: MrtrInputResponse| {
        if !form.signal.is_aborted()
            && let Some(admit) = &presentation.admit
        {
            admit();
        }
        result
    };
    let entries = properties
        .iter()
        .filter_map(|(name, property)| {
            property
                .as_object()
                .map(|property| (name.clone(), property.clone()))
        })
        .collect::<Vec<_>>();
    let mut content = Map::new();
    let result = async {
        let begin = form
            .select(
                &form.heading,
                &[
                    "Continue".to_owned(),
                    "Decline".to_owned(),
                    "Cancel".to_owned(),
                ],
            )
            .await;
        if begin.as_deref() != Some("Continue") {
            return finish(if begin.as_deref() == Some("Decline") {
                MrtrInputResponse::Decline
            } else {
                MrtrInputResponse::Cancel
            });
        }
        for (name, property) in &entries {
            if !form.field(&mut content, name, property).await {
                return finish(MrtrInputResponse::Cancel);
            }
        }
        loop {
            let review = entries
                .iter()
                .map(|(name, _)| {
                    let shown = content
                        .get(name)
                        .map_or_else(|| "(unset)".to_owned(), |value| inert(&value.to_string()));
                    format!("{}: {shown}", inert(name))
                })
                .collect::<Vec<_>>()
                .join("\n");
            let action = form
                .select(
                    &format!("{}\nReview\n{review}", form.heading),
                    &[
                        "Submit".to_owned(),
                        "Edit field".to_owned(),
                        "Decline".to_owned(),
                        "Cancel".to_owned(),
                    ],
                )
                .await;
            if action.as_deref() == Some("Submit") && !form.signal.is_aborted() {
                return finish(MrtrInputResponse::Accept {
                    content: content.clone(),
                });
            }
            if action.as_deref() != Some("Edit field") {
                return finish(if action.as_deref() == Some("Decline") {
                    MrtrInputResponse::Decline
                } else {
                    MrtrInputResponse::Cancel
                });
            }
            let labels = entries
                .iter()
                .enumerate()
                .map(|(index, (name, _))| format!("{}. {}", index + 1, inert(name)))
                .collect::<Vec<_>>();
            let chosen = form
                .select(&format!("{}\nEdit field", form.heading), &labels)
                .await;
            let entry = chosen
                .and_then(|chosen| labels.iter().position(|label| *label == chosen))
                .and_then(|index| entries.get(index));
            let Some((name, property)) = entry else {
                return finish(MrtrInputResponse::Cancel);
            };
            if !form.field(&mut content, name, property).await {
                return finish(MrtrInputResponse::Cancel);
            }
        }
    }
    .await;
    crate::form_types::bind_mcp_form_presented(&presented_key, None);
    local.abort();
    content.clear();
    Ok(result)
}
