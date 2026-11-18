use super::providers::{
    DomainFilters, build_context, build_search_request, normalize_search_response,
    shared::{BuiltSearchRequest, FetchedPage, SearchResultItem, filter_result_domains},
};
pub use super::route_attempts::provider_entry_label;
use super::route_attempts::{attempt_route_label, route_attempt_entries};
use super::types::{
    RoutingStrategy, SearchAttempt, SearchBlockReason, SearchDetails, SearchProvider,
    SearchProviderConfig, SearchProviderEntry, SearchRequest, WebsearchConfig,
};
use maho_tools::definition::{AbortSignal, ToolError};
use serde_json::{Value, json};
use std::{collections::HashMap, time::Duration};
use tokio::time::Instant;
/// First cooldown after a keyless engine blocks a search; each consecutive block doubles it up to the cap.
pub const ENGINE_COOLDOWN_BASE_MS: f64 = 60_000.;
pub const ENGINE_COOLDOWN_MAX_MS: f64 = 15. * 60_000.;
/// Engines that need no key or account; a block on any of them puts it on a per-session cooldown.
pub fn is_keyless_provider(provider: SearchProvider) -> bool {
    matches!(
        provider,
        SearchProvider::DuckduckgoHtml
            | SearchProvider::Startpage
            | SearchProvider::Mojeek
            | SearchProvider::Ecosia
            | SearchProvider::GoogleHtml
            | SearchProvider::ExaMcp
            | SearchProvider::Searxng
            | SearchProvider::Public
            | SearchProvider::Keenable
    )
}
fn engine_name(provider: SearchProvider) -> &'static str {
    match provider {
        SearchProvider::DuckduckgoHtml => "DuckDuckGo",
        SearchProvider::Startpage => "Startpage",
        SearchProvider::Mojeek => "Mojeek",
        SearchProvider::Ecosia => "Ecosia",
        SearchProvider::GoogleHtml => "Google",
        SearchProvider::ExaMcp => "Exa MCP",
        SearchProvider::Searxng => "SearXNG",
        SearchProvider::Keenable => "Keenable",
        other => other.as_str(),
    }
}
const fn block_description(reason: SearchBlockReason) -> &'static str {
    match reason {
        SearchBlockReason::Challenge => "a bot challenge",
        SearchBlockReason::RateLimited => "a rate limit",
        SearchBlockReason::Forbidden => "a refusal",
        SearchBlockReason::Network => "a network error",
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EngineCooldown {
    pub failures: u32,
    pub until: f64,
    pub reason: SearchBlockReason,
}
/// Wall-clock milliseconds; tests replace it so cooldown expiry is deterministic.
pub type Clock = std::sync::Arc<dyn Fn() -> f64 + Send + Sync>;
pub fn system_clock() -> Clock {
    std::sync::Arc::new(now_ms)
}
#[derive(Clone)]
pub struct SearchRoutingState {
    pub round_robin_cursor: usize,
    pub success_counts: Vec<usize>,
    /// Keyed by provider entry label; carried over when the provider list changes so a block outlives it.
    pub cooldowns: HashMap<String, EngineCooldown>,
    pub clock: Clock,
}
impl std::fmt::Debug for SearchRoutingState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchRoutingState")
            .field("round_robin_cursor", &self.round_robin_cursor)
            .field("success_counts", &self.success_counts)
            .field("cooldowns", &self.cooldowns)
            .finish_non_exhaustive()
    }
}
pub fn create_search_routing_state(provider_count: usize) -> SearchRoutingState {
    create_search_routing_state_with(provider_count, HashMap::new(), system_clock())
}
pub fn create_search_routing_state_with(
    provider_count: usize,
    cooldowns: HashMap<String, EngineCooldown>,
    clock: Clock,
) -> SearchRoutingState {
    SearchRoutingState {
        round_robin_cursor: 0,
        success_counts: vec![0; provider_count],
        cooldowns,
        clock,
    }
}
fn entry_label(entry: &SearchProviderEntry) -> String {
    provider_entry_label(
        entry.config.provider.as_str(),
        entry.config.id.as_deref(),
        None,
    )
}
fn active_cooldown(
    state: &SearchRoutingState,
    provider: &SearchProviderEntry,
    now: f64,
) -> Option<EngineCooldown> {
    if !is_keyless_provider(provider.config.provider) {
        return None;
    }
    state
        .cooldowns
        .get(&entry_label(provider))
        .copied()
        .filter(|cooldown| cooldown.until > now)
}
fn skipped_attempt(
    provider: &SearchProviderEntry,
    cooldown: EngineCooldown,
    now: f64,
) -> SearchAttempt {
    let seconds = ((cooldown.until - now) / 1000.).ceil();
    SearchAttempt {
        provider: provider.config.provider,
        entry_id: provider.config.id.clone(),
        skipped: true,
        blocked: Some(cooldown.reason),
        error: Some(format!(
            "skipped (cooling down for {seconds}s after {})",
            block_description(cooldown.reason)
        )),
        ..Default::default()
    }
}
/// Exponential backoff per keyless engine: a block starts or doubles its cooldown, a success clears it.
fn record_engine_outcome(
    state: &mut SearchRoutingState,
    provider: &SearchProviderEntry,
    details: &SearchDetails,
) {
    if !is_keyless_provider(provider.config.provider) {
        return;
    }
    let key = entry_label(provider);
    if details.error.is_none() {
        state.cooldowns.remove(&key);
        return;
    }
    let Some(reason) = details.blocked else {
        return;
    };
    let failures = state
        .cooldowns
        .get(&key)
        .map_or(0, |cooldown| cooldown.failures)
        + 1;
    let backoff = ENGINE_COOLDOWN_MAX_MS
        .min(ENGINE_COOLDOWN_BASE_MS * 2f64.powi(i32::try_from(failures - 1).unwrap_or(i32::MAX)));
    let requested = ENGINE_COOLDOWN_MAX_MS.min(details.retry_after_seconds.unwrap_or(0.) * 1000.);
    let until = (state.clock)() + backoff.max(requested);
    state.cooldowns.insert(
        key,
        EngineCooldown {
            failures,
            until,
            reason,
        },
    );
}
fn block_for_status(status: u16) -> Option<SearchBlockReason> {
    match status {
        429 => Some(SearchBlockReason::RateLimited),
        403 => Some(SearchBlockReason::Forbidden),
        _ => None,
    }
}
fn retry_after_seconds(headers: &reqwest::header::HeaderMap, now: f64) -> Option<f64> {
    let header = headers.get("retry-after")?.to_str().ok()?.trim();
    if header.is_empty() {
        return None;
    }
    if let Ok(seconds) = header.parse::<f64>()
        && seconds.is_finite()
    {
        return Some(seconds.max(0.));
    }
    let date = httpdate_millis(header)?;
    Some(((date - now) / 1000.).ceil().max(0.))
}
/// IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`), the only form an HTTP server sends in `Retry-After`.
fn httpdate_millis(value: &str) -> Option<f64> {
    let parts = value.split_whitespace().collect::<Vec<_>>();
    let [_, day, month, year, time, "GMT"] = parts.as_slice() else {
        return None;
    };
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|name| name == month)? as i64
        + 1;
    let (day, year) = (day.parse::<i64>().ok()?, year.parse::<i64>().ok()?);
    let clock = time
        .split(':')
        .map(|part| part.parse::<i64>().ok())
        .collect::<Option<Vec<_>>>()?;
    let [hour, minute, second] = clock.as_slice() else {
        return None;
    };
    let (y, m) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 86_400 + hour * 3600 + minute * 60 + second) * 1000) as f64)
}
pub fn select_order(
    strategy: RoutingStrategy,
    providers: &[SearchProviderEntry],
    state: &mut SearchRoutingState,
) -> Vec<usize> {
    match strategy {
        RoutingStrategy::Priority => {
            let mut indices: Vec<_> = (0..providers.len()).collect();
            indices.sort_by(|left, right| {
                let a = providers[*left].priority.unwrap_or(*left as f64);
                let b = providers[*right].priority.unwrap_or(*right as f64);
                (a - b)
                    .partial_cmp(&0.)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(left.cmp(right))
            });
            indices
        }
        RoutingStrategy::RoundRobin => {
            let mut weighted = Vec::new();
            for (index, provider) in providers.iter().enumerate() {
                let weight = provider.weight.unwrap_or(1.).trunc().max(1.) as usize;
                weighted.extend(std::iter::repeat_n(index, weight));
            }
            if weighted.is_empty() {
                weighted.extend(0..providers.len());
            }
            if weighted.is_empty() {
                state.round_robin_cursor = 0;
                return vec![];
            }
            let start = state.round_robin_cursor % weighted.len();
            let mut order = Vec::new();
            for offset in 0..weighted.len() {
                let index = weighted[(start + offset) % weighted.len()];
                if !order.contains(&index) {
                    order.push(index);
                }
            }
            for index in 0..providers.len() {
                if !order.contains(&index) {
                    order.push(index);
                }
            }
            state.round_robin_cursor = (state.round_robin_cursor + 1) % weighted.len();
            order
        }
        RoutingStrategy::FillFirst => {
            let mut selected = 0;
            let mut selected_count = state.success_counts.first().copied().unwrap_or(0);
            for index in 1..providers.len() {
                let count = state.success_counts.get(index).copied().unwrap_or(0);
                if count < selected_count {
                    selected = index;
                    selected_count = count;
                }
            }
            std::iter::once(selected)
                .chain((0..providers.len()).filter(|index| *index != selected))
                .collect()
        }
    }
}
fn error_detail(payload: &Value, body: &str) -> String {
    let detail=payload.get("error").and_then(Value::as_str).filter(|s|!s.is_empty()).or_else(||payload.get("error").and_then(|error|error.get("message")).and_then(Value::as_str).filter(|s|!s.is_empty())).or_else(||payload.get("message").and_then(Value::as_str).filter(|s|!s.is_empty())).unwrap_or_else(||body.trim_matches(|character|matches!(character,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')));
    if detail.encode_utf16().count() <= 500 {
        return detail.into();
    }
    let mut units = 0;
    let mut end = 0;
    for (index, c) in detail.char_indices() {
        if units + c.len_utf16() > 499 {
            break;
        }
        units += c.len_utf16();
        end = index + c.len_utf8();
    }
    format!("{}…", &detail[..end])
}
/// The challenge a keyless engine served instead of results (`detectSearchChallenge`).
fn detect_search_challenge(
    provider: SearchProvider,
    page: &super::providers::shared::FetchedPage,
) -> Option<&'static str> {
    match provider {
        SearchProvider::DuckduckgoHtml => super::providers::duckduckgo_html::detect_challenge(page),
        SearchProvider::Startpage => super::providers::startpage::detect_challenge(page),
        SearchProvider::Mojeek => super::providers::mojeek::detect_challenge(page),
        SearchProvider::Ecosia => super::providers::ecosia::detect_challenge(page),
        SearchProvider::GoogleHtml => super::providers::google_html::detect_challenge(page),
        _ => None,
    }
}
fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0., |elapsed| elapsed.as_secs_f64() * 1000.)
}
const DEFAULT_TIMEOUT_MS: f64 = 60_000.;

