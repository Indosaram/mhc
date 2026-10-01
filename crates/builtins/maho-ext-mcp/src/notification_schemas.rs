use serde_json::{Map,Value};
#[derive(Debug,Clone,PartialEq)]
pub enum McpNotification {
    ListChanged {method:String,params:Option<Value>},
    ResourceUpdated {uri:String,params:Value},
    LoggingMessage {level:String,logger:Option<String>,data:Option<Value>,params:Value},
}
pub fn parse_notification(value:&Value)->Option<McpNotification> {
    let method=value.get("method")?.as_str()?;
    let raw=value.get("params");
    let params=match raw {None=>None,Some(Value::Object(map))=>Some(map),_=>return None};
    let mut stripped=Map::new();
    if let Some(meta)=params.and_then(|p|p.get("_meta")){if !meta.is_object(){return None;}stripped.insert("_meta".into(),meta.clone());}
    match method {
        "notifications/tools/list_changed"|"notifications/resources/list_changed"|"notifications/prompts/list_changed"=>Some(McpNotification::ListChanged {method:method.into(),params:params.map(|_|Value::Object(stripped))}),
        "notifications/resources/updated"=>{let uri=params?.get("uri")?.as_str()?.to_owned();stripped.insert("uri".into(),Value::String(uri.clone()));Some(McpNotification::ResourceUpdated {uri,params:Value::Object(stripped)})}
        "notifications/message"=>{
            let params=params?;let level=params.get("level")?.as_str()?;
            if !["debug","info","notice","warning","error","critical","alert","emergency"].contains(&level){return None;}
            let logger=match params.get("logger"){None=>None,Some(value)=>Some(value.as_str()?.to_owned())};let data=params.get("data").cloned();
            stripped.insert("level".into(),Value::String(level.into()));
            if let Some(logger)=&logger{stripped.insert("logger".into(),Value::String(logger.clone()));}
            if let Some(data)=&data{stripped.insert("data".into(),data.clone());}
            Some(McpNotification::LoggingMessage {level:level.into(),logger,data,params:Value::Object(stripped)})
        }
        _=>None,
    }
}
