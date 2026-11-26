//! Remote protocol cases from packages/ai/test/openai-chatgpt-logout.test.ts.
//! Store mutation, concurrent refresh, and pooled-account cases belong to the store layer.

use super::*;
use crate::auth::oauth::transport::{ScriptedResponse, ScriptedTransport};
use std::sync::Mutex;

const ENDPOINT: &str = "https://example.test/revoke";

fn credential() -> OAuthCredential {
    OAuthCredential::new("test-access", "test-refresh+&", 0.0)
        .with_extra("clientId", json!("test-client"))
}

fn discovery(endpoint: &str) -> ScriptedResponse {
    ScriptedResponse::Json {
        status: 200,
        body: json!({"revocation_endpoint":endpoint}),
    }
}

fn status(status: u16) -> ScriptedResponse {
    ScriptedResponse::Text {
        status,
        body: String::new(),
    }
}

fn tracked_backoff() -> (LogoutOptions, Arc<Mutex<Vec<u64>>>) {
    let delays = Arc::new(Mutex::new(Vec::new()));
    let seen = delays.clone();
    let options = LogoutOptions {
        sleep: Some(Arc::new(move |ms| {
            seen.lock().unwrap().push(ms);
            Box::pin(async {})
        })),
        ..Default::default()
    };
    (options, delays)
}

#[tokio::test]
async fn discovers_and_revokes_with_only_the_refresh_token_and_issued_client_id() {
    let transport = Arc::new(ScriptedTransport::new(vec![
        (None, discovery(ENDPOINT)),
        (None, status(200)),
    ]));
    let flow = OpenAIChatGPTOAuth::new(transport.clone());
    assert!(flow.has_revoke());
    assert_eq!(
        flow.revoke(&credential(), None).await.unwrap(),
        OAuthRevocationResult::Revoked
    );
    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].url, DISCOVERY_URL);
    assert!(requests[0].body.is_none());
    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[1].url, ENDPOINT);
    assert_eq!(
        requests[1].headers,
        [(
            "content-type".into(),
            "application/x-www-form-urlencoded".into()
        )]
    );
    assert_eq!(
        url::form_urlencoded::parse(requests[1].body.as_deref().unwrap().as_bytes())
            .collect::<Vec<_>>(),
        [
            ("token".into(), "test-refresh+&".into()),
            ("token_type_hint".into(), "refresh_token".into()),
            ("client_id".into(), "test-client".into())
        ],
    );
}

#[tokio::test]
async fn retries_503_then_accepts_an_empty_200() {
    let transport = ScriptedTransport::new(vec![
        (None, discovery(ENDPOINT)),
        (None, status(503)),
        (None, status(200)),
    ]);
    let (options, delays) = tracked_backoff();
    assert_eq!(
        revoke_session(&transport, &credential(), Some(options)).await,
        OAuthRevocationResult::Revoked
    );
    assert_eq!(*delays.lock().unwrap(), [250]);
    assert_eq!(
        transport
            .requests()
            .iter()
            .map(|r| r.url.as_str())
            .collect::<Vec<_>>(),
        [DISCOVERY_URL, ENDPOINT, ENDPOINT]
    );
}

#[tokio::test]
async fn bounds_network_retries_and_does_not_retry_permanent_rejection() {
    let failure = ScriptedResponse::Failure {
        message: "synthetic network failure".into(),
    };
    for (responses, expected_delay, expected_posts) in [
        (
            vec![failure.clone(), failure.clone(), failure.clone()],
            vec![250, 1000],
            0,
        ),
        (
            vec![
                discovery(ENDPOINT),
                failure.clone(),
                failure.clone(),
                failure,
            ],
            vec![250, 1000],
            3,
        ),
        (vec![discovery(ENDPOINT), status(401)], vec![], 1),
        (vec![discovery(ENDPOINT), status(307)], vec![], 1),
        (vec![discovery(ENDPOINT), status(204)], vec![], 1),
        (vec![status(404)], vec![], 0),
    ] {
        let transport = ScriptedTransport::new(
            responses
                .into_iter()
                .map(|response| (None, response))
                .collect(),
        );
        let (options, delays) = tracked_backoff();
        assert_eq!(
            revoke_session(&transport, &credential(), Some(options)).await,
            OAuthRevocationResult::RemoteRevocationUnconfirmed
        );
        assert_eq!(*delays.lock().unwrap(), expected_delay);
        assert_eq!(
            transport
                .requests()
                .iter()
                .filter(|r| r.method == "POST")
                .count(),
            expected_posts
        );
    }
}

