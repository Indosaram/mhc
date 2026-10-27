//! Against a real Chromium, when `$MAHO_BROWSER_TEST_PATH` names one (skipped otherwise): both
//! launch transports, snapshots with refs, clicks, fills, keys, selects, history, screenshots, and
//! attaching to the browser's DevTools endpoint.
use maho_browser::browser::{Browser, LaunchOptions, TransportKind};
use maho_browser::extension::{BrowserExtension, Session, run_browser, run_computer};
use maho_browser::page::{SnapshotOptions, WaitUntil};
use serde_json::json;
use std::path::{Path, PathBuf};

const PAGE: &str = r#"<!doctype html><title>Form</title>
<h1>Order</h1>
<label>Name <input id="name"></label>
<select id="size"><option value="s">Small</option><option value="l">Large</option></select>
<button id="go" onclick="document.title = 'clicked:' + document.getElementById('name').value + ':' + document.getElementById('size').value">Submit</button>
<input id="keys" onkeydown="if (event.key === 'Enter') document.title = 'enter:' + this.value">
<div style="height:3000px"></div>"#;

fn browser_path() -> Option<PathBuf> {
    std::env::var_os("MAHO_BROWSER_TEST_PATH").map(PathBuf::from)
}

fn page_url(dir: &Path) -> String {
    let file = dir.join("form.html");
    std::fs::write(&file, PAGE).unwrap_or_else(|error| panic!("{error}"));
    format!("file://{}", file.display())
}

fn exercise(transport: TransportKind) {
    let Some(executable) = browser_path() else {
        eprintln!("skipped: MAHO_BROWSER_TEST_PATH is not set");
        return;
    };
    let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let url = page_url(dir.path());
    let mut browser = Browser::launch(&LaunchOptions {
        executable: Some(executable),
        transport,
        viewport: (900, 700),
        ..LaunchOptions::default()
    })
    .unwrap_or_else(|error| panic!("launch failed: {error}"));
    assert_eq!(browser.open("main", Some(&url), WaitUntil::Load), Ok(true));
    let page = browser.tab("main").unwrap_or_else(|| panic!("no tab"));
    let snapshot = page
        .snapshot(&SnapshotOptions::default())
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(snapshot.title, "Form");
    assert!(
        snapshot.tree.contains("heading \"Order\""),
        "{}",
        snapshot.tree
    );
    let button_ref = snapshot
        .tree
        .lines()
        .find(|line| line.contains("button \"Submit\""))
        .and_then(|line| line.split("[ref=").nth(1))
        .and_then(|rest| rest.split(']').next())
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("no button ref in\n{}", snapshot.tree));
    page.fill("#name", "Ada")
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        page.select_option("#size", &["Large".into()]),
        Ok(vec!["l".to_owned()])
    );
    page.click(&button_ref, "left", 1)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(page.evaluate("document.title"), Ok(json!("clicked:Ada:l")));
    page.click("#keys", "left", 1)
        .unwrap_or_else(|error| panic!("{error}"));
    page.type_text("Hi!")
        .unwrap_or_else(|error| panic!("{error}"));
    page.press("Enter")
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(page.evaluate("document.title"), Ok(json!("enter:Hi!")));
    page.press("Control+a")
        .unwrap_or_else(|error| panic!("{error}"));
    page.press("Backspace")
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        page.evaluate("document.getElementById('keys').value"),
        Ok(json!(""))
    );
    page.scroll(None, 0.0, 500.0)
        .unwrap_or_else(|error| panic!("{error}"));
    let scrolled = page
        .evaluate("scrollY")
        .ok()
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    assert!(scrolled > 0.0, "wheel did not scroll");
    let shot = page.screenshot(false, None).unwrap_or_default();
    assert_eq!(
        maho_browser::computer::png_dimensions(&shot),
        Ok((900, 700))
    );
    let element = page.screenshot(false, Some("#go")).unwrap_or_default();
    assert!(
        maho_browser::computer::png_dimensions(&element).is_ok_and(|(w, h)| w < 300 && h < 100)
    );
    let stale = page.click("e999", "left", 1).err().unwrap_or_default();
    assert!(stale.contains("stale"), "{stale}");
    assert_eq!(
        page.goto(
            "about:blank",
            WaitUntil::Load,
            std::time::Duration::from_secs(10)
        ),
        Ok(true)
    );
    assert_eq!(
        page.history(-1, WaitUntil::Load, std::time::Duration::from_secs(10)),
        Ok(true)
    );
    assert_eq!(page.evaluate("document.title"), Ok(json!("Form")));
    browser.close();
}

