use super::browser_skill::{
    BrowserMode, BrowserSkillOptions, ConnectedBrowser, PageRenderOptions, PageRenderer,
    RenderError, RenderedPage, WebfetchBrowserSkip, WebfetchBrowserSkipReason,
    create_browser_skill_renderer,
};
use super::content::js_whitespace;
use super::fetcher::{FetchResult, WebfetchFormat};
use super::signed_in_hosts::{
    SIGNED_IN_HOSTS_SETTING, SignedInBrowserSettings, is_signed_in_host,
    read_signed_in_browser_settings,
};
use std::sync::Arc;

const BROWSER_ENV: &str = "PI_WEBFETCH_BROWSER";
// Share of the call's timeout the signed-in read may spend, so a skip still leaves time for the anonymous fetch.
const SIGNED_IN_BUDGET_SHARE: f64 = 0.6;
// A rendered read gets at least this long; with less of the call's timeout left it is skipped.
const MIN_RENDER_BUDGET_MS: u64 = 3_000;

pub fn requires_html_conversion(content_type: &str, format: WebfetchFormat) -> bool {
    let content_type = content_type.to_lowercase();
    format != WebfetchFormat::Html
        && (content_type.contains("text/html") || content_type.contains("application/xhtml+xml"))
}
pub fn parameters() -> serde_json::Value {
    serde_json::json!({"type":"object","properties":{"url":{"type":"string","description":"The URL to fetch content from"},"format":{"type":"string","enum":["markdown","text","html"],"description":"The format to return the content in. Defaults to markdown."},"timeout":{"type":"number","description":"Optional timeout in seconds. Defaults to 30; a missing, zero or negative value means 30. Maximum 120."}},"required":["url"]})
}
pub const DEFAULT_OUTPUT_MAX_BYTES: usize = 50 * 1024;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebfetchOutputCap {
    pub text: String,
    pub notice: Option<String>,
    pub truncated: bool,
    pub output_bytes: usize,
    pub total_bytes: usize,
}
pub fn parse_webfetch_format(value: Option<&str>) -> WebfetchFormat {
    match value {
        Some("text") => WebfetchFormat::Text,
        Some("html") => WebfetchFormat::Html,
        _ => WebfetchFormat::Markdown,
    }
}
fn format_byte_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
fn take_head_bytes(text: &str, max_bytes: usize) -> String {
    let mut lines = vec![];
    let mut used = 0;
    for line in text.split('\n') {
        let bytes = line.len() + usize::from(!lines.is_empty());
        if used + bytes > max_bytes {
            break;
        }
        lines.push(line);
        used += bytes;
    }
    if !lines.is_empty() {
        return lines.join("\n");
    }
    let mut end = max_bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let prefix = &text[..end];
    if end == max_bytes && prefix.ends_with('\u{fffd}') {
        prefix.strip_suffix('\u{fffd}').unwrap_or(prefix).into()
    } else {
        prefix.into()
    }
}
pub fn cap_webfetch_output(text: &str) -> WebfetchOutputCap {
    let total_bytes = text.len();
    if total_bytes <= DEFAULT_OUTPUT_MAX_BYTES {
        return WebfetchOutputCap {
            text: text.into(),
            notice: None,
            truncated: false,
            output_bytes: total_bytes,
            total_bytes,
        };
    }
    let head = take_head_bytes(text, DEFAULT_OUTPUT_MAX_BYTES);
    let output_bytes = head.len();
    let notice = format!(
        "[Output truncated: {} of {} shown ({} limit). Re-fetch a more specific URL or use web_search for targeted content.]",
        format_byte_size(output_bytes),
        format_byte_size(total_bytes),
        format_byte_size(DEFAULT_OUTPUT_MAX_BYTES)
    );
    WebfetchOutputCap {
        text: head,
        notice: Some(notice),
        truncated: true,
        output_bytes,
        total_bytes,
    }
}
#[derive(Clone, Default)]
pub struct WebfetchToolOptions {
    /// Reads app-shell pages rendered; defaults to the user's BrowserSkill-connected browser.
    pub renderer: Option<Arc<dyn PageRenderer>>,
    /// The user's signed-in hosts; defaults to `webfetch.signedInBrowser` in the global settings file.
    pub signed_in_settings: Option<Arc<dyn Fn() -> SignedInBrowserSettings + Send + Sync>>,
    /// Overrides `PI_WEBFETCH_BROWSER` in tests without mutating process environment variables.
    pub browser_enabled: Option<bool>,
}

