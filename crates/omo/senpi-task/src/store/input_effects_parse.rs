//! `store/input-effects-parse.ts`: the persisted `uncertain_inputs` list.

use serde_json::Value;

use crate::runners::input_effects::{IdentifiedInput, InputCommandType};

pub fn parse_uncertain_inputs(
    value: Option<&Value>,
) -> Result<Option<Vec<IdentifiedInput>>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let items = value.as_array().ok_or("uncertain_inputs is not an array")?;
    items
        .iter()
        .map(|item| -> Result<IdentifiedInput, String> {
            let item = item.as_object().ok_or("invalid uncertain input")?;
            let string = |key: &str| {
                item.get(key)
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| format!("{key} is not a string"))
            };
            let kind = match item.get("type").and_then(Value::as_str) {
                Some("prompt") => InputCommandType::Prompt,
                Some("steer") => InputCommandType::Steer,
                Some("follow_up") => InputCommandType::FollowUp,
                _ => return Err("invalid uncertain input type".to_string()),
            };
            let streaming_behavior = match item.get("streamingBehavior") {
                None => None,
                Some(Value::String(behavior)) if behavior == "steer" || behavior == "followUp" => {
                    Some(behavior.clone())
                }
                Some(_) => return Err("invalid uncertain input behavior".to_string()),
            };
            let run_epoch = match item.get("run_epoch") {
                None => None,
                Some(epoch) => Some(epoch.as_i64().ok_or("run_epoch is not a number")?),
            };
            Ok(IdentifiedInput {
                kind,
                message: string("message")?,
                streaming_behavior,
                client_message_id: string("clientMessageId")?,
                run_epoch,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}