#[test]
fn pipe_transport_drives_a_real_page() {
    exercise(TransportKind::Pipe);
}

#[test]
fn port_transport_drives_a_real_page() {
    exercise(TransportKind::Port);
}

#[test]
fn tools_against_a_real_browser_and_cdp_attach() {
    let Some(executable) = browser_path() else {
        eprintln!("skipped: MAHO_BROWSER_TEST_PATH is not set");
        return;
    };
    let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let url = page_url(dir.path());
    let mut settings = BrowserExtension::from_env();
    settings.launch.executable = Some(executable.clone());
    settings.cdp_url = None;
    let mut session = Session::new(settings, dir.path());
    let opened = run_browser(
        &mut session,
        &json!({"action": "open", "url": url, "snapshot": true, "interactive": true}),
    )
    .unwrap_or_else(|error| panic!("{error}"));
    assert!(opened.text.contains("button \"Submit\""), "{}", opened.text);
    let filled = run_browser(
        &mut session,
        &json!({"action": "fill", "target": "#name", "text": "Bo"}),
    );
    assert!(filled.is_ok(), "{filled:?}");
    let clicked = run_browser(&mut session, &json!({"action": "click", "target": "#go"}))
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(clicked.text.contains("— clicked:Bo:s"), "{}", clicked.text);
    let saved = run_browser(
        &mut session,
        &json!({"action": "screenshot", "path": "shots/page.png"}),
    )
    .unwrap_or_else(|error| panic!("{error}"));
    assert!(
        saved.text.starts_with("Screenshot 1365x768, saved to"),
        "{}",
        saved.text
    );
    assert!(dir.path().join("shots/page.png").is_file());
    let screen = run_computer(&mut session, &json!({"action": "screenshot"})).map(|o| o.text);
    assert_eq!(screen, Ok("Screenshot 1365x768 (browser)".into()));
    let waited = run_browser(
        &mut session,
        &json!({"action": "wait", "text": "Order", "timeout": 5}),
    );
    assert_eq!(
        waited.map(|o| o.text.starts_with("Condition met")),
        Ok(true)
    );
    run_browser(&mut session, &json!({"action": "close", "all": true}))
        .unwrap_or_else(|error| panic!("{error}"));

    // Attach to a browser someone else started, over its DevTools WebSocket.
    let mut owner = Browser::launch(&LaunchOptions {
        executable: Some(executable),
        transport: TransportKind::Port,
        ..LaunchOptions::default()
    })
    .unwrap_or_else(|error| panic!("{error}"));
    owner
        .open("seed", Some(&url), WaitUntil::Load)
        .unwrap_or_else(|error| panic!("{error}"));
    let endpoint = owner
        .stdio_tail()
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("DevTools listening on ")
                .map(str::to_owned)
        })
        .unwrap_or_else(|| panic!("no endpoint in {}", owner.stdio_tail()));
    let port = endpoint
        .split(':')
        .nth(2)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or_default()
        .to_owned();
    let mut guest = Browser::connect(&format!("http://127.0.0.1:{port}"))
        .unwrap_or_else(|error| panic!("{error}"));
    let seed = guest
        .targets()
        .unwrap_or_default()
        .into_iter()
        .find(|target| target.title == "Form")
        .unwrap_or_else(|| panic!("seed tab not listed"));
    guest
        .adopt("seed", &seed.id)
        .unwrap_or_else(|error| panic!("{error}"));
    let title = guest
        .tab("seed")
        .map(|page| page.evaluate("document.title"));
    assert_eq!(title, Some(Ok(json!("Form"))));
    guest.close();
    // Closing an attached browser leaves it running.
    assert!(owner.is_connected());
    owner.close();
}
