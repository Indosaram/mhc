//! Port of `test/routes.test.ts`.

use get_worker::{route, Method, Request, INSTALL_PS1, INSTALL_SH, NPM_DIST_TAGS};

use crate::fakes::{Harness, StaticFetcher};

fn get(path: &str) -> Request {
    Request::new(Method::Get, format!("https://get.omo.dev{path}"))
}

fn get_with(path: &str, headers: &[(&str, &str)]) -> Request {
    let mut request = get(path);
    for (name, value) in headers {
        request.headers.set(name, value);
    }
    request
}

fn mirror(h: &Harness, version: &str, assets: &[(&str, &str)]) {
    for (name, body) in assets {
        h.bucket.put(&format!("releases/v{version}/{name}"), body);
    }
    h.bucket.put(&format!("releases/v{version}/.complete"), "{}");
}

#[test]
fn serves_install_scripts_as_text_with_a_short_cache_and_counts_them() {
    let h = Harness::new("", StaticFetcher::new());
    let ctx = h.context();

    let sh = route(&get("/install.sh"), &ctx).expect("route");
    assert_eq!(sh.status, 200);
    assert_eq!(sh.body_text(), INSTALL_SH);
    assert_eq!(sh.headers.get("Cache-Control"), Some("public, max-age=300"));

    let ps1 = route(&get("/install.ps1"), &ctx).expect("route");
    assert_eq!(ps1.body_text(), INSTALL_PS1);

    let assets: Vec<String> = h.sink.points().iter().map(|p| p.blobs[3].clone()).collect();
    assert_eq!(assets, vec!["install.sh".to_string(), "install.ps1".to_string()]);
}

#[test]
fn root_serves_the_script_to_curl_and_powershell_and_sends_browsers_to_the_docs() {
    let h = Harness::new("", StaticFetcher::new());
    let ctx = h.context();

    let curl = route(&get_with("/", &[("User-Agent", "curl/8.7.1")]), &ctx).expect("route");
    assert_eq!(curl.body_text(), INSTALL_SH);

    let powershell = route(
        &get_with("/", &[("User-Agent", "Mozilla/5.0 WindowsPowerShell/5.1")]),
        &ctx,
    )
    .expect("route");
    assert_eq!(powershell.body_text(), INSTALL_PS1);

    let browser = route(
        &get_with("/", &[("User-Agent", "Mozilla/5.0 Safari/605")]),
        &ctx,
    )
    .expect("route");
    assert_eq!(browser.status, 302);
    assert_eq!(browser.headers.get("Location"), Some("https://omo.dev/docs/install"));
}

#[test]
fn reads_the_mirrored_channel_pointer_and_caches_it_for_a_minute() {
    let h = Harness::new("", StaticFetcher::new());
    h.bucket.put("channels/latest", "5.1.1\n");
    let ctx = h.context();

    let response = route(&get("/channels/latest"), &ctx).expect("route");
    assert_eq!(response.body_text(), "5.1.1\n");
    assert_eq!(response.headers.get("X-Omo-Source"), Some("r2"));
    assert_eq!(response.headers.get("Cache-Control"), Some("public, max-age=60"));

    h.settle();
    h.bucket.clear();
    let second = route(&get("/channels/latest"), &ctx).expect("route");
    assert_eq!(second.body_text(), "5.1.1\n");
}

