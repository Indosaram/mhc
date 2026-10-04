use std::time::Duration;
use maho_ext_api::{ExtensionUi,ExtensionUiDialogOptions};
use serde::{Deserialize,Serialize};
use serde_json::{Map,Value,json};

pub const MCP_ELICITATION_TIMEOUT_MS:u64=300000;
pub fn mcp_client_elicitation_capability()->Value {json!({"elicitation":{}})}
#[derive(Debug,Clone,Copy,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum ElicitationAction {Accept,Decline,Cancel}
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub struct ElicitationResponse {
    pub action:ElicitationAction,
    #[serde(skip_serializing_if="Option::is_none")] pub content:Option<Map<String,Value>>,
}
fn response(action:ElicitationAction)->ElicitationResponse {ElicitationResponse {action,content:None}}
pub async fn handle_mcp_elicitation(ui:Option<&dyn ExtensionUi>,params:&Value,timeout:Duration)->ElicitationResponse {
    let (Some(ui),Some(schema))=(ui,params.get("requestedSchema")) else{return response(ElicitationAction::Decline);};
    run_elicitation_form(ui,params.get("message").and_then(Value::as_str).unwrap_or(""),schema,timeout).await
}
pub async fn run_elicitation_form(ui:&dyn ExtensionUi,message:&str,schema:&Value,timeout:Duration)->ElicitationResponse {
    match tokio::time::timeout(timeout,collect_form(ui,message,schema)).await {
        Ok(result)=>result,
        Err(_)=>response(ElicitationAction::Cancel),
    }
}
async fn collect_form(ui:&dyn ExtensionUi,message:&str,schema:&Value)->ElicitationResponse {
    let mut content=Map::new();
    let Some(properties)=schema.get("properties").and_then(Value::as_object) else{return ElicitationResponse {action:ElicitationAction::Accept,content:Some(content)};};
    for (name,property) in properties {
        let title=format!("{message} — {}",property.get("title").and_then(Value::as_str).unwrap_or(name));
        let description=property.get("description").and_then(Value::as_str);
        let kind=property.get("type").and_then(Value::as_str);
        if kind==Some("boolean") {
            content.insert(name.clone(),json!(ui.confirm(&title,description.unwrap_or(name),ExtensionUiDialogOptions::default()).await));continue;
        }
        if let Some(options)=property.get("enum").and_then(Value::as_array).filter(|options|!options.is_empty()) {
            let options=options.iter().map(|value|value.as_str().map_or_else(||value.to_string(),str::to_owned)).collect::<Vec<_>>();
            let Some(picked)=ui.select(&title,&options,ExtensionUiDialogOptions::default()).await else{return response(ElicitationAction::Decline);};
            content.insert(name.clone(),json!(picked));continue;
        }
        let answer=ui.input(&title,description,ExtensionUiDialogOptions::default()).await;
        let Some(answer)=answer.filter(|answer|!answer.is_empty()) else{
            if schema.get("required").and_then(Value::as_array).is_some_and(|required|required.iter().any(|value|value.as_str()==Some(name))) {return response(ElicitationAction::Decline);}
            continue;
        };
        let value=if matches!(kind,Some("number"|"integer")) {
            let raw=answer.trim();
            let radix=raw.strip_prefix("0x").or_else(||raw.strip_prefix("0X")).map(|digits|(digits,16))
                .or_else(||raw.strip_prefix("0b").or_else(||raw.strip_prefix("0B")).map(|digits|(digits,2)))
                .or_else(||raw.strip_prefix("0o").or_else(||raw.strip_prefix("0O")).map(|digits|(digits,8)));
            let number=if raw.is_empty(){Some(0.0)}else if let Some((digits,radix))=radix {
                digits.chars().try_fold(0.0,|number,digit|digit.to_digit(radix).map(|digit|number*f64::from(radix)+f64::from(digit))).filter(|_|!digits.is_empty())
            }else{raw.parse::<f64>().ok()};
            let Some(number)=number.filter(|number|number.is_finite()) else{return response(ElicitationAction::Decline);};
            json!(number)
        }else{json!(answer)};
        content.insert(name.clone(),value);
    }
    ElicitationResponse {action:ElicitationAction::Accept,content:Some(content)}
}
