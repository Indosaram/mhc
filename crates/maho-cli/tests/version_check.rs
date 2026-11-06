use maho_cli::utils::version_check::*;
#[test]
fn calver_revisions_and_semver_prereleases_sort_correctly() {
    assert!(is_newer_package_version("2026.9.10-2", "2026.9.10"));
    assert!(is_newer_package_version("2026.10.1", "2026.9.30-2"));
    assert!(!is_newer_package_version("1.0.0-beta.2", "1.0.0"));
    assert_eq!(compare_package_versions("invalid", "1.0.0"), None);
}
#[test]
fn registry_document_selects_channel_or_version_and_rejects_empty_values() {
    let payload = serde_json::json!({"version":" 1.2.3 ", "beta":"1.2.4-beta.1", "empty":" "});
    assert_eq!(
        read_available_version(&payload, None).as_deref(),
        Some("1.2.3")
    );
    assert_eq!(
        read_available_version(&payload, Some("beta")).as_deref(),
        Some("1.2.4-beta.1")
    );
    assert_eq!(read_available_version(&payload, Some("empty")), None);
}
#[tokio::test]
async fn native_brand_without_registry_channel_does_not_advertise_engine_updates() {
    assert!(
        get_latest_release("1.2.3", Default::default())
            .await
            .unwrap()
            .is_none()
    );
    assert!(check_for_new_version("1.2.3").await.is_none());
}
#[test]
fn upstream_nested_network_errors_preserve_unique_errno_details() {
    let causes = [
        VersionCheckCause {
            code: Some("ETIMEDOUT"),
            message: Some("connect timeout"),
        },
        VersionCheckCause {
            code: Some("ENETUNREACH"),
            message: Some("network unreachable"),
        },
        VersionCheckCause {
            code: Some("ETIMEDOUT"),
            message: None,
        },
    ];
    assert_eq!(
        format_version_check_error("fetch failed", &causes),
        "fetch failed (ETIMEDOUT, ENETUNREACH)"
    );
    assert_eq!(
        format_version_check_error(
            "fetch failed",
            &[VersionCheckCause {
                code: None,
                message: Some("connect timeout")
            }]
        ),
        "fetch failed (cause: connect timeout)"
    );
    assert_eq!(format_version_check_error("root", &[]), "root");
}
// senpi version-check.test.ts and brand-update-channel.test.ts pure cases.
#[test]
fn upstream_compares_package_versions() {
    assert!(compare_package_versions("0.70.6", "0.70.5").unwrap() > 0);
    assert_eq!(compare_package_versions("0.70.5", "0.70.5"), Some(0));
    assert!(compare_package_versions("0.70.4", "0.70.5").unwrap() < 0);
    assert!(compare_package_versions("5.0.0-beta.20", "5.0.0-beta.9").unwrap() > 0);
    assert!(!is_newer_package_version("0.70.5", "0.70.5"));
    assert!(is_newer_package_version("0.70.6", "0.70.5"));
}
#[test]
fn upstream_orders_same_day_calver_revisions_after_the_bare_release() {
    assert!(compare_package_versions("2026.7.30-2", "2026.7.30").unwrap() > 0);
    assert!(compare_package_versions("2026.7.30", "2026.7.30-2").unwrap() < 0);
    assert!(compare_package_versions("2026.7.31", "2026.7.30-99").unwrap() > 0);
    assert!(!is_newer_package_version("2026.7.30", "2026.7.30-2"));
    assert!(is_newer_package_version("2026.7.30-2", "2026.7.30"));
}
#[test]
fn upstream_never_advertises_an_update_for_branded_build_labels() {
    let label = "omo@c6e7dd7 2026-09-04 10:17 +09:00";
    assert!(!is_newer_package_version("2026.9.3-3", label));
    assert!(!is_newer_package_version(label, "2026.9.3-3"));
}
#[test]
fn upstream_release_changelog_urls_follow_the_channel_or_the_engine() {
    assert_eq!(
        get_release_changelog_url("2026.6.28-4", None),
        "https://github.com/code-yeongyu/senpi/blob/v2026.6.28-4/packages/coding-agent/CHANGELOG.md"
    );
    assert_eq!(
        get_release_changelog_url("2026.8.10", None),
        "https://github.com/code-yeongyu/senpi/blob/v2026.8.10/packages/coding-agent/CHANGELOG.md"
    );
    assert_eq!(
        get_release_changelog_url(
            "5.0.0-0.beta.4",
            Some("https://github.com/code-yeongyu/oh-my-openagent/releases/tag/v{version}")
        ),
        "https://github.com/code-yeongyu/oh-my-openagent/releases/tag/v5.0.0-0.beta.4"
    );
}
#[test]
fn upstream_reads_the_branded_dist_tag_or_the_engine_version() {
    let dist_tags = serde_json::json!({"latest": "0.0.0-beta.0", "beta": "5.0.0-0.beta.4"});
    assert_eq!(
        read_available_version(&dist_tags, Some("beta")).as_deref(),
        Some("5.0.0-0.beta.4")
    );
    assert_eq!(read_available_version(&dist_tags, Some("missing")), None);
    assert_eq!(
        read_available_version(&serde_json::json!({"version": "2026.8.10"}), None).as_deref(),
        Some("2026.8.10")
    );
    assert_eq!(read_available_version(&serde_json::json!({}), None), None);
}