#[test]
fn falls_back_to_the_npm_dist_tag_and_maps_the_npm_pre_release_form() {
    let fetcher = StaticFetcher::new().with(
        NPM_DIST_TAGS,
        get_worker::FetchResponse::new(200, r#"{"latest":"5.1.1","beta":"5.2.0-0.beta.3"}"#),
    );
    let h = Harness::new("", fetcher);
    let ctx = h.context();

    let response = route(&get("/channels/beta"), &ctx).expect("route");
    assert_eq!(response.body_text(), "5.2.0-beta.3\n");
    assert_eq!(response.headers.get("X-Omo-Source"), Some("npm"));
}

#[test]
fn rejects_an_unknown_channel() {
    let h = Harness::new("", StaticFetcher::new());
    let response = route(&get("/channels/nightly"), &h.context()).expect("route");
    assert_eq!(response.status, 404);
}

#[test]
fn serves_a_mirrored_binary_from_storage_with_an_immutable_cache_and_counts_it() {
    let h = Harness::new("", StaticFetcher::new());
    mirror(&h, "5.1.1", &[("omo-linux-x64", "binary-bytes"), ("SHA256SUMS", "sums")]);
    let ctx = h.context();

    let response = route(&get("/v/5.1.1/omo-linux-x64"), &ctx).expect("route");
    assert_eq!(response.status, 200);
    assert_eq!(response.body_text(), "binary-bytes");
    assert_eq!(
        response.headers.get("Cache-Control"),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(response.headers.get("X-Omo-Source"), Some("r2"));

    let points = h.sink.points();
    let last = points.last().expect("a recorded point");
    let head: Vec<&str> = last.blobs[0..4].iter().map(String::as_str).collect();
    assert_eq!(head, vec!["binary", "r2", "5.1.1", "omo-linux-x64"]);
}

#[test]
fn repeat_downloads_are_cache_hits_that_never_read_storage() {
    let h = Harness::new("", StaticFetcher::new());
    mirror(&h, "5.1.1", &[("omo-darwin-arm64", "arm-bytes")]);
    let ctx = h.context();

    let _ = route(&get("/v/5.1.1/omo-darwin-arm64"), &ctx).expect("route");
    h.settle();
    let reads = h.bucket.reads().len();

    let again = route(&get("/v/5.1.1/omo-darwin-arm64"), &ctx).expect("route");
    assert_eq!(again.body_text(), "arm-bytes");
    assert_eq!(again.headers.get("X-Omo-Source"), Some("cache"));
    assert_eq!(h.bucket.reads().len(), reads);

    let points = h.sink.points();
    assert_eq!(points.last().expect("a recorded point").blobs[1], "cache");
}

#[test]
fn redirects_to_the_github_asset_when_the_version_was_never_marked_complete() {
    let h = Harness::new("", StaticFetcher::new());
    h.bucket.put("releases/v5.1.0/omo-linux-x64", "half-mirrored");
    let ctx = h.context();

    let response = route(&get("/v/5.1.0/omo-linux-x64"), &ctx).expect("route");
    assert_eq!(response.status, 302);
    assert_eq!(
        response.headers.get("Location"),
        Some("https://github.com/code-yeongyu/oh-my-openagent/releases/download/v5.1.0/omo-linux-x64")
    );
    assert_eq!(response.headers.get("X-Omo-Fallback-Reason"), Some("version-not-mirrored"));

    let points = h.sink.points();
    assert_eq!(points.last().expect("a recorded point").blobs[1], "github");
}

#[test]
fn redirects_to_github_when_storage_errors() {
    let h = Harness::new("", StaticFetcher::new());
    mirror(&h, "5.1.1", &[("omo-linux-x64", "bytes")]);
    h.bucket.fail_with("R2 unavailable");
    let ctx = h.context();

    let response = route(&get("/v/5.1.1/omo-linux-x64"), &ctx).expect("route");
    assert_eq!(response.status, 302);
    assert_eq!(response.headers.get("X-Omo-Fallback-Reason"), Some("r2-error"));
}

#[test]
fn refuses_names_outside_the_release_allowlist_so_the_redirect_cannot_be_steered() {
    let h = Harness::new("", StaticFetcher::new());
    let ctx = h.context();

    for path in [
        "/v/5.1.1/..%2F..%2Fevil",
        "/v/5.1.1/install.sh",
        "/v/latest/omo-linux-x64",
        "/v/5.1.1/omo-plan9-x64",
    ] {
        assert_eq!(route(&get(path), &ctx).expect("route").status, 404, "{path}");
    }
    assert!(h.sink.points().is_empty());
}

#[test]
fn tags_installer_qa_downloads_so_the_rollup_can_leave_them_out() {
    let h = Harness::new("", StaticFetcher::new());
    mirror(&h, "5.1.1", &[("omo-linux-x64", "bytes")]);
    let ctx = h.context();

    let _ = route(
        &get_with(
            "/v/5.1.1/omo-linux-x64",
            &[("User-Agent", "omo-install.sh/1 omo-install-qa")],
        ),
        &ctx,
    )
    .expect("route");
    let _ = route(
        &get_with("/v/5.1.1/omo-linux-x64", &[("User-Agent", "omo-install.sh/1")]),
        &ctx,
    )
    .expect("route");

    let tags: Vec<String> = h.sink.points().iter().map(|p| p.blobs[5].clone()).collect();
    assert_eq!(tags, vec!["qa".to_string(), String::new()]);
}

#[test]
fn counts_checksums_and_desktop_engines_under_their_own_kinds() {
    let h = Harness::new("", StaticFetcher::new());
    mirror(
        &h,
        "5.1.1",
        &[("SHA256SUMS", "s"), ("senpi-desktop-engine-darwin-arm64", "e")],
    );
    let ctx = h.context();

    let _ = route(&get("/v/5.1.1/SHA256SUMS"), &ctx).expect("route");
    let _ = route(&get("/v/5.1.1/senpi-desktop-engine-darwin-arm64"), &ctx).expect("route");

    let kinds: Vec<String> = h.sink.points().iter().map(|p| p.blobs[0].clone()).collect();
    assert_eq!(kinds, vec!["checksums".to_string(), "engine".to_string()]);
}