struct PageFailure {
    message: String,
    blocked: Option<SearchBlockReason>,
    retry_after_seconds: Option<f64>,
}

struct PageOutcome {
    payload: Value,
    results: Vec<SearchResultItem>,
}

enum FetchFailure {
    Aborted,
    Timeout,
    Network(String),
}

async fn fetch_page(
    built: &BuiltSearchRequest,
    deadline: Instant,
    signal: &AbortSignal,
) -> Result<(FetchedPage, reqwest::header::HeaderMap), FetchFailure> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(FetchFailure::Timeout);
    }
    let future = async {
        let client = reqwest::Client::new();
        let mut outgoing = if built.method == "GET" {
            client.get(&built.url)
        } else {
            client.post(&built.url)
        };
        for (key, value) in &built.headers {
            outgoing = outgoing.header(key, value);
        }
        if let Some(body) = &built.body {
            outgoing = outgoing.body(body.to_string());
        } else if let Some(form) = &built.form {
            outgoing = outgoing.body(form.clone());
        }
        let response = outgoing.send().await.map_err(|error| error.to_string())?;
        let status = response.status();
        let url = response.url().to_string();
        let headers = response.headers().clone();
        let text = response.text().await.map_err(|error| error.to_string())?;
        Ok::<_, String>((status, url, headers, text))
    };
    let fetched = tokio::select! {
        biased;
        ()=signal.cancelled()=>return Err(FetchFailure::Aborted),
        result=tokio::time::timeout(remaining,future)=>match result {
            Ok(result)=>result,
            Err(_)=>return Err(FetchFailure::Timeout),
        }
    };
    match fetched {
        Ok((status, url, headers, text)) => Ok((
            FetchedPage {
                status: status.as_u16(),
                url,
                body: text,
            },
            headers,
        )),
        Err(error) => Err(FetchFailure::Network(error)),
    }
}

