//! Port of senpi packages/ai/src/api/websocket-transport-failure.ts.
//!
//! The TS module consumes the runtime's WebSocket `error`/`close` event objects. The Rust host has
//! no DOM events, so the same payloads are modelled as [`WebSocketErrorEvent`] and
//! [`WebSocketCloseEvent`]; every predicate and the at-most-one-report sequencing are ported
//! verbatim.

use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const WEBSOCKET_ERROR_CLOSE_GRACE_MS: u64 = 250;
pub const WEBSOCKET_MESSAGE_TOO_BIG_CLOSE_CODE: u16 = 1009;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct WebSocketCloseError {
    pub message: String,
    pub code: Option<u16>,
    pub reason: Option<String>,
    pub was_clean: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebSocketErrorEvent {
    pub message: Option<String>,
    pub nested_message: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebSocketCloseEvent {
    pub code: Option<u16>,
    pub reason: Option<String>,
    pub was_clean: Option<bool>,
}

fn non_empty_string(value: Option<&str>) -> Option<String> {
    value.filter(|value| !value.is_empty()).map(str::to_owned)
}

/// The runtime's own message for an `error` event, or `None` when it sent none.
pub fn extract_web_socket_error_message(event: &WebSocketErrorEvent) -> Option<String> {
    if let Some(message) = non_empty_string(event.message.as_deref()) {
        return Some(message);
    }
    non_empty_string(event.nested_message.as_deref())
}

pub fn generic_web_socket_error() -> String {
    "WebSocket error".to_owned()
}

pub fn extract_web_socket_close_error(event: &WebSocketCloseEvent) -> WebSocketCloseError {
    let reason = non_empty_string(event.reason.as_deref());
    let code_text = event.code.map(|code| format!(" {code}")).unwrap_or_default();
    let reason_text = match &reason {
        Some(reason) => format!(" {reason}"),
        None if event.code == Some(WEBSOCKET_MESSAGE_TOO_BIG_CLOSE_CODE) => " message too big".to_owned(),
        None => String::new(),
    };
    WebSocketCloseError {
        message: format!("WebSocket closed{code_text}{reason_text}").trim().to_owned(),
        code: event.code,
        reason,
        was_clean: event.was_clean,
    }
}

pub struct WebSocketTransportFailure {
    fail: Arc<dyn Fn(String) + Send + Sync>,
    grace: Mutex<Option<tokio::task::JoinHandle<()>>>,
    reported: Arc<Mutex<bool>>,
    grace_ms: u64,
}

impl WebSocketTransportFailure {
    fn report(&self, error: String) {
        {
            let mut reported = self.reported.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if *reported {
                return;
            }
            *reported = true;
        }
        self.dispose();
        (self.fail)(error);
    }

    pub fn on_error(&self, event: &WebSocketErrorEvent) {
        if let Some(error) = extract_web_socket_error_message(event) {
            self.report(error);
            return;
        }
        let mut grace = self.grace.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if *self.reported.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) || grace.is_some() {
            return;
        }
        let fail = self.fail.clone();
        let reported = self.reported.clone();
        let grace_ms = self.grace_ms;
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(grace_ms)).await;
            let mut reported = reported.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if *reported {
                return;
            }
            *reported = true;
            drop(reported);
            fail(generic_web_socket_error());
        });
        *grace = Some(handle);
    }

    pub fn on_close(&self, event: &WebSocketCloseEvent) {
        let error = extract_web_socket_close_error(event);
        self.report(error.message);
    }

    pub fn dispose(&self) {
        let mut grace = self.grace.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(handle) = grace.take() {
            handle.abort();
        }
    }
}

