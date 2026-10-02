use std::{collections::BTreeMap, path::Path};
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use crate::config_schema::*;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct McpConfigValidationError(pub String);
pub fn validate_raw(raw: Value) -> Result<RawConfig, McpConfigValidationError> {
    validate_schema_value(&raw,"$")?;
    let config: RawConfig = serde_path_to_error::deserialize(raw).map_err(|e| {
        let path = e.path().to_string();
        let message = if e.inner().to_string().contains("expected a sequence") { "Expected array".into() } else { e.inner().to_string() };
        McpConfigValidationError(format!("Invalid MCP config at {}: {message}", if path == "." { "$" } else { &path }))
    })?;
    if let Some(error) = get_server_endpoint_validation_error(&config) { return Err(McpConfigValidationError(format!("Invalid MCP config at {error}"))); }
    if let Some(settings) = &config.settings {
        if settings.import_configs.as_ref().is_some_and(|items| items.iter().any(|s| s != "claude")) {
            return Err(McpConfigValidationError("Invalid MCP config at settings.importConfigs".into()));
        }
        if matches!(&settings.native_tool_search, Some(NativeToolSearch::Auto(s)) if s != "auto") {
            return Err(McpConfigValidationError("Invalid MCP config at settings.nativeToolSearch".into()));
        }
    }
    for (name, server) in config.mcp_servers.iter().flat_map(|m| m.iter()) {
        if matches!(server.auth, Some(Auth::Disabled(true))) { return Err(McpConfigValidationError(format!("Invalid MCP config at mcpServers.{name}.auth"))); }
    }
    Ok(config)
}
fn schema_error(path:&str,message:&str)->McpConfigValidationError {McpConfigValidationError(format!("Invalid MCP config at {path}: {message}"))}
fn validate_schema_value(raw:&Value,path:&str)->Result<(),McpConfigValidationError> {
    let object=raw.as_object().ok_or_else(||schema_error(path,"must be object"))?;
    for name in object.keys(){if !["mcpServers","settings"].contains(&name.as_str()){return Err(schema_error(name,"schema is false"));}}
    for (name,value) in object {
        match name.as_str() {
            "mcpServers"=>{
                let servers=value.as_object().ok_or_else(||schema_error("mcpServers","must be object"))?;
                for (name,server) in servers {
                    let path=format!("mcpServers.{name}");let server=server.as_object().ok_or_else(||schema_error(&path,"must be object"))?;
                    for name in server.keys(){if !["type","command","args","cwd","env","url","headers","bearerTokenEnv","auth","oauth","enabled","lifecycle","connectTimeoutMs","requestTimeoutMs","startupTimeoutMs","idleTimeoutMin","exposure","directTools","includeTools","excludeTools","logLevel"].contains(&name.as_str()){return Err(schema_error(&format!("{path}.{name}"),"schema is false"));}}
                    for (field,value) in server {
                        let path=format!("{path}.{field}");
                        match field.as_str() {
                            "command"|"cwd"|"url"|"bearerTokenEnv"=>check_type(value,&path,"string")?,
                            "enabled"=>check_type(value,&path,"boolean")?,
                            "connectTimeoutMs"|"requestTimeoutMs"|"startupTimeoutMs"|"idleTimeoutMin"=>check_type(value,&path,"number")?,
                            "args"|"includeTools"|"excludeTools"=>check_strings(value,&path)?,
                            "env"|"headers"=>{let map=value.as_object().ok_or_else(||schema_error(&path,"must be object"))?;for (name,value) in map {check_type(value,&format!("{path}.{name}"),"string")?;}}
                            "type"=>check_literals(value,&path,&["stdio","http"] )?,
                            "lifecycle"=>check_literals(value,&path,&["lazy","eager","keep-alive"] )?,
                            "exposure"=>check_literals(value,&path,&["auto","direct","search","proxy"] )?,
                            "logLevel"=>check_literals(value,&path,&["debug","info","notice","warning","error","critical","alert","emergency"] )?,
                            "auth" if value!=&Value::Bool(false)=>check_literals(value,&path,&["bearer","oauth"] )?,
                            "directTools" if value.as_array().is_none_or(|array|array.iter().any(|value|!value.is_string()))=>check_type(value,&path,"boolean")?,
                            "oauth"=>{
                                let oauth=value.as_object().ok_or_else(||schema_error(&path,"must be object"))?;
                                for name in oauth.keys() {if !["clientId","callbackPort","scopes","clientMetadataUrl","flow"].contains(&name.as_str()){return Err(schema_error(&format!("{path}.{name}"),"schema is false"));}}
                                for (name,value) in oauth {let path=format!("{path}.{name}");match name.as_str(){
                                    "clientId"|"clientMetadataUrl"=>check_type(value,&path,"string")?,"scopes"=>check_strings(value,&path)?,
                                    "flow"=>check_literals(value,&path,&["code","client_credentials"] )?,
                                    "callbackPort"=>{
                                        check_type(value,&path,"integer")?;let number=value.as_f64().unwrap_or(0.0);
                                        if number<0.0{return Err(schema_error(&path,"must be >= 0"));}
                                        if number>65535.0{return Err(schema_error(&path,"must be <= 65535"));}
                                    },_=>(),
                                }}
                            }
                            _=>(),
                        }
                    }
                }
            }
            "settings"=>{
                let settings=value.as_object().ok_or_else(||schema_error("settings","must be object"))?;
                for name in settings.keys(){if !["toolPrefix","searchThreshold","outputGuard","importConfigs","oauthCallbackUrl","stubSwap","nativeToolSearch"].contains(&name.as_str()){return Err(schema_error(&format!("settings.{name}"),"schema is false"));}}
                for (field,value) in settings {let path=format!("settings.{field}");match field.as_str(){
                    "toolPrefix"|"oauthCallbackUrl"=>check_type(value,&path,"string")?,"searchThreshold"=>check_type(value,&path,"number")?,"stubSwap"=>check_type(value,&path,"boolean")?,
                    "nativeToolSearch" if value!=&Value::String("auto".into()) && !value.is_boolean()=>return Err(schema_error(&path,if value.is_string(){"must be equal to constant"}else{"must be string"})),
                    "importConfigs"=>{let array=value.as_array().ok_or_else(||schema_error(&path,"Expected array"))?;for (index,value) in array.iter().enumerate(){let path=format!("{path}.{index}");check_type(value,&path,"string")?;if value!=&Value::String("claude".into()){return Err(schema_error(&path,"must be equal to constant"));}}},
                    "outputGuard"=>{let guard=value.as_object().ok_or_else(||schema_error(&path,"must be object"))?;for field in guard.keys(){if !["maxBytes","maxLines","maxTokens"].contains(&field.as_str()){return Err(schema_error(&format!("{path}.{field}"),"schema is false"));}}for (field,value) in guard {check_type(value,&format!("{path}.{field}"),"number")?;}},_=>(),
                }}
            }
            _=>(),
        }
    }
    Ok(())
}
fn check_type(value:&Value,path:&str,kind:&str)->Result<(),McpConfigValidationError> {
    let valid=match kind {"string"=>value.is_string(),"boolean"=>value.is_boolean(),"number"=>value.is_number(),"integer"=>value.as_f64().is_some_and(|number|number.fract()==0.0),_=>false};
    if valid{Ok(())}else{Err(schema_error(path,&format!("must be {kind}")))}
}
fn check_strings(value:&Value,path:&str)->Result<(),McpConfigValidationError> {
    let array=value.as_array().ok_or_else(||schema_error(path,"Expected array"))?;
    for (index,value) in array.iter().enumerate(){check_type(value,&format!("{path}.{index}"),"string")?;}Ok(())
}
fn check_literals(value:&Value,path:&str,literals:&[&str])->Result<(),McpConfigValidationError> {
    if value.as_str().is_some_and(|value|literals.contains(&value)){return Ok(());}
    check_type(value,path,"string")?;Err(schema_error(path,"must be equal to constant"))
}
pub fn validate_mcp_server_declaration(name: &str, raw: Value) -> Option<String> {
    let wrapped=serde_json::json!({"mcpServers": {name: raw}});
    if let Err(error)=validate_schema_value(&wrapped,"$") {
        let error=error.0.trim_start_matches("Invalid MCP config at ");let prefix=format!("mcpServers.{name}");
        let error=error.strip_prefix(&format!("{prefix}.")).map(str::to_owned).or_else(||error.strip_prefix(&prefix).map(|error|format!("${error}"))).unwrap_or_else(||error.into());
        return Some(format!("Invalid MCP server declaration \"{name}\": {error}"));
    }
    validate_raw(wrapped).err().map(|error|format!("Invalid MCP server declaration \"{name}\": {}",error.0.trim_start_matches("Invalid MCP config at ")))
}
pub fn normalize_server(mut server: ServerConfigWire) -> McpServerConfig {
    server.transport.get_or_insert(if server.url.as_ref().is_some_and(|v| !v.is_empty()) { Transport::Http } else { Transport::Stdio });
    server.args.get_or_insert_with(Vec::new);
    server.connect_timeout_ms.get_or_insert(15000.0);
    server.enabled.get_or_insert(true);
    server.exposure.get_or_insert(Exposure::Auto);
    server.idle_timeout_min.get_or_insert(10.0);
    server.lifecycle.get_or_insert(Lifecycle::Lazy);
    server.log_level.get_or_insert(LogLevel::Info);
    server.request_timeout_ms.get_or_insert(30000.0);
    server.startup_timeout_ms.get_or_insert(250.0);
    server
}
pub fn hash_config(config: &McpServerConfig) -> Result<String, McpConfigValidationError> {
    let mut value = serde_json::to_value(config).map_err(|e| McpConfigValidationError(e.to_string()))?;
    if let Some(obj) = value.as_object_mut() { obj.remove("startupTimeoutMs"); }
    Ok(format!("{:x}", Sha256::digest(stable_stringify(&value))))
}
fn stable_stringify(value: &Value) -> String {
    match value {
        Value::Array(items) => format!("[{}]", items.iter().map(stable_stringify).collect::<Vec<_>>().join(",")),
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect(); keys.sort();
            format!("{{{}}}", keys.iter().map(|key| format!("{}:{}", Value::String((*key).clone()), stable_stringify(&map[*key]))).collect::<Vec<_>>().join(","))
        }
        Value::Number(n) if n.as_f64().is_some_and(|f| f.fract() == 0.0) => format!("{:.0}", n.as_f64().unwrap_or_default()),
        _ => value.to_string(),
    }
}
pub fn interpolate_value(value: &Value, path: &str, env: &BTreeMap<String, String>) -> Result<Value, McpConfigValidationError> {
    match value {
        Value::String(s) => {
            if s.trim_start().starts_with('!') || s.contains("$(") {
                return Err(McpConfigValidationError(format!("MCP config interpolation rejected command substitution at {}: config values may reference only environment variables for security; shell commands are never executed.", path.strip_prefix("mcp.").unwrap_or(path))));
            }
            let re = Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)(:-([^}]*))?\}").map_err(|e| McpConfigValidationError(e.to_string()))?;
            Ok(Value::String(re.replace_all(s, |c: &regex::Captures<'_>| env.get(&c[1]).cloned().unwrap_or_else(|| c.get(3).map_or("", |m| m.as_str()).into())).into_owned()))
        }
        Value::Array(items) => items.iter().enumerate().map(|(i, v)| interpolate_value(v, &format!("{path}.{i}"), env)).collect::<Result<Vec<_>, _>>().map(Value::Array),
        Value::Object(map) => map.iter().map(|(k, v)| Ok((k.clone(), interpolate_value(v, &format!("{path}.{k}"), env)?))).collect::<Result<serde_json::Map<_, _>, _>>().map(Value::Object),
        _ => Ok(value.clone()),
    }
}
fn read_config(path: &Path, trusted: bool, diagnostics: &mut Vec<String>) -> Result<Option<Value>, McpConfigValidationError> {
    if !path.exists() { return Ok(None); }
    let result = std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()));
    match result {
        Ok(v) => Ok(Some(v)),
        Err(reason) if trusted => Err(McpConfigValidationError(format!("Invalid MCP config at {}: {reason}", path.display()))),
        Err(reason) => { diagnostics.push(format!("Blocked untrusted MCP config at {}: invalid JSON ({reason})", path.display())); Ok(None) }
    }
}
fn trusted_config(raw: Option<Value>, env: &BTreeMap<String, String>) -> Result<Option<RawConfig>, McpConfigValidationError> {
    raw.map(|raw| {
        let original=validate_raw(raw.clone())?;
        let mut resolved=validate_raw(interpolate_value(&raw,"mcp",env)?)?;
        if let (Some(original),Some(resolved))=(original.mcp_servers,resolved.mcp_servers.as_mut()) {
            for (name,target) in resolved {if let Some(source)=original.get(name){crate::sharing_policy::inherit_mcp_sharing_scope(source,target);}}
        }
        Ok(resolved)
    }).transpose()
}
pub fn load_mcp_config(options: LoadMcpConfigOptions<'_>) -> Result<ResolvedMcpConfig, McpConfigValidationError> {
    let global_path = options.agent_dir.join("mcp.json");
    let project_path = options.cwd.join(".maho/mcp.json");
    let claude_path = options.cwd.join(".mcp.json");
    let mut diagnostics = Vec::new();
    let global = trusted_config(read_config(&global_path, true, &mut diagnostics)?, options.env)?;
    let project_raw = read_config(&project_path, options.project_trusted, &mut diagnostics)?;
    let project = if options.project_trusted { trusted_config(project_raw.clone(), options.env)? } else { None };
    let preliminary = merge_settings(global.as_ref().and_then(|c| c.settings.as_ref()), project.as_ref().and_then(|c| c.settings.as_ref()))?;
    let import = preliminary.import_configs.as_ref().is_some_and(|i| i.iter().any(|v| v == "claude"));
    let claude_raw = if import { read_config(&claude_path, options.project_trusted, &mut diagnostics)? } else { None };
    let claude = if import && options.project_trusted { trusted_config(claude_raw.clone(), options.env)? } else { None };
    let settings = merge_settings(Some(&merge_settings(global.as_ref().and_then(|c| c.settings.as_ref()), claude.as_ref().and_then(|c| c.settings.as_ref()))?), project.as_ref().and_then(|c| c.settings.as_ref()))?;
    let mut result = ResolvedMcpConfig { settings, servers: BTreeMap::new(), diagnostics };
    for (config, raw, source, path, trusted) in [
        (global, None, McpServerSource::Global, global_path, true),
        (claude, claude_raw, McpServerSource::Claude, claude_path, options.project_trusted),
        (project, project_raw, McpServerSource::Project, project_path, options.project_trusted),
    ] {
        if trusted {
            for (name, declaration) in config.and_then(|c| c.mcp_servers).unwrap_or_default() {
                let config = normalize_server(declaration);
                result.servers.insert(name.clone(), resolved_server(name, source, path.clone(), config)?);
            }
        } else if let Some(servers) = raw.as_ref().and_then(|r| r.get("mcpServers")).and_then(Value::as_object) {
            for name in servers.keys() {
                if let Some(existing) = result.servers.get(name).filter(|s| s.state != McpServerState::Untrusted) {
                    result.diagnostics.push(format!("Blocked untrusted {source} MCP server '{name}' from shadowing trusted {} server.", existing.source)); continue;
                }
                result.servers.insert(name.clone(), ResolvedMcpServer { name: name.clone(), source, source_path: path.clone(), state: McpServerState::Untrusted, transport: None, config_hash: None, config: None });
            }
        }
    }
    Ok(result)
}
fn merge_settings(first: Option<&McpSettings>, second: Option<&McpSettings>) -> Result<McpSettings, McpConfigValidationError> {
    let mut raw = serde_json::to_value(default_settings()).map_err(|e| McpConfigValidationError(e.to_string()))?;
    for settings in [first, second].into_iter().flatten() {
        let next = serde_json::to_value(settings).map_err(|e| McpConfigValidationError(e.to_string()))?;
        if let (Some(target), Some(source)) = (raw.as_object_mut(), next.as_object()) { target.extend(source.clone()); }
    }
    serde_json::from_value(raw).map_err(|e| McpConfigValidationError(e.to_string()))
}
fn resolved_server(name: String, source: McpServerSource, source_path: std::path::PathBuf, config: McpServerConfig) -> Result<ResolvedMcpServer, McpConfigValidationError> {
    Ok(ResolvedMcpServer { name, source, source_path, state: if config.enabled == Some(false) { McpServerState::Disabled } else { McpServerState::Enabled }, transport: config.transport, config_hash: Some(hash_config(&config)?), config: Some(config) })
}
pub fn resolve_extension_mcp_server(name: &str, mut raw: ServerConfigWire, source_path: &Path, registration_cwd: &Path) -> Result<ResolvedMcpServer, McpConfigValidationError> {
    raw.cwd.get_or_insert_with(|| registration_cwd.to_string_lossy().into_owned());
    resolved_server(name.into(), McpServerSource::Extension, source_path.into(), normalize_server(raw))
}
pub fn resolve_skill_mcp_server(name: &str, raw: ServerConfigWire, source_path: &Path) -> Result<ResolvedMcpServer, McpConfigValidationError> {
    let mut config = normalize_server(raw);
    config.direct_tools = Some(DirectTools::Patterns(Vec::new())); config.exposure = Some(Exposure::Search);
    resolved_server(name.into(), McpServerSource::Skill, source_path.into(), config)
}
pub fn visit_spawnable_mcp_servers(config: &ResolvedMcpConfig, mut visit: impl FnMut(&str, &ResolvedMcpServer)) {
    for (name, server) in &config.servers { if server.state == McpServerState::Enabled { visit(name, server); } }
}
pub fn merge_extension_mcp_servers(result: &mut ResolvedMcpConfig, declarations: &[maho_ext_api::RegisteredMcpServerDeclaration]) -> Result<(), McpConfigValidationError> {
    for declaration in declarations {
        let name = &declaration.name;
        if let Some(existing) = result.servers.get(name) {
            if existing.state != McpServerState::Untrusted {
                result.diagnostics.push(format!("Extension MCP server '{name}' from {} skipped; {} config at {} wins.", declaration.extension_path, existing.source, existing.source_path.display()));
                continue;
            }
            result.diagnostics.push(format!("Extension MCP server '{name}' from {} replaces untrusted {} placeholder from {}.", declaration.extension_path, existing.source, existing.source_path.display()));
        }
        result.servers.insert(name.clone(), resolve_extension_mcp_server(name, ServerConfigWire::from(&declaration.config), Path::new(&declaration.extension_path), &declaration.registration_cwd)?);
    }
    Ok(())
}
