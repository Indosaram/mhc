//! End-to-end over the real pipe transport against a scripted stand-in browser
//! (`tests/fixtures/fake_browser.py`), so the CDP traffic each operation produces is checked
//! without a Chromium install.
#![cfg(unix)]

use maho_browser::browser::{Browser, LaunchOptions};
use maho_browser::extension::{BrowserExtension, Session, run_browser, run_computer};
use maho_browser::page::{SnapshotOptions, WaitUntil};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn fixture() -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_browser.py");
    let python = std::process::Command::new("python3").arg("--version").output().ok()?;
    if !python.status.success() {
        return None;
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).ok()?;
    Some(path)
}

fn requests(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn methods(log: &Path, method: &str) -> Vec<Value> {
    requests(log)
        .into_iter()
        .filter(|request| request["method"] == method)
        .map(|request| request["params"].clone())
        .collect()
}

/// Tests launch browsers and write logs; one at a time keeps the fixture traffic apart.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_fake<T>(run: impl FnOnce(&Path, PathBuf) -> T) -> Option<T> {
    let _guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let executable = fixture()?;
    let dir = tempfile::tempdir().ok()?;
    let log = dir.path().join("requests.jsonl");
    // The wrapper exports the log path for the fixture and then execs it, keeping fds 3 and 4.
    let wrapper = dir.path().join("browser.sh");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nFAKE_BROWSER_LOG='{}' exec '{}' \"$@\"\n",
            log.display(),
            executable.display()
        ),
    )
    .ok()?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).ok()?;
    Some(run(&log, wrapper))
}

#[test]
fn page_operations_send_the_expected_cdp_commands() {
    let ran = with_fake(|log, executable| {
        let mut browser = Browser::launch(&LaunchOptions {
            executable: Some(executable),
            viewport: (800, 600),
            ..LaunchOptions::default()
        })
        .unwrap_or_else(|error| panic!("launch failed: {error}"));
        assert_eq!(browser.version().ok().as_deref(), Some("FakeChrome/1.0"));
        let settled = browser.open("main", Some("https://fake.test/"), WaitUntil::Load);
        assert_eq!(settled, Ok(true));
        assert_eq!(methods(log, "Emulation.setDeviceMetricsOverride")[0]["width"], 800);
        assert_eq!(methods(log, "Page.navigate")[0]["url"], "https://fake.test/");
        assert_eq!(methods(log, "Target.attachToTarget")[0], json!({"targetId": "T1", "flatten": true}));

        let page = browser.tab("main").unwrap_or_else(|| panic!("tab missing"));
        let snapshot = page.snapshot(&SnapshotOptions::default()).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(snapshot.refs, 2);
        assert!(snapshot.tree.contains("button \"Go\" [ref=e1]"));
        assert_eq!(snapshot.title, "Fake Page");
        // The page world was injected exactly once.
        let installs = methods(log, "Runtime.evaluate")
            .into_iter()
            .filter(|params| params["expression"].as_str().is_some_and(|e| e.contains("globalThis.__mahoPage = {")))
            .count();
        assert_eq!(installs, 1);

        let element = page.click("e1", "left", 1).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!((element.x, element.y, element.tag.as_str()), (40.0, 30.0, "button"));
        let mouse: Vec<Value> = methods(log, "Input.dispatchMouseEvent");
        let kinds: Vec<&str> = mouse.iter().filter_map(|event| event["type"].as_str()).collect();
        assert_eq!(kinds, vec!["mouseMoved", "mousePressed", "mouseReleased"]);
        assert_eq!(mouse[1]["x"], 40.0);
        assert_eq!(mouse[1]["buttons"], 1);
        assert_eq!(mouse[2]["clickCount"], 1);

        page.fill("e2", "hello").unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(methods(log, "Input.insertText").last().map(|p| p["text"].clone()), Some(json!("hello")));

        page.press("Control+a").unwrap_or_else(|error| panic!("{error}"));
        let keys = methods(log, "Input.dispatchKeyEvent");
        let pressed: Vec<(String, String, u64)> = keys
            .iter()
            .map(|k| (k["type"].as_str().unwrap_or_default().to_owned(), k["key"].as_str().unwrap_or_default().to_owned(), k["modifiers"].as_u64().unwrap_or(99)))
            .collect();
        assert_eq!(
            pressed,
            vec![
                ("rawKeyDown".into(), "Control".into(), 0),
                ("rawKeyDown".into(), "a".into(), 2),
                ("keyUp".into(), "a".into(), 2),
                ("keyUp".into(), "Control".into(), 0),
            ]
        );
        page.type_text("A\n").unwrap_or_else(|error| panic!("{error}"));
        let keys = methods(log, "Input.dispatchKeyEvent");
        assert_eq!(keys[4]["text"], "A");
        assert_eq!(keys[4]["type"], "keyDown");
        assert_eq!(methods(log, "Input.insertText").last().map(|p| p["text"].clone()), Some(json!("\n")));

        page.scroll(None, 0.0, 300.0).unwrap_or_else(|error| panic!("{error}"));
        let wheel = methods(log, "Input.dispatchMouseEvent").into_iter().find(|e| e["type"] == "mouseWheel");
        assert_eq!(wheel.map(|e| (e["x"].clone(), e["y"].clone(), e["deltaY"].clone())), Some((json!(400.0), json!(300.0), json!(300.0))));

        let viewport_shot = page.screenshot(false, None).unwrap_or_default();
        assert_eq!(maho_browser::computer::png_dimensions(&viewport_shot), Ok((8, 6)));
        let full = page.screenshot(true, None).unwrap_or_default();
        assert_eq!(maho_browser::computer::png_dimensions(&full), Ok((8, 16)));

        assert_eq!(page.evaluate("1 + 1"), Ok(json!(2)));
        assert_eq!(page.evaluate("throw new Error('nope')"), Err("Error: nope".into()));
        let stale = page.click("e404", "left", 1).err().unwrap_or_default();
        assert!(stale.contains("stale"), "{stale}");
        let unreachable = page.goto("https://unreachable.test/", WaitUntil::Load, Duration::from_secs(2)).err().unwrap_or_default();
        assert!(unreachable.contains("ERR_NAME_NOT_RESOLVED"), "{unreachable}");

        let targets = browser.targets().unwrap_or_default();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].name.as_deref(), Some("main"));
        assert_eq!(browser.close_tab("main"), Ok(true));
        assert_eq!(methods(log, "Target.closeTarget")[0]["targetId"], "T1");
        browser.close();
        assert_eq!(methods(log, "Browser.close").len(), 1);
        // Startup noise on the browser's stdout did not corrupt the CDP stream, and is kept.
        assert!(browser.stdio_tail().contains("fake browser starting on stdout"), "{}", browser.stdio_tail());
    });
    if ran.is_none() {
        eprintln!("skipped: python3 unavailable");
    }
}

