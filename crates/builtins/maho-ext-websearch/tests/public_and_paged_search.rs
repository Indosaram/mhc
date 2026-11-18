//! Findings 13-15 of the 2026-11-18 weekly review, exercised through the real search path against a
//! loopback server: keyless `public` fan-out, OpenRouter domain enforcement and Tinyfish pagination.

use maho_ext_websearch::websearch::{
    provider_endpoints::SearchProvider,
    search::perform_search,
    types::{
        RoutingStrategy, SearchProviderConfig, SearchProviderEntry, SearchRequest, WebsearchConfig,
    },
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::watch,
};

const DDG: &str = include_str!("fixtures/websearch/duckduckgo-results.html");
const STARTPAGE: &str = include_str!("fixtures/websearch/startpage-results.html");
const GOOGLE: &str = include_str!("fixtures/websearch/google-results.html");
const ECOSIA: &str = include_str!("fixtures/websearch/ecosia-results.html");
const MOJEEK: &str = include_str!("fixtures/websearch/mojeek-results.html");
const TYPESCRIPT_URL: &str =
    "https://www.typescriptlang.org/docs/handbook/release-notes/typescript-4-9.html";

type Reply = Option<(u16, String)>;

struct Fake {
    address: std::net::SocketAddr,
    hits: Arc<Mutex<Vec<String>>>,
    count: watch::Receiver<usize>,
}

impl Fake {
    async fn start(answer: impl Fn(&str) -> Reply + Send + Sync + 'static) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let answer: Arc<dyn Fn(&str) -> Reply + Send + Sync> = Arc::new(answer);
        let hits = Arc::new(Mutex::new(Vec::new()));
        let (sender, count) = watch::channel(0usize);
        let (server_answer, server_hits) = (answer, hits.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let (answer, hits, sender) =
                    (server_answer.clone(), server_hits.clone(), sender.clone());
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let mut chunk = [0; 4096];
                    loop {
                        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let head = String::from_utf8_lossy(&bytes).into_owned();
                    let target = head
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_owned();
                    let arrived = {
                        let mut hits = hits.lock().expect("fixture hits");
                        hits.push(target.clone());
                        hits.len()
                    };
                    sender.send_replace(arrived);
                    let Some((status, body)) = answer(&target) else {
                        // No answer: hold the connection until the caller stops waiting for it.
                        while matches!(socket.read(&mut chunk).await, Ok(n) if n > 0) {}
                        return;
                    };
                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        Self {
            address,
            hits,
            count,
        }
    }

    async fn wait_for_requests(&self, requests: usize) {
        let mut count = self.count.clone();
        count
            .wait_for(move |arrived| *arrived >= requests)
            .await
            .expect("fixture stays alive");
    }

    fn hits(&self) -> Vec<String> {
        self.hits.lock().expect("fixture hits").clone()
    }

    fn paths(&self) -> Vec<String> {
        let mut paths = self
            .hits()
            .iter()
            .map(|target| target.split('?').next().unwrap_or_default().to_owned())
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    fn endpoint(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }
}

fn request(max_results: f64) -> SearchRequest {
    SearchRequest {
        query: "typescript satisfies operator".into(),
        max_results,
        allowed_domains: None,
        blocked_domains: None,
    }
}

fn entry(config: SearchProviderConfig) -> SearchProviderEntry {
    SearchProviderEntry {
        config,
        priority: None,
        weight: None,
        fallback_model: None,
    }
}

fn config(providers: Vec<SearchProviderEntry>) -> WebsearchConfig {
    WebsearchConfig {
        strategy: RoutingStrategy::Priority,
        fallback: true,
        auto: false,
        native_model: None,
        providers,
    }
}

fn public_at(fake: &Fake) -> SearchProviderConfig {
    let mut public = SearchProviderConfig::new(SearchProvider::Public);
    public.base_url = Some(fake.endpoint("/engines"));
    public
}

#[tokio::test]
async fn public_fans_out_to_every_engine_and_merges_their_results() {
    let fake = Fake::start(|target| {
        let path = target.split('?').next().unwrap_or_default();
        let body = match path {
            "/engines/startpage" => STARTPAGE,
            "/engines/google-html" => GOOGLE,
            "/engines/duckduckgo-html" => DDG,
            "/engines/ecosia" => ECOSIA,
            "/engines/mojeek" => MOJEEK,
            _ => return None,
        };
        Some((200, body.to_owned()))
    })
    .await;
    let details = perform_search(
        &config(vec![entry(public_at(&fake))]),
        &request(10.),
        None,
        None,
        None,
    )
    .await
    .expect("public search");
    assert_eq!(details.error, None);
    assert_eq!(details.provider, SearchProvider::Public);
    let urls = details
        .results
        .iter()
        .map(|item| item.url.as_str())
        .collect::<Vec<_>>();
    assert_eq!(urls[0], TYPESCRIPT_URL);
    let mut unique = urls.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), urls.len());
    assert!(urls.len() <= 10);
    assert_eq!(
        fake.paths(),
        [
            "/engines/duckduckgo-html",
            "/engines/ecosia",
            "/engines/google-html",
            "/engines/mojeek",
            "/engines/startpage",
        ]
    );
}

#[tokio::test]
async fn cancellation_stops_the_public_fan_out_before_any_engine_answers() {
    let signal = maho_tools::definition::AbortSignal::default();
    let fake = Fake::start(|_| None).await;
    let trigger = signal.clone();
    let watch = async {
        fake.wait_for_requests(1).await;
        trigger.abort();
    };
    let routes = config(vec![entry(public_at(&fake))]);
    let query = request(10.);
    let client = perform_search(&routes, &query, Some(&signal), None, None);
    let (_, result) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(watch, client)
    })
    .await
    .expect("public cancellation is bounded");
    assert!(matches!(
        result,
        Err(maho_tools::definition::ToolError::Aborted)
    ));
}

