use std::{future::Future, pin::Pin, sync::Arc};
use maho_ai::utils::abort::AbortSignal;
use serde_json::{Value, json};
use super::protocol::verify_bridge_token;

pub const DEFAULT_BODY_LIMIT_BYTES: usize = 1024 * 1024;
pub const LOOPBACK_HOST: &str = "127.0.0.1";

pub struct BridgeServerHandle {
    pub port: u16,
    pub token: String,
    shutdown: maho_ai::utils::abort::AbortController,
    closed: tokio::sync::watch::Receiver<bool>,
}

impl BridgeServerHandle {
    pub async fn close(&self) {
        self.shutdown.abort(None);
        let mut closed = self.closed.clone();
        let _ = closed.wait_for(|closed| *closed).await;
    }
}

impl Drop for BridgeServerHandle {
    fn drop(&mut self) { self.shutdown.abort(None); }
}

pub async fn start_bridge_server(options: BridgeServerOptions) -> Result<BridgeServerHandle, std::io::Error> {
    let listener = tokio::net::TcpListener::bind((LOOPBACK_HOST, 0)).await?;
    let port = listener.local_addr()?.port();
    let token = match options.token.clone() {
        Some(token) => token,
        None => super::protocol::generate_bridge_token(32).map_err(std::io::Error::other)?,
    };
    let options = Arc::new(options);
    let shutdown = maho_ai::utils::abort::AbortController::new();
    let stop = shutdown.signal();
    let (closed_tx, closed) = tokio::sync::watch::channel(false);
    let session_token = token.clone();
    tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                biased;
                () = stop.cancelled() => break,
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { break; };
                    let options = options.clone();
                    let token = session_token.clone();
                    connections.spawn(async move {
                        let service = hyper::service::service_fn(move |request| {
                            serve_bridge_request(request, token.clone(), options.clone())
                        });
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(hyper_util::rt::TokioIo::new(stream), service).await;
                    });
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
        drop(listener);
        connections.abort_all();
        while connections.join_next().await.is_some() {}
        let _ = closed_tx.send(true);
    });
    Ok(BridgeServerHandle { port, token, shutdown, closed })
}

struct RequestAbortGuard(Option<maho_ai::utils::abort::AbortController>);
impl Drop for RequestAbortGuard {
    fn drop(&mut self) { if let Some(controller) = &self.0 { controller.abort(None); } }
}

async fn serve_bridge_request(request: hyper::Request<hyper::body::Incoming>, token: String, options: Arc<BridgeServerOptions>) -> Result<hyper::Response<http_body_util::Full<bytes::Bytes>>, std::convert::Infallible> {
    use http_body_util::BodyExt;
    let controller = maho_ai::utils::abort::AbortController::new();
    let signal = controller.signal();
    let mut guard = RequestAbortGuard(Some(controller));
    let (parts, mut body) = request.into_parts();
    let authorization = parts.headers.get(hyper::header::AUTHORIZATION).and_then(|value| value.to_str().ok());
    let url = parts.uri.path_and_query().map_or("/", |value| value.as_str());
    let route = url.split('?').next().unwrap_or(url);
    // Reject method and authorization before reading a potentially unbounded body.
    let early = if parts.method != "POST" || !matches!(route, "/call" | "/emit" | "/completion") {
        Some((404, Some(transport_error("not_found", "Bridge route was not found"))))
    } else if authorization.and_then(|header| header.strip_prefix("Bearer ")).is_none_or(|auth| auth.is_empty() || verify_bridge_token(&token, auth).is_err()) {
        Some((401, Some(transport_error("unauthorized", "Bridge authorization failed"))))
    } else { None };
    let (status, reply) = if let Some(early) = early { early } else {
        let limit = options.body_limit_bytes.unwrap_or(DEFAULT_BODY_LIMIT_BYTES);
        let mut raw = Vec::new();
        let mut failure = None;
        while let Some(frame) = body.frame().await {
            match frame {
                Ok(frame) => if let Ok(data) = frame.into_data() {
                    if data.len() > limit.saturating_sub(raw.len()) {
                        failure = Some((413, Some(transport_error("body_too_large", &format!("Bridge request body exceeds {limit} bytes")))));
                        break;
                    }
                    raw.extend_from_slice(&data);
                },
                Err(_) => { failure = Some((400, Some(transport_error("invalid_json", "Bridge request body was not valid JSON")))); break; }
            }
        }
        if let Some(failure) = failure { failure } else {
            dispatch_bridge_http_request(parts.method.as_str(), url, authorization, &token, &raw, &options, signal).await
        }
    };
    let mut response = hyper::Response::new(http_body_util::Full::new(bytes::Bytes::from(reply.as_ref().map_or_else(Vec::new, |reply| reply.to_string().into_bytes()))));
    *response.status_mut() = hyper::StatusCode::from_u16(status).expect("fixed bridge status code");
    if reply.is_some() { response.headers_mut().insert(hyper::header::CONTENT_TYPE, hyper::header::HeaderValue::from_static("application/json; charset=utf-8")); }
    // Normal completion must not abort the handler's signal.
    guard.0.take();
    Ok(response)
}

