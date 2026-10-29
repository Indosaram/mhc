//! Structural pin for the release service's deploy configuration.
//!
//! `wrangler.jsonc` is the deployed surface: bindings, routes, cron and observability.
//! Remote deployment is not authorized for this port, so the config is pinned by the
//! machine-consumed fields rather than exercised.

const WRANGLER: &str = include_str!("../../wrangler.jsonc");

fn value_after<'a>(text: &'a str, needle: &str) -> Option<&'a str> {
    let start = text.find(needle)? + needle.len();
    let rest = &text[start..];
    let end = rest.find('"').unwrap_or(rest.len());
    Some(&rest[..end])
}

#[test]
fn the_deploy_config_keeps_its_bindings_routes_and_cron() {
    assert_eq!(value_after(WRANGLER, "\"name\": \""), Some("omo-get"));
    assert_eq!(value_after(WRANGLER, "\"main\": \""), Some("src/index.ts"));
    assert_eq!(value_after(WRANGLER, "\"compatibility_date\": \""), Some("2026-09-20"));

    assert!(WRANGLER.contains("\"binding\": \"RELEASES\", \"bucket_name\": \"omo-releases\""));
    assert!(WRANGLER.contains("\"binding\": \"DB\""));
    assert!(WRANGLER.contains("\"database_name\": \"omo-get\""));
    assert!(WRANGLER.contains("\"migrations_dir\": \"migrations\""));
    assert!(WRANGLER.contains("\"binding\": \"DOWNLOADS\", \"dataset\": \"omo_downloads\""));

    assert!(WRANGLER.contains("\"pattern\": \"get.omo.dev\", \"custom_domain\": true"));
    assert!(WRANGLER.contains("\"crons\": [\"17 * * * *\"]"));
    assert!(WRANGLER.contains("\"head_sampling_rate\": 0.1"));
}

#[test]
fn the_text_loader_rules_keep_the_install_scripts_importable_as_strings() {
    assert!(WRANGLER.contains("\"type\": \"Text\""));
    assert!(WRANGLER.contains("\"globs\": [\"**/*.sh\", \"**/*.ps1\"]"));
    assert!(WRANGLER.contains("\"fallthrough\": false"));
}

#[test]
fn the_served_install_scripts_are_the_shipped_wrapper_and_bash_body() {
    assert!(get_worker::INSTALL_SH.starts_with("#!/bin/sh\n"));
    assert!(get_worker::INSTALL_SH.contains("<<'OMO_INSTALL_BASH'\n"));
    assert!(get_worker::INSTALL_SH.contains("if [[ \"${BASH_SOURCE[0]}\" == \"$0\" ]]; then main \"$@\"; fi\n"));
    assert!(get_worker::INSTALL_SH.contains("omo installer: bash is required"));
    assert!(get_worker::INSTALL_PS1.starts_with("# OmO native installer for Windows"));
    assert!(get_worker::INSTALL_PS1.contains("Install-Omo -Target $OmoTarget"));
}

#[test]
fn the_migration_declares_the_rollup_and_adjustment_tables() {
    let migration: &str = include_str!("../../migrations/0001_downloads.sql");
    assert!(migration.contains("CREATE TABLE IF NOT EXISTS downloads_daily"));
    assert!(migration.contains("PRIMARY KEY (day, kind, source, version, asset)"));
    assert!(migration.contains("CREATE TABLE IF NOT EXISTS download_adjustments"));
    assert!(migration.contains("delta INTEGER NOT NULL"));
    assert!(migration.contains("reason TEXT NOT NULL"));
}
