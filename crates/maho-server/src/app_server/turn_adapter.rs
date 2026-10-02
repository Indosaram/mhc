use super::registry::JsonRpcError;
use serde_json::{Value,json};

pub struct TurnInputParams {
    pub thread_id: String,
    pub expected_turn_id: Option<String>,
    pub client_user_message_id: Option<String>,
    pub input: Vec<Value>,
}
fn required_string<'a>(value: &'a Value,name: &str) -> Result<&'a str,JsonRpcError> {
    value.as_str().filter(|value|!value.is_empty()).ok_or_else(||JsonRpcError::new(-32602,format!("Invalid params: {name} is required")))
}
fn object_params(request: &Value) -> Result<&Value,JsonRpcError> {
    let params = &request["params"];
    if !params.is_object() {return Err(JsonRpcError::new(-32602,"Invalid params"));}
    Ok(params)
}
pub fn turn_input_params(request: &Value,steer: bool) -> Result<TurnInputParams,JsonRpcError> {
    let params = object_params(request)?;
    let thread_id = required_string(&params["threadId"],"threadId")?.to_owned();
    let expected_turn_id = if steer {Some(required_string(&params["expectedTurnId"],"expectedTurnId")?.to_owned())} else {None};
    let client_user_message_id = match params.get("clientUserMessageId") {
        None|Some(Value::Null)=>None,
        Some(Value::String(value))=>Some(value.clone()),
        _=>return Err(JsonRpcError::new(-32602,"Invalid params: clientUserMessageId must be a string")),
    };
    let input = params["input"].as_array().ok_or_else(||JsonRpcError::new(-32602,"Invalid params: input must be an array"))?;
    let input = input.iter().map(|item| {
        if !item.is_object() {return Err(JsonRpcError::new(-32602,"Invalid params: input item must be an object"));}
        match item["type"].as_str() {
            Some("text")=>Ok(json!({"type":"text","text":required_string(&item["text"],"input.text")?,"text_elements":item["text_elements"].as_array().cloned().unwrap_or_default()})),
            Some("image")=>Ok(json!({"type":"image","url":required_string(&item["url"],"input.url")?})),
            Some("localImage")=>Ok(json!({"type":"localImage","path":required_string(&item["path"],"input.path")?})),
            Some(kind @ ("skill"|"mention"))=>Ok(json!({"type":kind,"name":required_string(&item["name"],"input.name")?,"path":required_string(&item["path"],"input.path")?})),
            _=>Err(JsonRpcError::new(-32602,"Invalid params: unsupported input item type")),
        }
    }).collect::<Result<Vec<_>,_>>()?;
    Ok(TurnInputParams {thread_id,expected_turn_id,client_user_message_id,input})
}
pub fn turn_interrupt_params(request: &Value) -> Result<(String,String),JsonRpcError> {
    let params = object_params(request)?;
    Ok((required_string(&params["threadId"],"threadId")?.into(),required_string(&params["turnId"],"turnId")?.into()))
}