#[tokio::test]
async fn a_failed_public_route_falls_back_to_the_next_provider() {
    let fake = Fake::start(|target| {
        let path = target.split('?').next().unwrap_or_default();
        if path == "/exa" {
            return Some((
                200,
                r#"{"results":[{"title":"Found","url":"https://example.test/a","text":"snippet"}]}"#
                    .to_owned(),
            ));
        }
        Some((403, "forbidden".to_owned()))
    })
    .await;
    let mut exa = SearchProviderConfig::new(SearchProvider::Exa);
    exa.api_key = Some("exa-test".into());
    exa.base_url = Some(fake.endpoint("/exa"));
    let details = perform_search(
        &config(vec![entry(public_at(&fake)), entry(exa)]),
        &request(10.),
        None,
        None,
        None,
    )
    .await
    .expect("fallback search");
    assert_eq!(details.error, None);
    assert_eq!(details.provider, SearchProvider::Exa);
    assert_eq!(details.results[0].url, "https://example.test/a");
    let attempts = details.attempts.expect("attempts");
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].provider, SearchProvider::Public);
    assert!(attempts[0].error.is_some());
    assert_eq!(attempts[1].provider, SearchProvider::Exa);
}

fn openrouter_body() -> String {
    r#"{"choices":[{"message":{"annotations":[
        {"type":"url_citation","url_citation":{"url":"https://docs.example.test/allowed","title":"Allowed"}},
        {"type":"url_citation","url_citation":{"url":"https://elsewhere.test/denied","title":"Denied"}},
        {"type":"url_citation","url_citation":{"url":"https://notdocs.example.test/sibling","title":"Sibling"}}
    ]}}]}"#
        .to_owned()
}

fn openrouter_at(fake: &Fake) -> SearchProviderEntry {
    let mut openrouter = SearchProviderConfig::new(SearchProvider::OpenRouter);
    openrouter.api_key = Some("or-key".into());
    openrouter.base_url = Some(fake.endpoint("/v1"));
    entry(openrouter)
}

#[tokio::test]
async fn openrouter_keeps_only_the_citations_inside_the_allowed_domains() {
    let fake = Fake::start(move |_| Some((200, openrouter_body()))).await;
    let mut search = request(10.);
    search.allowed_domains = Some(vec!["example.test".into()]);
    let details = perform_search(
        &config(vec![openrouter_at(&fake)]),
        &search,
        None,
        None,
        None,
    )
    .await
    .expect("openrouter search");
    assert_eq!(details.error, None);
    assert_eq!(
        details
            .results
            .iter()
            .map(|item| item.url.as_str())
            .collect::<Vec<_>>(),
        [
            "https://docs.example.test/allowed",
            "https://notdocs.example.test/sibling"
        ]
    );
    assert_eq!(fake.paths(), ["/v1/chat/completions"]);
}

#[tokio::test]
async fn openrouter_rejects_a_restriction_its_citations_cannot_satisfy() {
    let fake = Fake::start(move |_| Some((200, openrouter_body()))).await;
    let mut search = request(10.);
    search.allowed_domains = Some(vec!["missing.example.test".into()]);
    let details = perform_search(
        &config(vec![openrouter_at(&fake)]),
        &search,
        None,
        None,
        None,
    )
    .await
    .expect("openrouter search");
    assert!(details.results.is_empty());
    assert!(details.error.is_some());
    assert!(details.attempts.expect("attempts")[0].error.is_some());
}