#[test]
fn tools_drive_the_browser_and_its_viewport() {
    let ran = with_fake(|log, executable| {
        let mut settings = BrowserExtension::from_env();
        settings.launch.executable = Some(executable);
        settings.cdp_url = None;
        let mut session = Session::new(settings, Path::new("/tmp"));
        let opened = run_browser(&mut session, &json!({"action": "open", "url": "https://fake.test/", "snapshot": true}))
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(opened.text.starts_with("Tab main open.\nPage: https://fake.test/ — Fake Page"), "{}", opened.text);
        assert!(opened.text.contains("[ref=e1]"), "{}", opened.text);
        let clicked = run_browser(&mut session, &json!({"action": "click", "target": "e1"})).unwrap_or_else(|error| panic!("{error}"));
        assert!(clicked.text.starts_with("Clicked e1 (<button>) at (40, 30)"), "{}", clicked.text);
        let shot = run_browser(&mut session, &json!({"action": "screenshot"})).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(shot.text, "Screenshot 8x6");
        assert!(shot.image_png.is_some());
        let value = run_browser(&mut session, &json!({"action": "evaluate", "expression": "1 + 1"})).map(|o| o.text);
        assert_eq!(value, Ok("2".into()));
        let tabs = run_browser(&mut session, &json!({"action": "tabs"})).map(|o| o.text).unwrap_or_default();
        assert!(tabs.starts_with("1 tab(s):\n- main: https://fake.test/"), "{tabs}");
        let missing = run_browser(&mut session, &json!({"action": "snapshot", "name": "other"})).err().unwrap_or_default();
        assert!(missing.contains("Open tabs: main"), "{missing}");

        // The computer tool defaults to the open tab's viewport.
        let screen = run_computer(&mut session, &json!({"action": "screenshot"})).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(screen.text, "Screenshot 8x6 (browser)");
        let before = methods(log, "Input.dispatchMouseEvent").len();
        let click = run_computer(&mut session, &json!({"action": "click", "x": 5, "y": 7, "modifiers": ["Shift"]}))
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(click.text, "left click x1 at (5, 7) on browser");
        let mouse = methods(log, "Input.dispatchMouseEvent");
        assert_eq!(mouse[before + 1]["modifiers"], 8, "Shift is held during the click");
        let drag = run_computer(&mut session, &json!({"action": "drag", "path": [{"x": 1, "y": 1}, {"x": 9, "y": 9}]}));
        assert_eq!(drag.map(|o| o.text), Ok("Dragged through 2 points on browser".into()));
        let keys = run_computer(&mut session, &json!({"action": "key", "keys": ["Enter", "Shift+Tab"]})).map(|o| o.text);
        assert_eq!(keys, Ok("Pressed Enter, Shift+Tab on browser".into()));

        let closed = run_browser(&mut session, &json!({"action": "close", "all": true})).map(|o| o.text);
        assert_eq!(closed, Ok("Browser closed.".into()));
        assert_eq!(methods(log, "Browser.close").len(), 1);
    });
    if ran.is_none() {
        eprintln!("skipped: python3 unavailable");
    }
}

#[test]
fn a_browser_that_never_answers_reports_its_stderr() {
    let Ok(dir) = tempfile::tempdir() else { return };
    let script = dir.path().join("broken.sh");
    if std::fs::write(&script, "#!/bin/sh\necho 'cannot open display' >&2\nexit 3\n").is_err() {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755));
    let error = Browser::launch(&LaunchOptions { executable: Some(script), ..LaunchOptions::default() })
        .err()
        .unwrap_or_default();
    assert!(error.contains("did not become ready"), "{error}");
    assert!(error.contains("cannot open display"), "{error}");
}