fn interpret_page(
    config: &SearchProviderConfig,
    (page, headers): (FetchedPage, reqwest::header::HeaderMap),
) -> Result<PageOutcome, PageFailure> {
    if let Some(challenge) = detect_search_challenge(config.provider, &page) {
        return Err(PageFailure {
            message: format!(
                "{} served a bot challenge ({challenge}) that needs a browser to pass.",
                engine_name(config.provider)
            ),
            blocked: Some(SearchBlockReason::Challenge),
            retry_after_seconds: None,
        });
    }
    let payload =
        if let Some(parsed) = super::providers::parse_search_body(config.provider, &page.body) {
            parsed
        } else if matches!(
            config.provider,
            SearchProvider::DuckduckgoHtml
                | SearchProvider::Startpage
                | SearchProvider::Mojeek
                | SearchProvider::Ecosia
                | SearchProvider::GoogleHtml
        ) {
            json!({"html":page.body,"url":page.url})
        } else if page.body.is_empty() {
            json!({})
        } else {
            serde_json::from_str(&page.body).unwrap_or_else(|_| json!({}))
        };
    if !(200..300).contains(&page.status) {
        let detail = error_detail(&payload, &page.body);
        let message = if detail.is_empty() {
            format!("Search failed with HTTP {}", page.status)
        } else {
            format!("Search failed with HTTP {}: {detail}", page.status)
        };
        return Err(PageFailure {
            message,
            blocked: block_for_status(page.status),
            retry_after_seconds: retry_after_seconds(&headers, now_ms()),
        });
    }
    let results = normalize_search_response(config.provider, &payload);
    Ok(PageOutcome { payload, results })
}