#[tokio::test]
async fn does_not_send_a_token_to_an_insecure_endpoint_or_without_registration() {
    for endpoint in [
        "http://example.test/revoke",
        "https://user:pass@example.test/revoke",
        "not-a-url",
    ] {
        let transport = ScriptedTransport::new(vec![(None, discovery(endpoint))]);
        assert_eq!(
            revoke_session(&transport, &credential(), None).await,
            OAuthRevocationResult::RemoteRevocationUnconfirmed
        );
        assert_eq!(transport.requests().len(), 1);
    }
    let transport = ScriptedTransport::new(vec![]);
    for credential in [
        OAuthCredential::new("a", "r", 0.0),
        OAuthCredential::new("a", "", 0.0).with_extra("clientId", json!("client")),
    ] {
        assert_eq!(
            revoke_session(&transport, &credential, None).await,
            OAuthRevocationResult::RemoteRevocationUnconfirmed
        );
    }
    assert!(transport.requests().is_empty());
}

struct PendingDiscovery {
    started: tokio::sync::Notify,
}

#[async_trait]
impl OAuthTransport for PendingDiscovery {
    async fn execute(
        &self,
        request: HttpRequest,
        _signal: &AbortSignal,
    ) -> anyhow::Result<HttpResponse> {
        assert_eq!(request.url, DISCOVERY_URL);
        assert_eq!(request.method, "GET");
        self.started.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn aborts_active_discovery_before_sending_the_revocation_post() {
    let transport = PendingDiscovery {
        started: tokio::sync::Notify::new(),
    };
    let controller = AbortController::new();
    let credential = credential();
    let options = LogoutOptions {
        signal: Some(controller.signal()),
        ..Default::default()
    };
    let scenario = async {
        let (result, ()) = tokio::join!(
            revoke_session(&transport, &credential, Some(options)),
            async {
                transport.started.notified().await;
                controller.abort(None);
            }
        );
        assert_eq!(result, OAuthRevocationResult::RemoteRevocationUnconfirmed);
    };
    tokio::time::timeout(Duration::from_secs(1), scenario)
        .await
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn applies_the_overall_fifteen_second_deadline_to_a_stalled_request() {
    let transport = PendingDiscovery {
        started: tokio::sync::Notify::new(),
    };
    let credential = credential();
    let started = tokio::time::Instant::now();
    assert_eq!(
        revoke_session(&transport, &credential, None).await,
        OAuthRevocationResult::RemoteRevocationUnconfirmed
    );
    assert_eq!(started.elapsed(), Duration::from_secs(15));
}

#[tokio::test]
async fn production_transport_does_not_follow_a_body_preserving_redirect() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let redirect_target = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let origin = listener.local_addr().unwrap();
    let target = redirect_target.local_addr().unwrap();
    let signal = AbortController::new().signal();
    let transport = NoRedirectTransport::default();
    let request = HttpRequest {
        method: "POST".into(),
        url: format!("http://{origin}/revoke"),
        headers: Vec::new(),
        body: Some("synthetic-body".into()),
        timeout_ms: Some(1000),
    };
    let redirect = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\nsynthetic-body") {
            bytes.push(stream.read_u8().await.unwrap());
        }
        assert!(bytes.starts_with(b"POST /revoke HTTP/1.1"));
        stream.write_all(format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{target}/elsewhere\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ).as_bytes()).await.unwrap();
    };
    let (response, ()) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(transport.execute(request, &signal), redirect)
    })
    .await
    .unwrap();
    assert_eq!(response.unwrap().status, 307);
    assert!(
        tokio::time::timeout(Duration::ZERO, redirect_target.accept())
            .await
            .is_err()
    );
}