// senpi version-check.test.ts live cases: the global `fetch` stub is the `VersionFetch` seam.
mod upstream_fetch {
    use maho_cli::utils::version_check::*;
    use std::{
        collections::{HashMap, VecDeque},
        sync::Mutex,
    };

    struct FakeFetch {
        answers: Mutex<VecDeque<Result<String, String>>>,
        fallback: Result<String, String>,
        calls: Mutex<Vec<VersionFetchRequest>>,
    }
    impl FakeFetch {
        fn always(answer: Result<&str, &str>) -> Self {
            Self::sequence(&[], answer)
        }
        fn sequence(first: &[Result<&str, &str>], then: Result<&str, &str>) -> Self {
            fn own(answer: &Result<&str, &str>) -> Result<String, String> {
                (*answer).map(str::to_owned).map_err(str::to_owned)
            }
            Self {
                answers: Mutex::new(first.iter().map(own).collect()),
                fallback: own(&then),
                calls: Mutex::new(Vec::new()),
            }
        }
        fn calls(&self) -> Vec<VersionFetchRequest> {
            self.calls.lock().expect("calls").clone()
        }
    }
    impl VersionFetch for FakeFetch {
        fn fetch(
            &self,
            request: VersionFetchRequest,
        ) -> futures::future::BoxFuture<'_, Result<VersionFetchResponse, String>> {
            self.calls.lock().expect("calls").push(request);
            let answer = self
                .answers
                .lock()
                .expect("answers")
                .pop_front()
                .unwrap_or_else(|| self.fallback.clone());
            Box::pin(async move {
                answer.map(|body| VersionFetchResponse {
                    status: 200,
                    body: body.into_bytes(),
                })
            })
        }
    }
    fn io<'a>(fetch: &'a FakeFetch, env: &[(&str, &str)]) -> VersionCheckIo<'a> {
        VersionCheckIo {
            fetch,
            channel: Some(maho_core::brand::BrandUpdateChannel {
                package_name: "@code-yeongyu/senpi".to_owned(),
                dist_tag: "latest".to_owned(),
                command: "npm i -g @code-yeongyu/senpi".to_owned(),
                changelog_url: None,
            }),
            env: env
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect::<HashMap<_, _>>(),
        }
    }
    const LATEST: &str = r#"{"latest":"1.2.4"}"#;

    #[tokio::test]
    async fn returns_only_newer_versions() {
        let fetch = FakeFetch::always(Ok(r#"{"latest":"1.2.3"}"#));
        assert!(
            check_for_new_version_with(&io(&fetch, &[]), "1.2.3")
                .await
                .is_none()
        );
        assert_eq!(
            check_for_new_version_with(&io(&fetch, &[]), "1.2.2")
                .await
                .map(|release| release.version)
                .as_deref(),
            Some("1.2.3")
        );
    }
    #[tokio::test]
    async fn uses_the_package_metadata_url_with_the_product_user_agent() {
        let fetch = FakeFetch::always(Ok(LATEST));
        assert_eq!(
            get_latest_version_with(&io(&fetch, &[]), "1.2.3", Default::default())
                .await
                .unwrap()
                .as_deref(),
            Some("1.2.4")
        );
        let calls = fetch.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].url,
            "https://registry.npmjs.org/-/package/%40code-yeongyu%2Fsenpi/dist-tags"
        );
        assert!(
            calls[0].user_agent.starts_with("mhc/1.2.3 "),
            "{}",
            calls[0].user_agent
        );
        assert_eq!(calls[0].accept, "application/json");
    }
    #[tokio::test]
    async fn retries_a_transient_version_request_when_explicitly_requested() {
        let fetch = FakeFetch::sequence(&[Err("fetch failed"), Err("fetch failed")], Ok(LATEST));
        let release = get_latest_release_with(
            &io(&fetch, &[]),
            "1.2.3",
            VersionCheckOptions {
                retry: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            release.map(|release| release.version).as_deref(),
            Some("1.2.4")
        );
        assert_eq!(fetch.calls().len(), 3);
    }
    #[tokio::test]
    async fn keeps_automatic_version_checks_to_one_request() {
        let fetch = FakeFetch::always(Err("fetch failed"));
        assert!(
            check_for_new_version_with(&io(&fetch, &[]), "1.2.3")
                .await
                .is_none()
        );
        assert_eq!(fetch.calls().len(), 1);
    }
    #[tokio::test]
    async fn skips_automatic_api_calls_when_version_checks_are_disabled() {
        let fetch = FakeFetch::always(Ok(LATEST));
        assert!(
            check_for_new_version_with(&io(&fetch, &[("PI_SKIP_VERSION_CHECK", "1")]), "1.2.3")
                .await
                .is_none()
        );
        assert!(fetch.calls().is_empty());
    }
    #[tokio::test]
    async fn allows_direct_api_calls_when_automatic_version_checks_are_disabled() {
        let fetch = FakeFetch::always(Ok(LATEST));
        assert_eq!(
            get_latest_version_with(
                &io(&fetch, &[("PI_SKIP_VERSION_CHECK", "1")]),
                "1.2.3",
                Default::default()
            )
            .await
            .unwrap()
            .as_deref(),
            Some("1.2.4")
        );
        assert_eq!(fetch.calls().len(), 1);
    }
}