#[tokio::test]
async fn openrouter_blocked_domains_are_dropped_from_the_citations() {
    let fake = Fake::start(move |_| Some((200, openrouter_body()))).await;
    let mut search = request(10.);
    search.blocked_domains = Some(vec!["elsewhere.test".into()]);
    let details = perform_search(
        &config(vec![openrouter_at(&fake)]),
        &search,
        None,
        None,
        None,
    )
    .await
    .expect("openrouter search");
    assert_eq!(details.error, None);
    assert_eq!(
        details
            .results
            .iter()
            .map(|item| item.url.as_str())
            .collect::<Vec<_>>(),
        [
            "https://docs.example.test/allowed",
            "https://notdocs.example.test/sibling"
        ]
    );
}

fn tinyfish_page(page: usize) -> String {
    let names = match page {
        0 => (1..=10).collect::<Vec<_>>(),
        1 => (5..=14).collect::<Vec<_>>(),
        _ => vec![15, 16],
    };
    let results = names
        .iter()
        .map(|index| {
            format!(
                r#"{{"title":"u{index}","url":"https://u{index}.test/page","snippet":"u{index}"}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(r#"{{"results":[{results}]}}"#)
}

fn tinyfish_page_of(target: &str) -> usize {
    let page = target
        .split('&')
        .find_map(|pair| pair.strip_prefix("page="))
        .unwrap_or("0");
    page.parse().expect("fixture page number")
}

fn tinyfish_entry(fake: &Fake) -> SearchProviderEntry {
    let mut tinyfish = SearchProviderConfig::new(SearchProvider::Tinyfish);
    tinyfish.api_key = Some("tf-key".into());
    tinyfish.base_url = Some(fake.endpoint("/tinyfish"));
    entry(tinyfish)
}

#[tokio::test]
async fn tinyfish_walks_every_page_and_drops_repeated_urls() {
    let fake = Fake::start(|target| Some((200, tinyfish_page(tinyfish_page_of(target))))).await;
    let details = perform_search(
        &config(vec![tinyfish_entry(&fake)]),
        &request(20.),
        None,
        None,
        None,
    )
    .await
    .expect("tinyfish search");
    assert_eq!(details.error, None);
    assert_eq!(
        details
            .results
            .iter()
            .map(|item| item.url.clone())
            .collect::<Vec<_>>(),
        (1..=16)
            .map(|index| format!("https://u{index}.test/page"))
            .collect::<Vec<_>>()
    );
    assert!(!details.truncated);
    assert_eq!(
        fake.hits()
            .iter()
            .map(|target| tinyfish_page_of(target))
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
}

#[tokio::test]
async fn tinyfish_pages_stop_at_the_requested_count() {
    let fake = Fake::start(|target| Some((200, tinyfish_page(tinyfish_page_of(target))))).await;
    let details = perform_search(
        &config(vec![tinyfish_entry(&fake)]),
        &request(12.),
        None,
        None,
        None,
    )
    .await
    .expect("tinyfish search");
    assert_eq!(details.error, None);
    assert_eq!(details.results.len(), 12);
    assert!(details.truncated);
    assert_eq!(
        fake.hits()
            .iter()
            .map(|target| tinyfish_page_of(target))
            .collect::<Vec<_>>(),
        [0, 1]
    );
}

#[tokio::test]
async fn tinyfish_stops_between_pages_when_cancelled() {
    let signal = maho_tools::definition::AbortSignal::default();
    let fake = Fake::start(|target| {
        let page = tinyfish_page_of(target);
        (page == 0).then(|| (200, tinyfish_page(0)))
    })
    .await;
    let trigger = signal.clone();
    let watch = async {
        fake.wait_for_requests(2).await;
        trigger.abort();
    };
    let routes = config(vec![tinyfish_entry(&fake)]);
    let query = request(20.);
    let client = perform_search(&routes, &query, Some(&signal), None, None);
    let (_, result) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(watch, client)
    })
    .await
    .expect("tinyfish cancellation is bounded");
    assert!(matches!(
        result,
        Err(maho_tools::definition::ToolError::Aborted)
    ));
    assert_eq!(
        fake.hits()
            .iter()
            .map(|target| tinyfish_page_of(target))
            .collect::<Vec<_>>(),
        [0, 1]
    );
}

#[tokio::test]
async fn a_single_page_provider_answers_through_the_shared_path() {
    let fake = Fake::start(|_| Some((200, DDG.to_owned()))).await;
    let mut duckduckgo = SearchProviderConfig::new(SearchProvider::DuckduckgoHtml);
    duckduckgo.base_url = Some(fake.endpoint("/ddg"));
    let details = perform_search(
        &config(vec![entry(duckduckgo)]),
        &request(2.),
        None,
        None,
        None,
    )
    .await
    .expect("duckduckgo search");
    assert_eq!(details.error, None);
    assert_eq!(details.results.len(), 2);
    assert!(details.truncated);
    assert_eq!(fake.hits().len(), 1);
}