fn payload_error(payload: &Value) -> String {
    payload
        .get("error")
        .map(|error| error_detail(&json!({"error":error}), ""))
        .unwrap_or_default()
}

async fn provider_search(
    config: &SearchProviderEntry,
    request: &SearchRequest,
    signal: Option<&AbortSignal>,
    deadline: Option<Instant>,
) -> Result<SearchDetails, ToolError> {
    let started = Instant::now();
    let signal = signal.cloned().unwrap_or_default();
    signal.check()?;
    let timeout_ms = config
        .config
        .timeout_ms
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .trunc()
        .max(1.) as u64;
    let deadline = deadline.unwrap_or_else(|| started + Duration::from_millis(timeout_ms));
    let filters = DomainFilters::resolve(&config.config, request);
    let failure = |error: String,
                   blocked: Option<SearchBlockReason>,
                   retry_after_seconds: Option<f64>| SearchDetails {
        provider: config.config.provider,
        entry_id: config.config.id.clone(),
        query: request.query.clone(),
        duration_ms: started.elapsed().as_secs_f64() * 1000.,
        error: Some(error),
        blocked,
        retry_after_seconds,
        ..Default::default()
    };
    let limit = (request.max_results >= 0.).then(|| request.max_results.trunc() as usize);
    let page_size = config.config.max_results.unwrap_or(request.max_results);
    let paged = config.config.provider == SearchProvider::Tinyfish;
    let mut collected = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut overflowed = false;
    let mut returned = 0usize;
    let mut page_index = 0usize;
    let error_suffix = loop {
        let built = if page_index == 0 {
            build_search_request(&config.config, request)
                .map_err(|error| ToolError::Message(error.to_string()))?
        } else {
            let ctx = build_context(&config.config, request, &filters);
            let mut page = super::providers::tinyfish::page_request(&ctx, page_index);
            super::providers::shared::with_auth_headers(
                config.config.headers.as_ref(),
                &mut page.headers,
            );
            page
        };
        let fetched = match fetch_page(&built, deadline, &signal).await {
            Ok(fetched) => fetched,
            Err(FetchFailure::Aborted) => return Err(ToolError::Aborted),
            Err(FetchFailure::Timeout) => {
                return Ok(failure(
                    format!("Search timed out after {timeout_ms}ms"),
                    Some(SearchBlockReason::Network),
                    None,
                ));
            }
            Err(FetchFailure::Network(error)) => {
                return Ok(failure(error, Some(SearchBlockReason::Network), None));
            }
        };
        let outcome = match interpret_page(&config.config, fetched) {
            Ok(outcome) => outcome,
            Err(page_failure) => {
                return Ok(failure(
                    page_failure.message,
                    page_failure.blocked,
                    page_failure.retry_after_seconds,
                ));
            }
        };
        returned += outcome.results.len();
        let accepted = if config.config.provider == SearchProvider::OpenRouter {
            let from_config = filter_result_domains(
                outcome.results,
                config.config.allowed_domains.as_deref(),
                config.config.blocked_domains.as_deref(),
            );
            filter_result_domains(
                from_config,
                request.allowed_domains.as_deref(),
                request.blocked_domains.as_deref(),
            )
        } else {
            outcome.results
        };
        for item in accepted {
            if paged && !seen.insert(super::providers::public::public_dedupe_key(&item.url)) {
                continue;
            }
            if limit.is_some_and(|limit| collected.len() >= limit) {
                overflowed = true;
                break;
            }
            collected.push(item);
        }
        let more_pages = paged
            && super::providers::tinyfish::has_next_page(&outcome.payload, page_size, page_index);
        if !more_pages || overflowed || limit.is_some_and(|limit| collected.len() >= limit) {
            break payload_error(&outcome.payload);
        }
        page_index += 1;
    };
    let domain_restricted = [&filters.allowed, &filters.blocked]
        .into_iter()
        .any(|domains| domains.as_ref().is_some_and(|domains| !domains.is_empty()));
    let results = collected;
    if config.config.provider == SearchProvider::OpenRouter
        && results.is_empty()
        && returned > 0
        && domain_restricted
    {
        return Ok(failure(
            super::providers::openrouter::domains_unsupported_error(returned),
            None,
            None,
        ));
    }
    let count = if request.max_results < 0. {
        results
            .len()
            .saturating_sub((-request.max_results).trunc() as usize)
    } else {
        request.max_results.trunc() as usize
    };
    let truncated = overflowed || results.len() > count;
    let mut details = SearchDetails {
        provider: config.config.provider,
        entry_id: config.config.id.clone(),
        query: request.query.clone(),
        truncated,
        results: results.into_iter().take(count).collect(),
        ..Default::default()
    };
    if details.results.is_empty() {
        let label = provider_entry_label(
            config.config.provider.as_str(),
            config.config.id.as_deref(),
            None,
        );
        details.error = Some(if error_suffix.is_empty() {
            format!(
                "Search provider {label} returned no results for \"{}\".",
                request.query
            )
        } else {
            format!("Search provider {label} failed: {error_suffix}")
        });
    }
    details.duration_ms = started.elapsed().as_secs_f64() * 1000.;
    Ok(details)
}