pub fn parse_browser_enabled(value: Option<&str>) -> bool {
    let Some(value) = value else {
        return true;
    };
    !matches!(
        value.trim_matches(js_whitespace).to_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

fn is_browser_enabled(override_enabled: Option<bool>) -> bool {
    override_enabled
        .unwrap_or_else(|| parse_browser_enabled(std::env::var(BROWSER_ENV).ok().as_deref()))
}

enum RenderOutcome {
    Rendered {
        text: String,
        url: String,
        browser: ConnectedBrowser,
        truncated: bool,
    },
    Skipped(WebfetchBrowserSkip),
}

async fn convert_html(format: WebfetchFormat, html: &str, url: &str) -> String {
    match format {
        WebfetchFormat::Markdown => super::content_lazy::html_to_markdown(html, url).await,
        WebfetchFormat::Text => super::content_lazy::html_to_text(html, url).await,
        WebfetchFormat::Html => html.to_owned(),
    }
}

/// Renders an unlisted app shell in an incognito window; any failure keeps the static text and says why.
async fn read_rendered(
    renderer: &dyn PageRenderer,
    url: &str,
    budget_ms: u64,
    format: WebfetchFormat,
    static_text: &str,
    signal: &maho_tools::definition::AbortSignal,
) -> Result<RenderOutcome, maho_tools::definition::ToolError> {
    if budget_ms < MIN_RENDER_BUDGET_MS {
        return Ok(RenderOutcome::Skipped(WebfetchBrowserSkip {
            reason: WebfetchBrowserSkipReason::NoTimeLeft,
            cause: None,
            message: "no time was left in the request timeout to render it".into(),
            detail: None,
        }));
    }
    match renderer
        .render(
            url,
            PageRenderOptions {
                mode: BrowserMode::Incognito,
                timeout_ms: budget_ms,
                signal: Some(signal.clone()),
                browser: None,
            },
        )
        .await
    {
        Ok(outcome) => {
            let stripped =
                super::content_lazy::strip_scripts(&outcome.page.html, &outcome.page.url).await;
            let text = convert_html(format, &stripped, &outcome.page.url).await;
            if text.trim_matches(js_whitespace).encode_utf16().count()
                <= static_text
                    .trim_matches(js_whitespace)
                    .encode_utf16()
                    .count()
            {
                return Ok(RenderOutcome::Skipped(WebfetchBrowserSkip {
                    reason: WebfetchBrowserSkipReason::NoMoreText,
                    cause: None,
                    message: "the rendered page had no more text".into(),
                    detail: None,
                }));
            }
            Ok(RenderOutcome::Rendered {
                text,
                url: outcome.page.url,
                browser: outcome.browser,
                truncated: outcome.page.truncated,
            })
        }
        Err(RenderError::Aborted) => Err(maho_tools::definition::ToolError::Aborted),
        Err(error) => {
            if signal.is_aborted() {
                return Err(maho_tools::definition::ToolError::Aborted);
            }
            Ok(RenderOutcome::Skipped(match error {
                RenderError::Skip(skip) => skip,
                RenderError::Other(message) => WebfetchBrowserSkip {
                    reason: WebfetchBrowserSkipReason::RenderFailed,
                    cause: None,
                    message: "the browser read failed".into(),
                    detail: Some(message),
                },
                RenderError::Aborted => unreachable!("handled above"),
            }))
        }
    }
}

fn render_notice(
    rendered: bool,
    renderer_name: &str,
    skipped: Option<&WebfetchBrowserSkip>,
) -> Option<String> {
    if rendered {
        return Some(format!(
            "[The static HTML was an empty app shell, so this page was read in {renderer_name}, in an incognito window without your cookies.]"
        ));
    }
    let skipped = skipped?;
    let detail = skipped
        .detail
        .as_deref()
        .map_or_else(String::new, |detail| format!(" ({detail})"));
    let reason = serde_json::to_value(skipped.reason)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "render_failed".into());
    Some(format!(
        "[The static HTML looks like an empty app shell that needs JavaScript; {reason}: {}{detail}.]",
        skipped.message
    ))
}

enum SignedInOutcome {
    Read {
        page: RenderedPage,
        browser: ConnectedBrowser,
    },
    Skipped(WebfetchBrowserSkip),
}

/// Reads a listed host in a normal session of the user's browser; any failure falls back to the anonymous read.
async fn read_signed_in(
    renderer: &dyn PageRenderer,
    url: &str,
    settings: &SignedInBrowserSettings,
    timeout_ms: u64,
    signal: &maho_tools::definition::AbortSignal,
) -> Result<SignedInOutcome, maho_tools::definition::ToolError> {
    match renderer
        .render(
            url,
            PageRenderOptions {
                mode: BrowserMode::SignedIn,
                timeout_ms,
                signal: Some(signal.clone()),
                browser: settings.browser.clone(),
            },
        )
        .await
    {
        Ok(outcome) => {
            if is_signed_in_host(&outcome.page.url, &settings.hosts) {
                return Ok(SignedInOutcome::Read {
                    page: outcome.page,
                    browser: outcome.browser,
                });
            }
            let host = url::Url::parse(&outcome.page.url)
                .ok()
                .and_then(|parsed| {
                    parsed.host_str().map(|host| match parsed.port() {
                        Some(port) => format!("{host}:{port}"),
                        None => host.to_owned(),
                    })
                })
                .unwrap_or_else(|| outcome.page.url.clone());
            Ok(SignedInOutcome::Skipped(WebfetchBrowserSkip {
                reason: WebfetchBrowserSkipReason::HostNotAllowed,
                cause: None,
                message: "the signed-in read ended on a host outside the list, so it was discarded"
                    .into(),
                detail: Some(format!("{host} is not in {SIGNED_IN_HOSTS_SETTING}")),
            }))
        }
        Err(RenderError::Aborted) => Err(maho_tools::definition::ToolError::Aborted),
        Err(error) => {
            if signal.is_aborted() {
                return Err(maho_tools::definition::ToolError::Aborted);
            }
            Ok(SignedInOutcome::Skipped(match error {
                RenderError::Skip(skip) => skip,
                RenderError::Other(message) => WebfetchBrowserSkip {
                    reason: WebfetchBrowserSkipReason::RenderFailed,
                    cause: None,
                    message: "the browser read failed".into(),
                    detail: Some(message),
                },
                RenderError::Aborted => unreachable!("handled above"),
            }))
        }
    }
}

fn page_as_fetch_result(page: &RenderedPage, html: String) -> FetchResult {
    let body = html.into_bytes();
    let bytes = body.len();
    FetchResult {
        url: page.url.clone(),
        status: 200,
        status_text: "OK".into(),
        content_type: "text/html".into(),
        bytes,
        body,
        truncated: page.truncated,
    }
}

fn signed_in_notice(
    read: bool,
    renderer_name: &str,
    skipped: Option<&WebfetchBrowserSkip>,
) -> Option<String> {
    if read {
        return Some(format!(
            "[Read through {renderer_name} with your signed-in session, because you listed this host.]"
        ));
    }
    let skipped = skipped?;
    let detail = skipped
        .detail
        .as_deref()
        .map_or_else(String::new, |detail| format!(" ({detail})"));
    let reason = serde_json::to_value(skipped.reason)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "render_failed".into());
    Some(format!(
        "[This host is listed for signed-in reads, but it was read anonymously; {reason}: {}{detail}.]",
        skipped.message
    ))
}