pub struct BridgeHttpCallRequest {
    pub call_id: String,
    pub tool_name: String,
    pub args: Value,
    pub signal: AbortSignal,
}

pub struct BridgeHttpCompletionRequest {
    pub prompt: String,
    pub opts: Option<Value>,
    pub signal: AbortSignal,
}

pub type BridgeHttpFuture<T> = Pin<Box<dyn Future<Output = Result<T, Value>> + Send>>;
pub type BridgeCallHandler = Arc<dyn Fn(BridgeHttpCallRequest) -> BridgeHttpFuture<Value> + Send + Sync>;
pub type BridgeCompletionHandler = Arc<dyn Fn(BridgeHttpCompletionRequest) -> BridgeHttpFuture<Value> + Send + Sync>;
pub type BridgeEmitHandler = Arc<dyn Fn(Value, AbortSignal) -> BridgeHttpFuture<()> + Send + Sync>;

pub struct BridgeServerOptions {
    pub token: Option<String>,
    pub body_limit_bytes: Option<usize>,
    pub on_call: BridgeCallHandler,
    pub on_emit: BridgeEmitHandler,
    pub on_completion: BridgeCompletionHandler,
}

fn transport_error(code: &str, message: &str) -> Value { json!({"ok":false,"error":{"code":code,"message":message}}) }

pub async fn dispatch_bridge_http_request(method: &str, url: &str, authorization: Option<&str>, token: &str, body: &[u8], options: &BridgeServerOptions, signal: AbortSignal) -> (u16, Option<Value>) {
    let route = url.split(['?', '#']).next().unwrap_or(url);
    if method != "POST" || !matches!(route,"/call"|"/emit"|"/completion") {
        return (404,Some(transport_error("not_found","Bridge route was not found")));
    }
    if authorization.and_then(|header|header.strip_prefix("Bearer ")).is_none_or(|auth|auth.is_empty() || verify_bridge_token(token,auth).is_err()) {
        return (401,Some(transport_error("unauthorized","Bridge authorization failed")));
    }
    let limit=options.body_limit_bytes.unwrap_or(DEFAULT_BODY_LIMIT_BYTES);
    if body.len()>limit {return (413,Some(transport_error("body_too_large",&format!("Bridge request body exceeds {limit} bytes"))));}
    let Ok(body)=serde_json::from_slice::<Value>(body) else {return (400,Some(transport_error("invalid_json","Bridge request body was not valid JSON")));};
    if route=="/emit" {
        let valid=match body["kind"].as_str() {
            Some("text")=>matches!(body["stream"].as_str(),Some("stdout"|"stderr")) && body["data"].is_string(),
            Some("display")=>body["mimeType"].is_string() && body["dataBase64"].is_string(),
            Some("log")=>body["message"].is_string(),
            Some("phase")=>body["title"].is_string(),
            _=>false,
        };
        if !valid {return (400,Some(transport_error("invalid_request","Bridge emit request was invalid")));}
        return match (options.on_emit)(body,signal).await {
            Ok(())=>(204,None),Err(error)=>(200,Some(json!({"ok":false,"error":error}))),
        };
    }
    let result=if route=="/call" {
        let (Some(call_id),Some(tool_name),Some(args))=(body["callId"].as_str(),body["toolName"].as_str(),body.get("args")) else {
            return (200,Some(transport_error("invalid_request","Bridge call request was invalid")));
        };
        (options.on_call)(BridgeHttpCallRequest{call_id:call_id.into(),tool_name:tool_name.into(),args:args.clone(),signal}).await
    } else {
        let Some(prompt)=body["prompt"].as_str() else {return (200,Some(transport_error("invalid_request","Bridge completion request was invalid")));};
        (options.on_completion)(BridgeHttpCompletionRequest{prompt:prompt.into(),opts:body.get("opts").cloned(),signal}).await
    };
    (200,Some(match result {Ok(value)=>json!({"ok":true,"value":value}),Err(error)=>json!({"ok":false,"error":error})}))
}
