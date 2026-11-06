//! Upstream `test/suite/websearch-free-chain-routing.test.ts` "per-engine block and cooldown" and
//! "web_search tool keeps cooldowns" (senpi 903b8336). Startpage is not dispatched by this crate yet,
//! so a second DuckDuckGo entry (`id: "backup"`) plays the next keyless engine, and a recording
//! loopback server stands in for the stubbed `fetch`.
use maho_ext_websearch::websearch::{
    provider_endpoints::SearchProvider,
    search::{
        Clock, ENGINE_COOLDOWN_BASE_MS, SearchRoutingState, create_search_routing_state_with,
        format_search_text, perform_search,
    },
    types::{
        RoutingStrategy, SearchBlockReason, SearchDetails, SearchProviderConfig,
        SearchProviderEntry, SearchRequest, WebsearchConfig,
    },
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const RESULTS: &str = include_str!("fixtures/websearch/duckduckgo-results.html");
const ANOMALY: &str = include_str!("fixtures/websearch/duckduckgo-anomaly.html");

#[derive(Clone)]
enum Reply {
    Html(u16, &'static str),
    Close,
}
type Routes = Arc<Mutex<BTreeMap<&'static str, Reply>>>;

/// Answers by path; every request path is recorded, and a path missing from the table fails loudly.
struct Engines {
    address: std::net::SocketAddr,
    routes: Routes,
    hits: Arc<Mutex<Vec<String>>>,
}
impl Engines {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let routes: Routes = Arc::default();
        let hits: Arc<Mutex<Vec<String>>> = Arc::default();
        let (server_routes, server_hits) = (routes.clone(), hits.clone());
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.expect("accept fixture request");
                let (routes, hits) = (server_routes.clone(), server_hits.clone());
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let mut chunk = [0; 4096];
                    let header_end = loop {
                        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n")
                        {
                            break end + 4;
                        }
                        let n = socket
                            .read(&mut chunk)
                            .await
                            .expect("read fixture request headers");
                        if n == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&chunk[..n]);
                    };
                    let head = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
                    let length = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    while bytes.len() < header_end + length {
                        let n = socket
                            .read(&mut chunk)
                            .await
                            .expect("read fixture request body");
                        if n == 0 {
                            break;
                        }
                        bytes.extend_from_slice(&chunk[..n]);
                    }
                    let target = head.split_whitespace().nth(1).unwrap_or_default();
                    let path = target.split('?').next().unwrap_or_default().to_owned();
                    hits.lock().expect("fixture hits").push(path.clone());
                    let reply = routes
                        .lock()
                        .expect("fixture routes")
                        .get(path.as_str())
                        .cloned();
                    let Some(Reply::Html(status, body)) = reply else {
                        return;
                    };
                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        Self {
            address,
            routes,
            hits,
        }
    }
    fn serve(&self, routes: &[(&'static str, Reply)]) {
        *self.routes.lock().expect("fixture routes") = routes.iter().cloned().collect();
        self.hits.lock().expect("fixture hits").clear();
    }
    fn hits(&self) -> Vec<String> {
        self.hits.lock().expect("fixture hits").clone()
    }
    fn entry(&self, provider: SearchProvider, id: Option<&str>, path: &str) -> SearchProviderEntry {
        let mut config = SearchProviderConfig::new(provider);
        config.id = id.map(str::to_owned);
        config.base_url = Some(format!("http://{}{path}", self.address));
        if provider != SearchProvider::DuckduckgoHtml {
            config.api_key = Some("brave-test".into());
        }
        SearchProviderEntry {
            config,
            priority: None,
            weight: None,
            fallback_model: None,
        }
    }
    fn free_chain(&self) -> WebsearchConfig {
        WebsearchConfig {
            strategy: RoutingStrategy::Priority,
            fallback: true,
            auto: false,
            native_model: None,
            providers: vec![
                self.entry(SearchProvider::DuckduckgoHtml, None, "/ddg"),
                self.entry(SearchProvider::DuckduckgoHtml, Some("backup"), "/backup"),
            ],
        }
    }
}

fn request() -> SearchRequest {
    SearchRequest {
        query: "typescript satisfies operator".into(),
        max_results: 10.,
        allowed_domains: None,
        blocked_domains: None,
    }
}
fn fake_clock() -> (Clock, Arc<AtomicU64>) {
    let now = Arc::new(AtomicU64::new(1_790_640_000_000));
    let reader = now.clone();
    (Arc::new(move || reader.load(Ordering::SeqCst) as f64), now)
}
fn state(clock: &Clock) -> SearchRoutingState {
    create_search_routing_state_with(2, HashMap::new(), clock.clone())
}
fn labels(details: &SearchDetails) -> Vec<String> {
    details
        .attempts
        .iter()
        .flatten()
        .map(|attempt| {
            let label = maho_ext_websearch::websearch::route_attempts::provider_entry_label(
                attempt.provider.as_str(),
                None,
                attempt.entry_id.as_deref(),
            );
            let outcome = if attempt.skipped {
                "skipped".into()
            } else if let Some(blocked) = attempt.blocked {
                blocked.as_str().to_owned()
            } else {
                attempt.results_count.to_string()
            };
            format!("{label}:{outcome}")
        })
        .collect()
}
const BASE: u64 = ENGINE_COOLDOWN_BASE_MS as u64;

#[tokio::test]
async fn moves_on_to_the_next_engine_when_duckduckgo_serves_its_anomaly_page() {
    let engines = Engines::start().await;
    let (clock, _) = fake_clock();
    engines.serve(&[
        ("/ddg", Reply::Html(202, ANOMALY)),
        ("/backup", Reply::Html(200, RESULTS)),
    ]);
    let details = perform_search(
        &engines.free_chain(),
        &request(),
        None,
        Some(&mut state(&clock)),
        None,
    )
    .await
    .unwrap();
    assert_eq!(details.error, None);
    assert_eq!(details.entry_id.as_deref(), Some("backup"));
    assert_eq!(
        labels(&details),
        ["duckduckgo-html:challenge", "duckduckgo-html/backup:3"]
    );
    assert_eq!(engines.hits(), ["/ddg", "/backup"]);
    assert!(
        format_search_text(&details).contains(
            "duckduckgo-html challenged: DuckDuckGo served a bot challenge (anomaly page)"
        )
    );
}

#[tokio::test]
async fn skips_a_cooled_down_engine_and_names_it_in_the_routing_attempts_line() {
    let engines = Engines::start().await;
    let (clock, _) = fake_clock();
    let mut state = state(&clock);
    engines.serve(&[
        ("/ddg", Reply::Html(202, ANOMALY)),
        ("/backup", Reply::Html(200, RESULTS)),
    ]);
    perform_search(
        &engines.free_chain(),
        &request(),
        None,
        Some(&mut state),
        None,
    )
    .await
    .unwrap();
    engines.serve(&[("/backup", Reply::Html(200, RESULTS))]);
    let second = perform_search(
        &engines.free_chain(),
        &request(),
        None,
        Some(&mut state),
        None,
    )
    .await
    .unwrap();
    assert_eq!(engines.hits(), ["/backup"]);
    assert_eq!(
        labels(&second),
        ["duckduckgo-html:skipped", "duckduckgo-html/backup:3"]
    );
    assert!(
        format_search_text(&second).contains(
            "Routing attempts: duckduckgo-html skipped (cooling down for 60s after a bot challenge)"
        ),
        "{}",
        format_search_text(&second)
    );
}

#[tokio::test]
async fn retries_once_the_cooldown_expires_and_doubles_it_on_a_repeat_block() {
    let engines = Engines::start().await;
    let (clock, now) = fake_clock();
    let mut state = state(&clock);
    let config = engines.free_chain();
    engines.serve(&[
        ("/ddg", Reply::Html(202, ANOMALY)),
        ("/backup", Reply::Html(200, RESULTS)),
    ]);
    perform_search(&config, &request(), None, Some(&mut state), None)
        .await
        .unwrap();

    now.fetch_add(BASE - 1, Ordering::SeqCst);
    engines.serve(&[("/backup", Reply::Html(200, RESULTS))]);
    perform_search(&config, &request(), None, Some(&mut state), None)
        .await
        .unwrap();
    assert!(!engines.hits().contains(&"/ddg".to_owned()));

    now.fetch_add(1, Ordering::SeqCst);
    engines.serve(&[
        ("/ddg", Reply::Html(202, ANOMALY)),
        ("/backup", Reply::Html(200, RESULTS)),
    ]);
    perform_search(&config, &request(), None, Some(&mut state), None)
        .await
        .unwrap();
    assert_eq!(engines.hits()[0], "/ddg");

    now.fetch_add(BASE, Ordering::SeqCst);
    engines.serve(&[("/backup", Reply::Html(200, RESULTS))]);
    perform_search(&config, &request(), None, Some(&mut state), None)
        .await
        .unwrap();
    assert!(!engines.hits().contains(&"/ddg".to_owned()));

    now.fetch_add(BASE, Ordering::SeqCst);
    engines.serve(&[("/ddg", Reply::Html(200, RESULTS))]);
    let recovered = perform_search(&config, &request(), None, Some(&mut state), None)
        .await
        .unwrap();
    assert_eq!(recovered.entry_id, None);
    assert_eq!(labels(&recovered), ["duckduckgo-html:3"]);
}

#[tokio::test]
async fn a_success_clears_the_backoff_so_the_next_block_starts_from_the_base_cooldown() {
    let engines = Engines::start().await;
    let (clock, now) = fake_clock();
    let mut state = state(&clock);
    let config = engines.free_chain();
    engines.serve(&[
        ("/ddg", Reply::Html(202, ANOMALY)),
        ("/backup", Reply::Html(200, RESULTS)),
    ]);
    perform_search(&config, &request(), None, Some(&mut state), None)
        .await
        .unwrap();
    now.fetch_add(BASE, Ordering::SeqCst);
    engines.serve(&[("/ddg", Reply::Html(200, RESULTS))]);
    perform_search(&config, &request(), None, Some(&mut state), None)
        .await
        .unwrap();

    engines.serve(&[
        ("/ddg", Reply::Html(202, ANOMALY)),
        ("/backup", Reply::Html(200, RESULTS)),
    ]);
    perform_search(&config, &request(), None, Some(&mut state), None)
        .await
        .unwrap();
    now.fetch_add(BASE, Ordering::SeqCst);
    engines.serve(&[("/ddg", Reply::Html(200, RESULTS))]);
    perform_search(&config, &request(), None, Some(&mut state), None)
        .await
        .unwrap();
    assert_eq!(engines.hits(), ["/ddg"]);
}

#[tokio::test]
async fn cools_down_on_429_403_and_network_errors_but_not_on_a_server_error() {
    let engines = Engines::start().await;
    let (clock, _) = fake_clock();
    let config = engines.free_chain();
    let mut outcomes = Vec::new();
    for reply in [
        Reply::Html(429, "slow down"),
        Reply::Html(403, "forbidden"),
        Reply::Close,
        Reply::Html(500, "oops"),
    ] {
        let mut state = state(&clock);
        engines.serve(&[("/ddg", reply), ("/backup", Reply::Html(200, RESULTS))]);
        let first = perform_search(&config, &request(), None, Some(&mut state), None)
            .await
            .unwrap();
        engines.serve(&[
            ("/ddg", Reply::Html(200, RESULTS)),
            ("/backup", Reply::Html(200, RESULTS)),
        ]);
        perform_search(&config, &request(), None, Some(&mut state), None)
            .await
            .unwrap();
        let blocked = first
            .attempts
            .as_ref()
            .and_then(|attempts| attempts[0].blocked);
        outcomes.push((blocked, engines.hits().contains(&"/ddg".to_owned())));
    }
    assert_eq!(
        outcomes,
        [
            (Some(SearchBlockReason::RateLimited), false),
            (Some(SearchBlockReason::Forbidden), false),
            (Some(SearchBlockReason::Network), false),
            (None, true),
        ]
    );
}

#[tokio::test]
async fn a_provider_that_needs_a_key_is_never_cooled_down() {
    let engines = Engines::start().await;
    let (clock, _) = fake_clock();
    let mut state = state(&clock);
    let config = WebsearchConfig {
        providers: vec![
            engines.entry(SearchProvider::Brave, None, "/brave"),
            engines.entry(SearchProvider::DuckduckgoHtml, None, "/ddg"),
        ],
        ..engines.free_chain()
    };
    let routes = [
        ("/brave", Reply::Html(429, "limit")),
        ("/ddg", Reply::Html(200, RESULTS)),
    ];
    engines.serve(&routes);
    perform_search(&config, &request(), None, Some(&mut state), None)
        .await
        .unwrap();
    engines.serve(&routes);
    let second = perform_search(&config, &request(), None, Some(&mut state), None)
        .await
        .unwrap();
    assert_eq!(engines.hits()[0], "/brave");
    assert!(!second.attempts.unwrap()[0].skipped);
}

#[tokio::test]
async fn reports_every_engine_as_skipped_when_all_are_cooling_down() {
    let engines = Engines::start().await;
    let (clock, _) = fake_clock();
    let mut state = state(&clock);
    let config = engines.free_chain();
    engines.serve(&[
        ("/ddg", Reply::Html(202, ANOMALY)),
        ("/backup", Reply::Html(202, ANOMALY)),
    ]);
    perform_search(&config, &request(), None, Some(&mut state), None)
        .await
        .unwrap();
    engines.serve(&[]);
    let details = perform_search(&config, &request(), None, Some(&mut state), None)
        .await
        .unwrap();
    assert!(engines.hits().is_empty());
    assert_eq!(
        labels(&details),
        ["duckduckgo-html:skipped", "duckduckgo-html/backup:skipped"]
    );
    assert!(
        details
            .error
            .unwrap()
            .starts_with("All configured search providers failed: duckduckgo-html skipped")
    );
}