fn public_engine_config(
    config: &SearchProviderConfig,
    engine: SearchProvider,
    timeout_ms: u64,
) -> SearchProviderConfig {
    let mut engine_config = SearchProviderConfig::new(engine);
    engine_config.base_url = config
        .base_url
        .as_ref()
        .map(|base| format!("{}/{}", base.trim_end_matches('/'), engine.as_str()));
    engine_config.max_results = config.max_results;
    engine_config.allowed_domains = config.allowed_domains.clone();
    engine_config.blocked_domains = config.blocked_domains.clone();
    engine_config.timeout_ms = Some(timeout_ms as f64);
    engine_config
}

fn shared_block_reason(blocked: &[Option<SearchBlockReason>]) -> Option<SearchBlockReason> {
    let first = *blocked.first()?;
    first.filter(|_| blocked.iter().all(|reason| *reason == first))
}

async fn public_search(
    config: &SearchProviderEntry,
    request: &SearchRequest,
    signal: Option<&AbortSignal>,
) -> Result<SearchDetails, ToolError> {
    let started = Instant::now();
    let signal = signal.cloned().unwrap_or_default();
    signal.check()?;
    let hard_ms = config
        .config
        .timeout_ms
        .unwrap_or(super::providers::public::PUBLIC_HARD_DEADLINE_MS as f64)
        .trunc()
        .clamp(1., super::providers::public::PUBLIC_HARD_DEADLINE_MS as f64)
        as u64;
    let hard_deadline = started + Duration::from_millis(hard_ms);
    let engines = super::providers::public::PUBLIC_ENGINES;
    let mut tasks = tokio::task::JoinSet::new();
    for (index, engine) in engines.iter().copied().enumerate() {
        let entry = SearchProviderEntry {
            config: public_engine_config(&config.config, engine, hard_ms),
            priority: None,
            weight: None,
            fallback_model: None,
        };
        let request = request.clone();
        let signal = signal.clone();
        tasks.spawn(async move {
            (
                index,
                provider_search(&entry, &request, Some(&signal), Some(hard_deadline)).await,
            )
        });
    }
    finish_public_search(config, request, &signal, started, hard_ms, tasks).await
}

async fn finish_public_search(
    config: &SearchProviderEntry,
    request: &SearchRequest,
    signal: &AbortSignal,
    started: Instant,
    hard_ms: u64,
    mut tasks: tokio::task::JoinSet<(usize, Result<SearchDetails, ToolError>)>,
) -> Result<SearchDetails, ToolError> {
    let hard_deadline = started + Duration::from_millis(hard_ms);
    let soft_deadline = started
        + Duration::from_millis(hard_ms.min(super::providers::public::PUBLIC_SOFT_DEADLINE_MS));
    let count = super::providers::public::PUBLIC_ENGINES.len();
    let mut answers: Vec<Option<Vec<SearchResultItem>>> = vec![None; count];
    let mut failures: Vec<Option<String>> = vec![None; count];
    let mut blocked: Vec<Option<SearchBlockReason>> = vec![None; count];
    let mut answered = 0usize;
    while !tasks.is_empty() {
        let deadline = if answered > 0 {
            soft_deadline
        } else {
            hard_deadline
        };
        let joined = tokio::select! {
            biased;
            ()=signal.cancelled()=>return Err(ToolError::Aborted),
            joined=tasks.join_next()=>joined,
            ()=tokio::time::sleep_until(deadline)=>break,
        };
        match joined {
            Some(Ok((index, Ok(details)))) => {
                if details.error.is_none() {
                    answers[index] = Some(details.results);
                    answered += 1;
                } else {
                    blocked[index] = details.blocked;
                    failures[index] = details.error;
                }
            }
            Some(Ok((_, Err(ToolError::Aborted)))) => return Err(ToolError::Aborted),
            Some(Ok((index, Err(error)))) => failures[index] = Some(error.to_string()),
            Some(Err(_)) => continue,
            None => break,
        }
    }
    let duration_ms = started.elapsed().as_secs_f64() * 1000.;
    let (merged, truncated) =
        super::providers::public::merge_engine_results(&answers, request.max_results);
    let filters = DomainFilters::resolve(&config.config, request);
    let merged = filter_result_domains(
        merged,
        filters.allowed.as_deref(),
        filters.blocked.as_deref(),
    );
    let mut details = SearchDetails {
        provider: SearchProvider::Public,
        entry_id: config.config.id.clone(),
        query: request.query.clone(),
        duration_ms,
        truncated,
        results: merged,
        ..Default::default()
    };
    if details.results.is_empty() {
        details.error = Some(super::providers::public::all_engines_failed_error(
            &failures,
            hard_ms as f64,
        ));
        details.blocked = shared_block_reason(&blocked);
    }
    Ok(details)
}

