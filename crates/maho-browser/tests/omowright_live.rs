//! omowright frames, network snoop, trace and stealth against a real Chromium. The tests are
//! `#[ignore]`d: run them with `$MAHO_BROWSER_TEST_PATH` naming a browser and
//! `cargo nextest run --run-ignored ignored-only` (or `cargo test -- --ignored`); without the
//! variable an ignored run fails instead of passing silently.
use maho_browser::browser::{Browser, LaunchOptions};
use maho_browser::page::network_snoop::{EntryMatch, SnoopOptions};
use maho_browser::page::trace::TraceOptions;
use maho_browser::page::{NAVIGATION_TIMEOUT, SnapshotOptions, WaitUntil};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn browser_path() -> Option<PathBuf> {
    std::env::var_os("MAHO_BROWSER_TEST_PATH").map(PathBuf::from)
}

fn launch() -> Browser {
    let Some(executable) = browser_path() else {
        panic!("MAHO_BROWSER_TEST_PATH is not set; it must name a Chromium for these tests");
    };
    Browser::launch(&LaunchOptions {
        executable: Some(executable),
        features: maho_browser::launch::LaunchFeatures {
            allow_file_access: true,
            ..Default::default()
        },
        ..LaunchOptions::default()
    })
    .unwrap_or_else(|error| panic!("launch failed: {error}"))
}

fn write(dir: &Path, name: &str, html: &str) -> String {
    let file = dir.join(name);
    std::fs::write(&file, html).unwrap_or_else(|error| panic!("{error}"));
    format!("file://{}", file.display())
}

#[test]
#[ignore = "needs MAHO_BROWSER_TEST_PATH"]
fn frames_are_stitched_into_one_snapshot() {
    let mut browser = launch();
    let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    write(dir.path(), "child.html", "<button>Inner</button>");
    let url = write(
        dir.path(),
        "parent.html",
        "<h1>Outer</h1><iframe src=\"child.html\"></iframe>",
    );
    assert_eq!(browser.open("main", Some(&url), WaitUntil::Load), Ok(true));
    let page = browser.tab("main").unwrap_or_else(|| panic!("no tab"));
    let mut frames = page
        .frame_manager()
        .unwrap_or_else(|error| panic!("{error}"));
    let snapshot = page
        .snapshot_with_frames(&mut frames, &SnapshotOptions::default())
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(
        snapshot.tree.contains("heading \"Outer\""),
        "{}",
        snapshot.tree
    );
    assert!(
        snapshot.tree.contains("button \"Inner\""),
        "{}",
        snapshot.tree
    );
    assert!(
        snapshot
            .refs
            .keys()
            .any(|reference| reference.starts_with("f1e")),
        "{:?}",
        snapshot.refs.keys().collect::<Vec<_>>()
    );
}

#[test]
#[ignore = "needs MAHO_BROWSER_TEST_PATH"]
fn snoop_and_trace_record_a_navigation() {
    let mut browser = launch();
    let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let url = write(
        dir.path(),
        "page.html",
        "<title>T</title><script>console.log('traced')</script>",
    );
    assert_eq!(browser.open("main", None, WaitUntil::Load), Ok(true));
    let page = browser.tab("main").unwrap_or_else(|| panic!("no tab"));
    page.capture_console()
        .unwrap_or_else(|error| panic!("{error}"));
    let mut snoop = page.network_snoop(SnoopOptions::default());
    let mark = snoop.mark();
    let mut trace = page
        .trace(TraceOptions::new(dir.path().join("trace")))
        .unwrap_or_else(|error| panic!("{error}"));
    let navigated = trace.step(page, "open", |page: &mut maho_browser::Page| {
        page.goto(&url, WaitUntil::Load, NAVIGATION_TIMEOUT)
    });
    assert_eq!(navigated, Ok(true));
    let entry = snoop
        .wait_from(mark, &EntryMatch::url("page.html"), NAVIGATION_TIMEOUT)
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(entry.url.ends_with("page.html"));
    let summary = trace.stop(page).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(summary["steps"], 1);
    let lines = std::fs::read_to_string(dir.path().join("trace/trace.jsonl")).unwrap_or_default();
    let kinds: Vec<Value> = lines
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .map(|line| line["kind"].clone())
        .collect();
    assert!(kinds.contains(&json!("console")), "{kinds:?}");
    assert!(kinds.contains(&json!("navigation")), "{kinds:?}");
}

#[test]
#[ignore = "needs MAHO_BROWSER_TEST_PATH"]
fn stealth_hides_webdriver_and_headless_markers() {
    let mut browser = launch();
    assert_eq!(browser.open("main", None, WaitUntil::Load), Ok(true));
    // A launched headless browser patches every tab it opens.
    let page = browser.tab("main").unwrap_or_else(|| panic!("no tab"));
    let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
    let url = write(dir.path(), "s.html", "<title>S</title>");
    assert_eq!(
        page.goto(&url, WaitUntil::Load, NAVIGATION_TIMEOUT),
        Ok(true)
    );
    let probe = page
        .evaluate("[navigator.webdriver === true, navigator.userAgent]")
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(probe[0], false);
    assert!(
        probe[1]
            .as_str()
            .is_some_and(|ua| !ua.contains("HeadlessChrome")),
        "{probe}"
    );
}
