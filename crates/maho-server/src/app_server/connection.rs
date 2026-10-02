use serde_json::{Value,json};
use std::collections::BTreeSet;

pub fn parse_initialize_params(value:&Value)->Option<Value> {
    let params=value.as_object()?;let client=params.get("clientInfo")?.as_object()?;
    let name=client.get("name")?.as_str().filter(|s|!s.is_empty())?;
    let version=client.get("version")?.as_str().filter(|s|!s.is_empty())?;
    let title=match client.get("title") {None|Some(Value::Null)=>Value::Null,Some(Value::String(title))=>json!(title),_=>return None};
    let capabilities=match params.get("capabilities") {
        None|Some(Value::Null)=>Value::Null,
        Some(Value::Object(capabilities))=>{
            let mut normalized=json!({"experimentalApi":false,"requestAttestation":false});
            for key in ["experimentalApi","requestAttestation","mcpServerOpenaiFormElicitation"] {
                if let Some(value)=capabilities.get(key) {normalized[key]=json!(value.as_bool()?);}
            }
            if let Some(value)=capabilities.get("optOutNotificationMethods").filter(|v|!v.is_null()) {
                let methods=value.as_array()?;if methods.iter().any(|v|!v.is_string()) {return None;}normalized["optOutNotificationMethods"]=value.clone();
            }
            normalized
        },
        _=>return None,
    };
    Some(json!({"clientInfo":{"name":name,"title":title,"version":version},"capabilities":capabilities}))
}
#[derive(Default)]
pub struct InitializedConnection {
    pub state:Option<Value>,
    pub opt_out_notification_methods:BTreeSet<String>,
}
impl InitializedConnection {
    pub fn can_deliver_notification(&self, method: &str) -> bool {
        use super::methods::{ADDITIVE_SERVER_NOTIFICATION_METHODS, EXPERIMENTAL_SERVER_NOTIFICATION_METHODS, SERVER_NOTIFICATION_METHODS};
        self.state.is_some()
            && (SERVER_NOTIFICATION_METHODS.contains(&method) || ADDITIVE_SERVER_NOTIFICATION_METHODS.contains(&method))
            && !self.opt_out_notification_methods.contains(method)
            && (!EXPERIMENTAL_SERVER_NOTIFICATION_METHODS.contains(&method) || self.registry_connection().experimental_api)
    }
    pub fn initialize_response(&self, agent_home: &str, platform: &str) -> Result<Value, super::errors::JsonRpcError> {
        let state = self.state.as_ref().ok_or_else(|| super::errors::JsonRpcError::new(-32600, "Connection is not initialized"))?;
        let (family, os) = match platform {
            "win32" => ("windows", "windows"),
            "darwin" => ("unix", "macos"),
            _ => ("unix", "linux"),
        };
        Ok(json!({"userAgent":state["userAgent"],"codexHome":agent_home,"platformFamily":family,"platformOs":os}))
    }
    pub fn capabilities(&self)->Value {self.state.as_ref().map(|state|state["capabilities"].clone()).unwrap_or_else(||json!({"experimentalApi":false,"requestAttestation":false}))}
    pub fn initialize(&mut self,params:&Value,server_version:&str,os_type:&str,os_release:&str,arch:&str)->bool {
        if self.state.is_some() {return false;}
        let capabilities=if params["capabilities"].is_null() {json!({"experimentalApi":false,"requestAttestation":false})} else {params["capabilities"].clone()};
        self.opt_out_notification_methods.clear();
        if let Some(methods)=capabilities["optOutNotificationMethods"].as_array() {self.opt_out_notification_methods.extend(methods.iter().filter_map(Value::as_str).map(str::to_owned));}
        let name=params["clientInfo"]["name"].as_str().unwrap_or_default();
        self.state=Some(json!({"initialized":true,"clientInfo":params["clientInfo"],"capabilities":capabilities,"userAgent":format!("{name}/{server_version} ({os_type} {os_release}; {arch}) senpi_app_server")}));true
    }
    pub fn registry_connection(&self)->super::registry::RegistryConnection {
        super::registry::RegistryConnection {initialized:self.state.is_some(),experimental_api:self.capabilities()["experimentalApi"].as_bool().unwrap_or(false),..Default::default()}
    }
}