/// Reports at most one failure to `fail`. A message-bearing `error` reports at once; a message-less
/// one defers to the `close` that follows, or to the generic error once the grace expires.
pub fn create_web_socket_transport_failure(
    fail: impl Fn(String) + Send + Sync + 'static,
    grace_ms: u64,
) -> WebSocketTransportFailure {
    WebSocketTransportFailure {
        fail: Arc::new(fail),
        grace: Mutex::new(None),
        reported: Arc::new(Mutex::new(false)),
        grace_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_errors_name_the_code_reason_and_cleanliness() {
        let error = extract_web_socket_close_error(&WebSocketCloseEvent {
            code: Some(1006),
            reason: Some("Connection ended".into()),
            was_clean: Some(false),
        });
        assert_eq!(error.message, "WebSocket closed 1006 Connection ended");
        assert_eq!(error.code, Some(1006));
        assert_eq!(error.was_clean, Some(false));

        let too_big = extract_web_socket_close_error(&WebSocketCloseEvent {
            code: Some(WEBSOCKET_MESSAGE_TOO_BIG_CLOSE_CODE),
            reason: None,
            was_clean: None,
        });
        assert_eq!(too_big.message, "WebSocket closed 1009 message too big");

        assert_eq!(extract_web_socket_close_error(&WebSocketCloseEvent::default()).message, "WebSocket closed");
    }

    #[test]
    fn error_messages_prefer_the_event_then_the_nested_error() {
        assert_eq!(extract_web_socket_error_message(&WebSocketErrorEvent::default()), None);
        assert_eq!(
            extract_web_socket_error_message(&WebSocketErrorEvent {
                message: Some(String::new()),
                nested_message: Some("nested".into())
            }),
            Some("nested".to_owned())
        );
        assert_eq!(
            extract_web_socket_error_message(&WebSocketErrorEvent {
                message: Some("outer".into()),
                nested_message: Some("nested".into())
            }),
            Some("outer".to_owned())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_message_less_error_waits_for_the_close_frame() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let failure = create_web_socket_transport_failure(
            move |error| sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(error),
            WEBSOCKET_ERROR_CLOSE_GRACE_MS,
        );

        failure.on_error(&WebSocketErrorEvent::default());
        assert!(seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).is_empty());

        failure.on_close(&WebSocketCloseEvent { code: Some(1006), reason: None, was_clean: Some(false) });
        assert_eq!(
            seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_slice(),
            ["WebSocket closed 1006"]
        );

        failure.on_close(&WebSocketCloseEvent { code: Some(1001), reason: None, was_clean: Some(true) });
        assert_eq!(seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn the_grace_reports_the_generic_error_once() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let failure = create_web_socket_transport_failure(
            move |error| sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(error),
            WEBSOCKET_ERROR_CLOSE_GRACE_MS,
        );

        failure.on_error(&WebSocketErrorEvent::default());
        failure.on_error(&WebSocketErrorEvent::default());
        for _ in 0..16 {
            tokio::time::advance(Duration::from_millis(WEBSOCKET_ERROR_CLOSE_GRACE_MS)).await;
            tokio::task::yield_now().await;
            if !seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).is_empty() {
                break;
            }
        }
        assert_eq!(
            seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_slice(),
            ["WebSocket error"]
        );

        failure.on_close(&WebSocketCloseEvent { code: Some(1006), reason: None, was_clean: None });
        assert_eq!(seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn dispose_cancels_a_pending_grace() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let failure = create_web_socket_transport_failure(
            move |error| sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(error),
            WEBSOCKET_ERROR_CLOSE_GRACE_MS,
        );

        failure.on_error(&WebSocketErrorEvent::default());
        failure.dispose();
        tokio::time::advance(Duration::from_millis(WEBSOCKET_ERROR_CLOSE_GRACE_MS * 4)).await;
        tokio::task::yield_now().await;
        assert!(seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_message_bearing_error_reports_immediately() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let failure = create_web_socket_transport_failure(
            move |error| sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(error),
            WEBSOCKET_ERROR_CLOSE_GRACE_MS,
        );

        failure.on_error(&WebSocketErrorEvent { message: Some("boom".into()), nested_message: None });
        assert_eq!(seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_slice(), ["boom"]);
    }
}