pub fn create_webfetch_tool() -> maho_tools::definition::ToolDefinition {
    create_webfetch_tool_with(WebfetchToolOptions::default())
}

pub fn create_webfetch_tool_with(
    options: WebfetchToolOptions,
) -> maho_tools::definition::ToolDefinition {
    use maho_tools::definition::{ToolContent, ToolDefinition, ToolError, ToolResult};
    use serde_json::json;
    use std::sync::Mutex;
    let renderer = options
        .renderer
        .unwrap_or_else(|| create_browser_skill_renderer(BrowserSkillOptions::default()));
    let signed_in_settings = options
        .signed_in_settings
        .unwrap_or_else(|| Arc::new(|| read_signed_in_browser_settings(None)));
    let browser_enabled = options.browser_enabled;
    let mut tool = ToolDefinition::new(
        "webfetch",
        "Fetches content from a URL and returns it as markdown, plain text, or HTML. A response over 5MB fails with an error instead of returning partial content, and the text handed back is capped at 50KB with a truncation notice.",
        parameters(),
        Arc::new(move |call| {
            let renderer = Arc::clone(&renderer);
            let signed_in_settings = Arc::clone(&signed_in_settings);
            Box::pin(async move {
                let url = call.params["url"]
                    .as_str()
                    .ok_or_else(|| ToolError::Message("url is required".into()))?;
                let format = parse_webfetch_format(call.params["format"].as_str());
                let format_name = match format {
                    WebfetchFormat::Markdown => "markdown",
                    WebfetchFormat::Text => "text",
                    WebfetchFormat::Html => "html",
                };
                let timeout = super::fetcher::clamp_timeout(call.params["timeout"].as_f64());
                let started_instant = std::time::Instant::now();
                let started = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|error| ToolError::Message(error.to_string()))?
                    .as_millis();
                let progress = json!({"activity":format!("fetching {url}"),"startedAt":started,"maxWaitMs":timeout*1000});
                let emit = |phase: &str, bytes: Option<usize>, total: Option<usize>| {
                    if let Some(update) = &call.on_update {
                        let mut details = json!({"phase":phase,"url":url,"format":format_name,"timeoutSeconds":timeout,"progress":progress});
                        if let Some(bytes) = bytes {
                            details["bytesRead"] = json!(bytes);
                        }
                        if let Some(total) = total {
                            details["totalBytes"] = json!(total);
                        }
                        update(ToolResult {
                            content: vec![ToolContent::text(format!(
                                "Fetching {url} as {format_name} (timeout {timeout}s)"
                            ))],
                            details: Some(details),
                            structured_content: None,
                            is_error: None,
                        })?;
                    }
                    Ok::<_, ToolError>(())
                };
                emit("fetching", None, None)?;

                let settings = signed_in_settings();
                let mut signed_in_skipped: Option<WebfetchBrowserSkip> = None;
                let mut signed_in_page: Option<RenderedPage> = None;
                let mut signed_in_browser: Option<ConnectedBrowser> = None;
                if is_signed_in_host(url, &settings.hosts) {
                    if is_browser_enabled(browser_enabled) {
                        emit("rendering", None, None)?;
                        let browser_budget_ms = ((timeout as f64) * 1000.0 * SIGNED_IN_BUDGET_SHARE)
                            .floor()
                            .max(1.0) as u64;
                        match read_signed_in(
                            renderer.as_ref(),
                            url,
                            &settings,
                            browser_budget_ms,
                            &call.signal,
                        )
                        .await?
                        {
                            SignedInOutcome::Read { page, browser } => {
                                signed_in_page = Some(page);
                                signed_in_browser = Some(browser);
                            }
                            SignedInOutcome::Skipped(skip) => {
                                signed_in_skipped = Some(skip);
                            }
                        }
                    } else {
                        signed_in_skipped = Some(WebfetchBrowserSkip {
                            reason: WebfetchBrowserSkipReason::Disabled,
                            cause: None,
                            message: format!("{BROWSER_ENV}=0 turns browser reads off"),
                            detail: None,
                        });
                    }
                }

                let elapsed_ms = started_instant.elapsed().as_millis() as u64;
                let total_budget_ms = timeout * 1000;
                let left_ms = total_budget_ms.saturating_sub(elapsed_ms);
                let left_seconds = ((left_ms as f64) / 1000.0).ceil().max(1.0) as u64;

                let downloaded = Mutex::new(None);
                let fetched = if let Some(page) = &signed_in_page {
                    let stripped = super::content_lazy::strip_scripts(&page.html, &page.url).await;
                    page_as_fetch_result(page, stripped)
                } else {
                    let on_progress = |bytes, total| {
                        *downloaded
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = total;
                        emit("downloading", Some(bytes), total)
                            .map_err(|error| super::errors::WebfetchError::Abort(error.to_string()))
                    };
                    super::fetcher::fetch_url(super::fetcher::FetchOptions {
                        url,
                        format,
                        timeout_seconds: Some(left_seconds as f64),
                        signal: Some(&call.signal),
                        on_progress: Some(&on_progress),
                    })
                    .await
                    .map_err(|error| ToolError::Message(error.to_string()))?
                };
                emit(
                    "converting",
                    Some(fetched.bytes),
                    downloaded
                        .into_inner()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                )?;
                let raw_cow = String::from_utf8_lossy(&fetched.body);
                let raw = raw_cow.strip_prefix('\u{feff}').unwrap_or(&raw_cow);
                let converted = requires_html_conversion(&fetched.content_type, format);
                let mut text = if converted {
                    convert_html(format, raw, &fetched.url).await
                } else {
                    raw.to_owned()
                };
                let mut rendered = signed_in_page.is_some();
                let mut render_skipped: Option<WebfetchBrowserSkip> = None;
                let mut browser: Option<ConnectedBrowser> = signed_in_browser;
                let mut render_truncated = false;
                let mut rendered_url: Option<String> = None;

                if converted
                    && signed_in_page.is_none()
                    && super::content_lazy::looks_like_app_shell(raw, &fetched.url).await
                {
                    let budget_ms = total_budget_ms
                        .saturating_sub(started_instant.elapsed().as_millis() as u64);
                    let outcome = if is_browser_enabled(browser_enabled) {
                        emit("rendering", None, None)?;
                        read_rendered(
                            renderer.as_ref(),
                            &fetched.url,
                            budget_ms,
                            format,
                            &text,
                            &call.signal,
                        )
                        .await?
                    } else {
                        RenderOutcome::Skipped(WebfetchBrowserSkip {
                            reason: WebfetchBrowserSkipReason::Disabled,
                            cause: None,
                            message: format!("{BROWSER_ENV}=0 turns browser reads off"),
                            detail: None,
                        })
                    };
                    match outcome {
                        RenderOutcome::Rendered {
                            text: rendered_text,
                            url: next_url,
                            browser: next_browser,
                            truncated: next_truncated,
                        } => {
                            text = rendered_text;
                            rendered = true;
                            browser = Some(next_browser);
                            render_truncated = next_truncated;
                            rendered_url = Some(next_url);
                        }
                        RenderOutcome::Skipped(skip) => {
                            render_skipped = Some(skip);
                        }
                    }
                }

                let capped = cap_webfetch_output(&text);
                let final_url = rendered_url.unwrap_or(fetched.url);
                let mut details = json!({
                    "url": url,
                    "finalUrl": final_url,
                    "format": format_name,
                    "status": fetched.status,
                    "statusText": fetched.status_text,
                    "contentType": fetched.content_type,
                    "bytes": fetched.bytes,
                    "timeoutSeconds": timeout,
                    "converted": converted,
                    "truncated": fetched.truncated || render_truncated,
                    "outputTruncated": capped.truncated,
                    "outputBytes": capped.output_bytes,
                    "outputTotalBytes": capped.total_bytes,
                    "rendered": rendered,
                    "route": if signed_in_page.is_some() { "signed-in-browser" } else { "http" },
                });
                if let Some(skip) = &render_skipped {
                    details["renderSkipped"] = serde_json::to_value(skip)?;
                }
                if let Some(browser) = &browser {
                    details["browser"] = serde_json::to_value(browser)?;
                }
                if let Some(skip) = &signed_in_skipped {
                    details["signedInSkipped"] = serde_json::to_value(skip)?;
                }

                let mut notices = Vec::new();
                if let Some(notice) = capped.notice {
                    notices.push(notice);
                }
                if signed_in_page.is_none()
                    && let Some(notice) =
                        render_notice(rendered, renderer.name(), render_skipped.as_ref())
                {
                    notices.push(notice);
                }
                if let Some(notice) = signed_in_notice(
                    signed_in_page.is_some(),
                    renderer.name(),
                    signed_in_skipped.as_ref(),
                ) {
                    notices.push(notice);
                }

                let mut content = vec![ToolContent::text(if notices.is_empty() {
                    capped.text
                } else {
                    format!("{}\n", capped.text)
                })];
                for notice in notices {
                    content.push(ToolContent::Text {
                        text: notice,
                        audience: Some("model".into()),
                    });
                }
                Ok(ToolResult {
                    content,
                    details: Some(details),
                    structured_content: None,
                    is_error: None,
                })
            })
        }),
    );
    tool.workspace_effect = Some(maho_tools::definition::ToolWorkspaceEffect::Read);
    tool.label = "Web Fetch".into();
    tool.prompt_snippet = Some("webfetch: retrieve URL content as markdown, text, or html".into());
    tool.prompt_guidelines = Some(vec![
        "Use webfetch when a specific URL must be retrieved.".into(),
        "Prefer markdown format unless raw HTML or plain text is explicitly needed.".into(),
        "The tool is read-only and does not modify files.".into(),
    ]);
    tool
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn native_executor_converts_html_after_redirect_using_final_url() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for format in ["markdown", "text", "html"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let body = "<title>Article</title><div class='entry-content'><p>A sufficiently long explicit article paragraph for the native fetch test.</p><a href='child'>Link</a></div>";
            let server = async {
                for redirect in [true, false] {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut request = [0; 4096];
                    assert!(socket.read(&mut request).await.unwrap() > 0);
                    if redirect {
                        socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: /posts/final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                    } else {
                        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                    }
                }
            };
            let client = async {
                (create_webfetch_tool().execute)(maho_tools::definition::ToolCall {
                    id: "html",
                    params: serde_json::json!({"url":format!("{origin}/start"),"format":format}),
                    signal: Default::default(),
                    context: None,
                    on_update: None,
                })
                .await
                .unwrap()
            };
            let (_, result) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                tokio::join!(server, client)
            })
            .await
            .unwrap();
            let maho_tools::definition::ToolContent::Text { text, .. } = &result.content[0] else {
                panic!("expected text")
            };
            assert_eq!(
                result.details.as_ref().unwrap()["converted"],
                format != "html"
            );
            assert_eq!(
                result.details.as_ref().unwrap()["finalUrl"],
                format!("{origin}/posts/final")
            );
            match format {
                "markdown" => assert!(text.contains(&format!("[Link]({origin}/posts/child)"))),
                "text" => {
                    assert!(text.starts_with("Article\n\n"));
                    assert!(!text.contains("<p>"));
                }
                _ => assert_eq!(text, body),
            }
        }
    }
    #[tokio::test]
    async fn native_output_caps_match_upstream_single_line_multiline_and_small_cases() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let bodies = [
            format!("{{\"data\":\"{}\"}}", "x".repeat(2 * 1024 * 1024)),
            (0..5000)
                .map(|index| format!("line-{index}-{}\n", "y".repeat(40)))
                .collect::<String>(),
            "hello world\nsecond line\n".into(),
        ];
        for (index, body) in bodies.into_iter().enumerate() {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/", listener.local_addr().unwrap());
            let server = async {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                assert!(socket.read(&mut request).await.unwrap() > 0);
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
                socket.write_all(body.as_bytes()).await.unwrap();
            };
            let client = async {
                (create_webfetch_tool().execute)(maho_tools::definition::ToolCall {
                    id: "cap",
                    params: serde_json::json!({"url":url,"format":"text"}),
                    signal: Default::default(),
                    context: None,
                    on_update: None,
                })
                .await
                .unwrap()
            };
            let (_, result) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                tokio::join!(server, client)
            })
            .await
            .unwrap();
            let details = result.details.unwrap();
            assert_eq!(details["outputTotalBytes"], body.len());
            assert_eq!(details["outputTruncated"], index < 2);
            let maho_tools::definition::ToolContent::Text { text, .. } = &result.content[0] else {
                panic!("expected fetched text")
            };
            if index < 2 {
                assert!(
                    details["outputBytes"].as_u64().unwrap() <= DEFAULT_OUTPUT_MAX_BYTES as u64
                );
                assert!(text.starts_with(if index == 0 {
                    "{\"data\":\"xxxx"
                } else {
                    "line-0-"
                }));
                assert!(
                    matches!(&result.content[1],maho_tools::definition::ToolContent::Text{audience:Some(audience),..} if audience=="model")
                );
            } else {
                assert_eq!(text, &body);
                assert_eq!(result.content.len(), 1);
            }
        }
    }
    #[tokio::test]
    async fn native_executor_fetches_text_and_emits_progress_metadata() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 4096];
            assert!(socket.read(&mut buffer).await.unwrap() > 0);
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello").await.unwrap();
        };
        let updates = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let capture = updates.clone();
        let client = async {
            let result = (create_webfetch_tool().execute)(maho_tools::definition::ToolCall {
                id: "fetch",
                params: serde_json::json!({"url":url}),
                signal: Default::default(),
                context: None,
                on_update: Some(std::sync::Arc::new(move |update| {
                    capture.lock().unwrap().push(update);
                    Ok(())
                })),
            })
            .await
            .unwrap();
            assert_eq!(
                result.content,
                [maho_tools::definition::ToolContent::text("hello")]
            );
            let details = result.details.unwrap();
            assert_eq!(details["status"], 200);
            assert_eq!(details["outputBytes"], 5);
            assert_eq!(details["converted"], false);
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(server, client);
        })
        .await
        .unwrap();
        let updates = updates.lock().unwrap();
        assert_eq!(
            updates.first().unwrap().details.as_ref().unwrap()["phase"],
            "fetching"
        );
        assert!(
            updates
                .iter()
                .any(|update| update.details.as_ref().unwrap()["phase"] == "downloading")
        );
        assert_eq!(
            updates.last().unwrap().details.as_ref().unwrap()["phase"],
            "converting"
        );
    }
    #[test]
    fn only_html_text_and_markdown_require_conversion() {
        assert!(requires_html_conversion(
            "Text/HTML; charset=utf-8",
            WebfetchFormat::Markdown
        ));
        assert!(requires_html_conversion(
            "application/xhtml+xml",
            WebfetchFormat::Text
        ));
        assert!(!requires_html_conversion("text/html", WebfetchFormat::Html));
        assert!(!requires_html_conversion(
            "application/json",
            WebfetchFormat::Markdown
        ));
    }
    #[test]
    fn small_output_is_unchanged() {
        assert_eq!(
            cap_webfetch_output("hello"),
            WebfetchOutputCap {
                text: "hello".into(),
                notice: None,
                truncated: false,
                output_bytes: 5,
                total_bytes: 5
            }
        );
    }
    #[test]
    fn whole_lines_are_kept() {
        let input = format!("first\n{}", "x".repeat(DEFAULT_OUTPUT_MAX_BYTES));
        let result = cap_webfetch_output(&input);
        assert_eq!(result.text, "first");
        assert_eq!(result.output_bytes, 5);
        assert!(result.truncated);
    }
    #[test]
    fn oversized_first_line_keeps_utf8_safe_prefix() {
        let result = cap_webfetch_output(&"한".repeat(DEFAULT_OUTPUT_MAX_BYTES));
        assert_eq!(result.output_bytes, DEFAULT_OUTPUT_MAX_BYTES - 2);
        assert!(result.text.chars().all(|c| c == '한'));
    }
    #[test]
    fn exact_ceiling_is_not_truncated() {
        assert!(!cap_webfetch_output(&"x".repeat(DEFAULT_OUTPUT_MAX_BYTES)).truncated);
    }
    #[test]
    fn unknown_format_defaults_to_markdown() {
        assert_eq!(
            parse_webfetch_format(Some("invalid")),
            WebfetchFormat::Markdown
        );
        assert_eq!(parse_webfetch_format(None), WebfetchFormat::Markdown);
    }
}
