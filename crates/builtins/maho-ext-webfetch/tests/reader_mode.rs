//! Port of senpi `test/suite/webfetch-reader-mode.test.ts`, `test/suite/webfetch-tistory-reader-mode.test.ts`
//! and `test/webfetch-renderers.test.ts` (pin 903b8336). Each case drives the registered native
//! executor over a real local HTTP listener, as upstream drives `webfetch.execute` against `node:http`.

use maho_tools::definition::{ToolCall, ToolContent};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Serves `pages` in order, one connection each, and returns the request heads it saw.
async fn serve(
    listener: tokio::net::TcpListener,
    pages: Vec<(u16, &'static str, String)>,
) -> Vec<String> {
    let mut requests = Vec::new();
    for (status, headers, body) in pages {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut buffer = [0u8; 4096];
        let mut request = Vec::new();
        loop {
            let read = socket.read(&mut buffer).await.expect("read");
            assert!(read > 0, "request closed before its head");
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        requests.push(String::from_utf8_lossy(&request).into_owned());
        let response = format!(
            "HTTP/1.1 {status} X\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.expect("write");
    }
    requests
}

async fn fetch(url: String, format: &str) -> String {
    let result = (maho_ext_webfetch::webfetch::tool::create_webfetch_tool().execute)(ToolCall {
        id: "tool",
        params: serde_json::json!({"url": url, "format": format}),
        signal: Default::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("webfetch");
    let ToolContent::Text { text, .. } = &result.content[0] else {
        panic!("expected text content")
    };
    text.clone()
}

/// One page fetched through the real tool; returns the converted text and the request heads.
async fn fetch_pages(
    path: &str,
    format: &str,
    pages: Vec<(u16, &'static str, String)>,
) -> (String, Vec<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let url = format!("http://{}{path}", listener.local_addr().expect("address"));
    let (requests, text) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(serve(listener, pages), fetch(url, format))
    })
    .await
    .expect("bounded fetch");
    (text, requests)
}

const HTML: &str = "Content-Type: text/html; charset=utf-8\r\n";

fn reader_fixture_html() -> String {
    r#"<!doctype html>
		<html>
			<head>
				<title>Fixture chrome title</title>
				<meta name="description" content="Fixture promo summary">
				<style>.ad { color: red; }</style>
			</head>
			<body>
				<header>Fixture subscribe banner</header>
				<nav><a href="/latest">Latest fixture link</a></nav>
				<main>
					<article>
						<h1>Readable Fixture Article</h1>
						<p>Alpha fixture paragraph with enough words to be selected as the central article content.</p>
						<p>Beta fixture paragraph should remain after reader cleanup.</p>
					</article>
				</main>
				<aside>Fixture sponsored sidebar</aside>
				<footer>Fixture footer legal links</footer>
				<script>window.fixtureTracker = true;</script>
			</body>
		</html>"#
        .into()
}

fn assert_reader_chrome_removed(text: &str) {
    assert!(text.contains("Alpha fixture paragraph"), "{text}");
    assert!(text.contains("Beta fixture paragraph"), "{text}");
    for chrome in [
        "Fixture chrome title",
        "Fixture subscribe banner",
        "Latest fixture link",
        "Fixture sponsored sidebar",
        "Fixture footer legal links",
        "fixtureTracker",
    ] {
        assert!(!text.contains(chrome), "{chrome} leaked into {text}");
    }
}

#[tokio::test]
async fn reader_mode_markdown_returns_main_content() {
    let (text, _) = fetch_pages(
        "/article",
        "markdown",
        vec![(200, HTML, reader_fixture_html())],
    )
    .await;
    assert!(text.contains("## Readable Fixture Article"), "{text}");
    assert_reader_chrome_removed(&text);
}

#[tokio::test]
async fn reader_mode_text_returns_main_content() {
    let (text, _) = fetch_pages("/article", "text", vec![(200, HTML, reader_fixture_html())]).await;
    assert!(text.contains("Readable Fixture Article"), "{text}");
    assert_reader_chrome_removed(&text);
}

#[tokio::test]
async fn markdown_fetch_sends_browser_navigation_headers() {
    let (_, requests) = fetch_pages(
        "/article",
        "markdown",
        vec![(200, HTML, reader_fixture_html())],
    )
    .await;
    let head = requests[0].to_lowercase();
    assert!(head.contains("user-agent: mozilla/5.0"));
    assert!(head.contains("accept: text/markdown"));
    assert!(head.contains("accept-language: en-us,en;q=0.9"));
    assert!(head.contains("sec-fetch-mode: navigate"));
    assert!(head.contains("sec-fetch-dest: document"));
    assert!(head.contains("sec-ch-ua-platform: \"windows\""));
}

#[tokio::test]
async fn one_redirect_returns_the_final_response_body() {
    let (text, requests) = fetch_pages(
        "/start",
        "markdown",
        vec![
            (
                302,
                "Location: /final\r\nContent-Type: text/plain; charset=utf-8\r\n",
                "redirecting".into(),
            ),
            (
                200,
                HTML,
                "<html><body><h1>Redirect Complete</h1><p>Final page.</p></body></html>".into(),
            ),
        ],
    )
    .await;
    assert!(text.contains("# Redirect Complete"), "{text}");
    assert!(text.contains("Final page."));
    let paths: Vec<_> = requests
        .iter()
        .map(|head| head.split_whitespace().nth(1).expect("path").to_owned())
        .collect();
    assert_eq!(paths, ["/start", "/final"]);
}

#[tokio::test]
async fn too_many_redirects_return_the_final_redirect_response_body() {
    let pages = (0..21)
        .map(|_| {
            (
                302,
                "Location: /loop\r\nContent-Type: text/plain; charset=utf-8\r\n",
                "redirect limit reached".to_owned(),
            )
        })
        .collect();
    let (text, requests) = fetch_pages("/loop", "text", pages).await;
    assert!(text.contains("redirect limit reached"), "{text}");
    assert_eq!(requests.len(), 21);
}

fn tistory_body(header_title: &str, script: &str) -> String {
    format!(
        r#"<!doctype html>
		<html>
			<head>
				<title>Admin menu must not beat the title</title>
				<meta name="description" content="Tistory blog promotional tagline">
			</head>
			<body class="tt-body-page">
				<header>
					{header_title}
					<a href="/manage">Manage</a>
					<a href="/category">View all categories</a>
				</header>
				<section class="sidebar">
					<h2>Recent posts</h2>
					<p>A long irrelevant sidebar description must not make the reader mistake this area for the body.</p>
				</section>
				<div id="content">
					<h1 class="tit_post">We must read the Tistory article body</h1>
					<div class="entry-content contents_style">
						<div class="article_view tt_article_useless_p_margin">
							<p data-ke-size="size16">The first body sentence must remain even in a short Tistory post.</p>
							<p data-ke-size="size16">The second body sentence must take precedence over categories or related posts.</p>
							<figure data-ke-type="image">
								<figcaption>Body image captions are preserved too.</figcaption>
							</figure>
						</div>
						<div class="another_category">
							<h4>See other posts</h4>
							<ul>
								<li>Related post title one</li>
								<li>Related post title two</li>
							</ul>
						</div>
					</div>
				</div>
				<footer>Subscribe footer and guestbook link</footer>
				{script}
			</body>
		</html>"#
    )
}

#[tokio::test]
async fn tistory_markdown_prefers_the_article_body_over_category_chrome() {
    let html = tistory_body("", "<script>window.tistoryTracker = true;</script>");
    let (text, _) = fetch_pages("/tistory", "markdown", vec![(200, HTML, html)]).await;
    for kept in [
        "# We must read the Tistory article body",
        "The first body sentence must remain",
        "The second body sentence must take precedence",
        "Body image captions are preserved too",
    ] {
        assert!(text.contains(kept), "{kept} missing from {text}");
    }
    for dropped in [
        "Admin menu must not beat the title",
        "View all categories",
        "Recent posts",
        "Related post title",
        "Subscribe footer",
        "tistoryTracker",
    ] {
        assert!(!text.contains(dropped), "{dropped} leaked into {text}");
    }
}

#[tokio::test]
async fn tistory_title_chrome_loses_to_the_article_title_in_markdown_and_text() {
    for format in ["markdown", "text"] {
        let html = tistory_body("<h1>Blog name</h1>", "");
        let (text, _) = fetch_pages("/tistory-title", format, vec![(200, HTML, html)]).await;
        if format == "markdown" {
            assert!(
                text.contains("# We must read the Tistory article body"),
                "{text}"
            );
        } else {
            assert!(
                text.starts_with("We must read the Tistory article body"),
                "{text}"
            );
        }
        assert!(text.contains("The first body sentence must remain"));
        assert!(text.contains("The second body sentence must take precedence"));
        assert!(!text.contains("Blog name"));
        assert!(!text.contains("irrelevant sidebar description"));
    }
}

#[tokio::test]
async fn tistory_text_preserves_readable_line_breaks() {
    let html = r#"<!doctype html>
		<html>
			<body>
				<div class="article_view">
					<h1>Preserve line breaks</h1>
					<p><span>First line</span><br><span>Second line</span></p>
					<p><span>New paragraph</span> <strong>emphasis</strong></p>
					<ul>
						<li><span>First item</span></li>
						<li><span>Second item</span></li>
					</ul>
					<table>
						<tr><td>Left cell</td><td>Right cell</td></tr>
					</table>
				</div>
			</body>
		</html>"#;
    let (text, _) = fetch_pages("/newline", "text", vec![(200, HTML, html.into())]).await;
    assert!(
        text.contains("Preserve line breaks\n\nFirst line\nSecond line\n\nNew paragraph emphasis"),
        "{text}"
    );
    assert!(text.contains("First item\n\nSecond item"));
    assert!(text.contains("Left cell\nRight cell"));
    assert!(!text.contains("\n\n\n"));
    assert!(!text.contains("First lineSecond line"));
}

#[tokio::test]
async fn literal_entity_examples_keep_one_decoded_layer() {
    let html = r#"<!doctype html>
		<html>
			<body>
				<article>
					<h1>Literal Entity Fixture</h1>
					<p>Rendered tag example: &amp;lt;custom-element&amp;gt;</p>
					<p>Escaped ampersand example: AT&amp;amp;T docs</p>
				</article>
			</body>
		</html>"#;
    for format in ["markdown", "text"] {
        let (text, _) =
            fetch_pages("/literal-entity", format, vec![(200, HTML, html.into())]).await;
        assert!(text.contains("&lt;custom-element&gt;"), "{format}: {text}");
        assert!(text.contains("AT&amp;T docs"), "{format}: {text}");
        assert!(!text.contains("<custom-element>"));
        assert!(!text.contains("AT&T docs"));
    }
}

#[test]
fn progress_renderer_humanizes_fetching_download_and_converting_phases() {
    use maho_ext_api::{AgentToolResult, ToolRenderContext, ToolRendererSession};
    use maho_interactive::theme::{Theme, ThemeBg, ThemeColor, theme::ColorMode};
    use serde_json::json;
    let native = Theme::builtin("dark", ColorMode::Truecolor).expect("builtin theme");
    let theme = maho_ext_api::Theme {
        name: Some(native.name.clone()),
        colors: ThemeColor::ALL
            .iter()
            .map(|color| (color.key().into(), native.get_fg_ansi(*color)))
            .collect(),
        backgrounds: ThemeBg::ALL
            .iter()
            .map(|bg| (bg.key().into(), native.get_bg_ansi(*bg)))
            .collect(),
        vars: Default::default(),
    };
    let render = |details: serde_json::Value| {
        let context = ToolRenderContext {
            args: json!({"url": "https://example.com/large-page", "format": "markdown"}),
            tool_call_id: "progress".into(),
            invalidate: std::rc::Rc::new(|| {}),
            last_component: None,
            state: (),
            cwd: Default::default(),
            execution_started: true,
            args_complete: true,
            is_partial: true,
            expanded: false,
            show_images: false,
            image_protocol: None,
            is_error: false,
            has_result: None,
            spinner_frame: None,
        };
        let mut slots = ToolRendererSession {
            renderers: std::sync::Arc::new(maho_ext_webfetch::webfetch::renderers::renderers()),
            context,
        }
        .into_slots();
        let mut result = AgentToolResult::text("");
        result.content.clear();
        result.details = details;
        let lines = slots
            .render_result(&result, &theme, 200)
            .expect("result renderer");
        maho_tui::utils::strip_terminal_sequences(&lines.join("\n"))
            .trim_end()
            .to_owned()
    };
    let progress = |phase: &str, extra: serde_json::Value| {
        let mut details = json!({
            "phase": phase,
            "url": "https://example.com/large-page",
            "format": "markdown",
            "timeoutSeconds": 30,
            "progress": {"activity": "fetching https://example.com/large-page", "startedAt": 0, "maxWaitMs": 30000},
        });
        if let (Some(target), Some(extra)) = (details.as_object_mut(), extra.as_object()) {
            target.extend(extra.clone());
        }
        details
    };
    assert_eq!(
        render(progress("fetching", json!({}))),
        "Fetching https://example.com/large-page as markdown (30s)"
    );
    assert_eq!(
        render(progress("downloading", json!({"bytesRead": 12595}))),
        "Downloading https://example.com/large-page: 12.3 KB"
    );
    assert_eq!(
        render(progress(
            "downloading",
            json!({"bytesRead": 12595, "totalBytes": 1258291})
        )),
        "Downloading https://example.com/large-page: 12.3 KB / 1.2 MB"
    );
    assert_eq!(
        render(progress("converting", json!({}))),
        "Converting https://example.com/large-page to markdown"
    );
}