pub async fn perform_provider_search(
    config: &SearchProviderEntry,
    request: &SearchRequest,
    signal: Option<&AbortSignal>,
) -> Result<SearchDetails, ToolError> {
    if config.config.provider == SearchProvider::Public {
        return public_search(config, request, signal).await;
    }
    provider_search(config, request, signal, None).await
}

pub type SearchAttemptListener<'a> =
    dyn Fn(&str, &[SearchAttempt], &[String]) -> Result<(), ToolError> + Send + Sync + 'a;
fn attempt_from_details(details: &SearchDetails) -> SearchAttempt {
    SearchAttempt {
        provider: details.provider,
        entry_id: details.entry_id.clone().filter(|id| !id.is_empty()),
        model: details.model.clone(),
        duration_ms: details.duration_ms,
        results_count: details.results.len(),
        error: details.error.clone().filter(|error| !error.is_empty()),
        blocked: details.blocked,
        skipped: false,
    }
}
fn route_label(entry: &SearchProviderEntry) -> String {
    attempt_route_label(
        entry.config.provider.as_str(),
        entry.config.id.as_deref(),
        None,
        entry.config.model.as_deref(),
    )
}
/// One configured route: the entry, then its fallback model when the first attempt fails (`searchRoute`).
async fn search_route(
    entry: &SearchProviderEntry,
    request: &SearchRequest,
    signal: Option<&AbortSignal>,
    attempts: &mut Vec<SearchAttempt>,
    labels: &[String],
    on_attempt: Option<&SearchAttemptListener<'_>>,
) -> Result<SearchDetails, ToolError> {
    let mut details = None;
    for variant in route_attempt_entries(entry) {
        if details
            .as_ref()
            .is_some_and(|details: &SearchDetails| details.error.is_none())
        {
            break;
        }
        if let Some(listener) = on_attempt {
            listener(&entry_label(&variant), attempts, labels)?;
        }
        let mut result = perform_provider_search(&variant, request, signal).await?;
        if variant.config.model.is_some() {
            result.model.clone_from(&variant.config.model);
        }
        attempts.push(attempt_from_details(&result));
        details = Some(result);
    }
    details.ok_or_else(|| ToolError::Message("Search route has no attempt".into()))
}
pub async fn perform_search(
    config: &WebsearchConfig,
    request: &SearchRequest,
    signal: Option<&AbortSignal>,
    routing_state: Option<&mut SearchRoutingState>,
    on_attempt: Option<&SearchAttemptListener<'_>>,
) -> Result<SearchDetails, ToolError> {
    let started = Instant::now();
    let mut local = create_search_routing_state(config.providers.len());
    let state = routing_state.unwrap_or(&mut local);
    let order = select_order(config.strategy, &config.providers, state);
    let mut attempts = Vec::new();
    let labels: Vec<_> = order
        .iter()
        .filter_map(|index| config.providers.get(*index))
        .flat_map(|entry| {
            route_attempt_entries(entry)
                .iter()
                .map(route_label)
                .collect::<Vec<_>>()
        })
        .collect();
    let mut collected: Vec<super::types::SearchResultItem> = Vec::new();
    let mut selected = None;
    for index in order {
        let Some(provider) = config.providers.get(index) else {
            continue;
        };
        let now = (state.clock)();
        if let Some(cooldown) = active_cooldown(state, provider, now) {
            attempts.push(skipped_attempt(provider, cooldown, now));
            if !config.fallback {
                break;
            }
            continue;
        }
        let mut details = search_route(
            provider,
            request,
            signal,
            &mut attempts,
            &labels,
            on_attempt,
        )
        .await?;
        record_engine_outcome(state, provider, &details);
        if details.error.is_some() {
            if !config.fallback {
                details.strategy = Some(config.strategy);
                details.attempts = Some(attempts);
                return Ok(details);
            }
            selected = Some(details);
            continue;
        }
        if state.success_counts.len() <= index {
            state.success_counts.resize(index + 1, 0);
        }
        state.success_counts[index] += 1;
        if config.strategy != RoutingStrategy::FillFirst {
            details.strategy = Some(config.strategy);
            details.attempts = Some(attempts);
            return Ok(details);
        }
        for item in &details.results {
            if collected.len() as f64 >= request.max_results {
                break;
            }
            if let Some(existing) = collected
                .iter_mut()
                .find(|existing| existing.url == item.url)
            {
                *existing = item.clone();
            } else {
                collected.push(item.clone());
            }
        }
        selected = Some(details);
        if collected.len() as f64 >= request.max_results {
            break;
        }
    }
    let mut details = selected.unwrap_or_else(|| SearchDetails {
        provider: config
            .providers
            .first()
            .map_or(SearchProvider::Exa, |entry| entry.config.provider),
        query: request.query.clone(),
        error: Some("All configured search providers failed.".into()),
        ..Default::default()
    });
    details.duration_ms = started.elapsed().as_secs_f64() * 1000.;
    details.strategy = Some(config.strategy);
    if !collected.is_empty() {
        details.truncated = collected.len() as f64 >= request.max_results;
        details.results = collected;
        details.error = None;
        details.answer = None;
        details.blocked = None;
        details.retry_after_seconds = None;
    } else {
        details.error = Some(format!(
            "All configured search providers failed: {}",
            attempts
                .iter()
                .map(|attempt| format!(
                    "{} {}",
                    attempt_label(attempt),
                    attempt.error.as_deref().unwrap_or("failed")
                ))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    details.attempts = Some(attempts);
    Ok(details)
}
fn attempt_label(attempt: &SearchAttempt) -> String {
    attempt_route_label(
        attempt.provider.as_str(),
        None,
        attempt.entry_id.as_deref(),
        attempt.model.as_deref(),
    )
}
fn attempt_outcome(attempt: &SearchAttempt) -> String {
    if attempt.skipped {
        return attempt.error.clone().unwrap_or_else(|| "skipped".into());
    }
    if let Some(error) = attempt.error.as_ref().filter(|error| !error.is_empty()) {
        return format!(
            "{}: {error}",
            if attempt.blocked == Some(SearchBlockReason::Challenge) {
                "challenged"
            } else {
                "failed"
            }
        );
    }
    format!(
        "{} result{}",
        attempt.results_count,
        if attempt.results_count == 1 { "" } else { "s" }
    )
}
pub fn format_search_text(details: &SearchDetails) -> String {
    if let Some(error) = &details.error {
        return error.clone();
    }
    if details.results.is_empty() {
        return format!("No web search results found for \"{}\".", details.query);
    }
    let route = attempt_route_label(
        details.provider.as_str(),
        None,
        details.entry_id.as_deref(),
        details.model.as_deref(),
    );
    let mut lines = vec![
        format!("Web search results for \"{}\" via {route}:", details.query),
        String::new(),
    ];
    if let Some(attempts) = details
        .attempts
        .as_ref()
        .filter(|attempts| !attempts.is_empty())
    {
        lines.push(format!(
            "Routing attempts: {}",
            attempts
                .iter()
                .map(|attempt| format!("{} {}", attempt_label(attempt), attempt_outcome(attempt)))
                .collect::<Vec<_>>()
                .join(" -> ")
        ));
        lines.push(String::new());
    }
    for (index, item) in details.results.iter().enumerate() {
        lines.push(format!("{}. {}", index + 1, item.title));
        lines.push(format!("   {}", item.url));
        if let Some(snippet) = item.snippet.as_ref().filter(|snippet| !snippet.is_empty()) {
            lines.push(format!("   {snippet}"));
        }
    }
    lines.push(String::new());
    lines
        .push("REMINDER: Include relevant sources from the URLs above in the final answer.".into());
    lines.join("\n")
}
#[cfg(test)]
mod tests {
    use super::super::types::{SearchProvider, SearchProviderConfig};
    use super::*;
    fn providers() -> Vec<SearchProviderEntry> {
        (0..3)
            .map(|_| SearchProviderEntry {
                config: SearchProviderConfig::new(SearchProvider::Exa),
                priority: None,
                weight: None,
                fallback_model: None,
            })
            .collect()
    }
    #[test]
    fn priority_is_stable_with_default_index() {
        let mut providers = providers();
        providers[2].priority = Some(-1.);
        assert_eq!(
            select_order(
                RoutingStrategy::Priority,
                &providers,
                &mut create_search_routing_state(3)
            ),
            [2, 0, 1]
        );
    }
    #[test]
    fn weighted_round_robin_rotates_unique_providers() {
        let mut providers = providers();
        providers[0].weight = Some(2.);
        let mut state = create_search_routing_state(3);
        assert_eq!(
            select_order(RoutingStrategy::RoundRobin, &providers, &mut state),
            [0, 1, 2]
        );
        assert_eq!(
            select_order(RoutingStrategy::RoundRobin, &providers, &mut state),
            [0, 1, 2]
        );
        assert_eq!(
            select_order(RoutingStrategy::RoundRobin, &providers, &mut state),
            [1, 2, 0]
        );
    }
    #[test]
    fn fill_first_uses_lowest_success_count() {
        let mut state = create_search_routing_state(3);
        state.success_counts = vec![2, 1, 0];
        assert_eq!(
            select_order(RoutingStrategy::FillFirst, &providers(), &mut state),
            [2, 0, 1]
        );
    }
    #[test]
    fn native_labels_preserve_explicit_suffix() {
        assert_eq!(
            provider_entry_label("openai", Some("native-openai-1"), None),
            "openai/native"
        );
        assert_eq!(
            provider_entry_label("openai", Some("x"), Some("custom/native")),
            "custom/native"
        );
    }
    #[tokio::test]
    async fn aborted_provider_never_connects() {
        let signal = AbortSignal::default();
        signal.abort();
        let mut config = providers().remove(0);
        config.config.base_url = Some("http://127.0.0.1:1".into());
        let request = SearchRequest {
            query: "q".into(),
            max_results: 1.,
            allowed_domains: None,
            blocked_domains: None,
        };
        assert!(matches!(
            perform_provider_search(&config, &request, Some(&signal)).await,
            Err(ToolError::Aborted)
        ));
    }
    #[tokio::test]
    async fn cancellation_after_http_request_does_not_fallback() {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let signal = AbortSignal::default();
        let mut entries = providers();
        entries.truncate(2);
        for entry in &mut entries {
            entry.config.base_url =
                Some(format!("http://{}/search", listener.local_addr().unwrap()));
        }
        let config = WebsearchConfig {
            strategy: RoutingStrategy::Priority,
            fallback: true,
            auto: false,
            native_model: None,
            providers: entries,
        };
        let request = SearchRequest {
            query: "q".into(),
            max_results: 1.,
            allowed_domains: None,
            blocked_domains: None,
        };
        let mut state = create_search_routing_state(2);
        let server = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 1024];
            assert!(socket.read(&mut bytes).await.unwrap() > 0);
            signal.abort();
            while socket.read(&mut bytes).await.unwrap() > 0 {}
        };
        let client = async {
            assert!(matches!(
                perform_search(&config, &request, Some(&signal), Some(&mut state), None).await,
                Err(ToolError::Aborted)
            ));
            assert_eq!(state.success_counts, [0, 0]);
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(server, client);
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn real_http_fallback_preserves_attempts_and_success_count() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = async {
            for (status, body) in [
                (500, "{\"error\":{\"message\":\"first failed\"}}"),
                (
                    200,
                    "{\"results\":[{\"title\":\"Found\",\"url\":\"https://example.test\",\"text\":\"snippet\"}]}",
                ),
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0; 1024];
                loop {
                    let n = socket.read(&mut chunk).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&chunk[..n]);
                    if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                assert!(String::from_utf8_lossy(&bytes).starts_with("POST /search"));
                socket.write_all(format!("HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        };
        let mut entries = providers();
        entries.truncate(2);
        for entry in &mut entries {
            entry.config.base_url = Some(format!("http://{address}/search"));
        }
        let config = WebsearchConfig {
            strategy: RoutingStrategy::Priority,
            fallback: true,
            auto: false,
            native_model: None,
            providers: entries,
        };
        let request = SearchRequest {
            query: "q".into(),
            max_results: 1.,
            allowed_domains: None,
            blocked_domains: None,
        };
        let mut state = create_search_routing_state(2);
        let client = async {
            let result = perform_search(&config, &request, None, Some(&mut state), None)
                .await
                .unwrap();
            assert_eq!(result.results[0].title, "Found");
            assert!(result.error.is_none());
            let attempts = result.attempts.unwrap();
            assert_eq!(attempts.len(), 2);
            assert_eq!(
                attempts[0].error.as_deref(),
                Some("Search failed with HTTP 500: first failed")
            );
            assert_eq!(state.success_counts, [0, 1]);
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(server, client);
        })
        .await
        .unwrap();
    }
}

#[cfg(test)]
mod public_deadline_tests {
    use super::*;
    use crate::websearch::providers::public::{PUBLIC_ENGINES, PUBLIC_SOFT_DEADLINE_MS};

    #[tokio::test(start_paused = true)]
    async fn a_slow_public_engine_cannot_hold_the_merged_answer() {
        let hard_ms = PUBLIC_SOFT_DEADLINE_MS * 3;
        for fast_answer in [true, false] {
            let mut tasks = tokio::task::JoinSet::new();
            for index in 0..PUBLIC_ENGINES.len() {
                tasks.spawn(async move {
                    if index != 0 || !fast_answer {
                        return std::future::pending().await;
                    }
                    let item = super::super::providers::shared::result(
                        Some("result"),
                        Some("https://example.test/found"),
                        None,
                        None,
                        None,
                    )
                    .expect("item");
                    (
                        index,
                        Ok(SearchDetails {
                            results: vec![item],
                            ..Default::default()
                        }),
                    )
                });
            }
            let entry = SearchProviderEntry {
                config: SearchProviderConfig::new(SearchProvider::Public),
                priority: None,
                weight: None,
                fallback_model: None,
            };
            let request = SearchRequest {
                query: "example".into(),
                max_results: 10.,
                allowed_domains: None,
                blocked_domains: None,
            };
            let start = Instant::now();
            let details = finish_public_search(
                &entry,
                &request,
                &AbortSignal::default(),
                start,
                hard_ms,
                tasks,
            )
            .await
            .expect("public search");
            let expected = Duration::from_millis(if fast_answer {
                PUBLIC_SOFT_DEADLINE_MS
            } else {
                hard_ms
            });
            assert!((expected..=expected + Duration::from_millis(1)).contains(&start.elapsed()));
            assert_eq!(details.error.is_none(), fast_answer);
            assert_eq!(details.results.len(), usize::from(fast_answer));
            if fast_answer {
                assert_eq!(details.results[0].url, "https://example.test/found");
            }
        }
    }
}
