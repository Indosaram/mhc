use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use std::sync::LazyLock;

/// The user's opt-in for reading listed hosts through their signed-in browser (senpi#5940). Read only
/// from the GLOBAL settings file, parse-only (no migration, no write), so a repository's
/// `.senpi/settings.json` can never turn it on and a tool call never rewrites the user's settings. The
/// model gets no parameter for it.
///
/// `{ "webfetch": { "signedInBrowser": { "hosts": ["wiki.example.com", "*.corp.example"], "browser": "Work Chrome" } } }`
pub const SIGNED_IN_HOSTS_SETTING: &str = "webfetch.signedInBrowser.hosts";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignedInBrowserSettings {
    pub hosts: Vec<String>,
    pub browser: Option<String>,
}

// ASCII host labels only. A wildcard is refused on a single label ("*.com"), on a country second-level
// domain ("*.co.uk", "*.com.tw": a generic second label under a two-letter country code), and on the shared
// hosting suffixes listed below ("*.github.io"). This is not a full public suffix list: a suffix outside
// these rules is accepted, and only the user can add a host.
static HOST_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A(?:\*\.)?[a-z0-9-]+(?:\.[a-z0-9-]+)*\z").expect("literal pattern")
});

static IPV4: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A(?:\d+(?:\.\d+){0,3}|0x[0-9a-f]+)\z").expect("literal pattern")
});

// Shared hosting suffixes a wildcard must sit below, not on.
static PUBLIC_SUFFIXES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "co.uk",
        "org.uk",
        "ac.uk",
        "gov.uk",
        "com.au",
        "net.au",
        "org.au",
        "co.jp",
        "or.jp",
        "ne.jp",
        "co.kr",
        "or.kr",
        "com.br",
        "com.cn",
        "com.mx",
        "co.in",
        "co.nz",
        "co.za",
        "github.io",
        "gitlab.io",
        "pages.dev",
        "workers.dev",
        "vercel.app",
        "netlify.app",
        "herokuapp.com",
        "azurewebsites.net",
        "cloudfront.net",
        "s3.amazonaws.com",
        "appspot.com",
        "web.app",
        "firebaseapp.com",
        "amazonaws.com",
        "blogspot.com",
        "ngrok.io",
        "ngrok-free.app",
    ])
});

// Generic second labels that, under a two-letter country code, form a country's public second level.
static COUNTRY_SECOND_LEVELS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "com", "co", "org", "net", "ac", "or", "ne", "go", "gov", "edu", "mil", "gob",
    ])
});

fn is_wildcard_too_broad(pattern: &str) -> bool {
    let Some(base) = pattern.strip_prefix("*.") else {
        return false;
    };
    let labels: Vec<&str> = base.split('.').collect();
    if labels.len() < 2 || PUBLIC_SUFFIXES.contains(base) {
        return true;
    }
    if labels.len() != 2 {
        return false;
    }
    let second = labels[0];
    let top = labels[1];
    top.len() == 2 && COUNTRY_SECOND_LEVELS.contains(second)
}

pub fn parse_signed_in_browser_settings(global_settings: &Value) -> SignedInBrowserSettings {
    let signed_in = global_settings
        .as_object()
        .and_then(|root| root.get("webfetch"))
        .and_then(Value::as_object)
        .and_then(|webfetch| webfetch.get("signedInBrowser"))
        .and_then(Value::as_object);

    let hosts = signed_in
        .and_then(|obj| obj.get("hosts"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|host| host.trim().to_lowercase())
                .filter(|host| {
                    HOST_PATTERN.is_match(host)
                        && !is_wildcard_too_broad(host)
                        && !IPV4.is_match(host)
                })
                .collect()
        })
        .unwrap_or_default();

    let browser = signed_in
        .and_then(|obj| obj.get("browser"))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());

    SignedInBrowserSettings { hosts, browser }
}

/// Reads the global settings file without loading a SettingsManager (whose load can migrate and write it).
pub fn read_signed_in_browser_settings(agent_dir: Option<&Path>) -> SignedInBrowserSettings {
    let owned_dir;
    let agent_dir = match agent_dir {
        Some(dir) => dir,
        None => {
            owned_dir = std::path::PathBuf::from(maho_core::config::get_agent_dir());
            owned_dir.as_path()
        }
    };
    let dir_str = agent_dir.to_string_lossy();
    let Some(source) = maho_core::settings_manager::resolve_settings_source(
        &dir_str,
        &dir_str,
        maho_core::settings_manager::SettingsScope::Global,
        &dir_str,
    ) else {
        return SignedInBrowserSettings::default();
    };
    let Ok(content) = std::fs::read_to_string(&source.path) else {
        return SignedInBrowserSettings::default();
    };
    let Ok(settings) = maho_core::settings_manager::parse_settings_json(&content) else {
        return SignedInBrowserSettings::default();
    };
    parse_signed_in_browser_settings(&Value::Object(settings))
}

/// `example.com` matches that host only; `*.example.com` matches its subdomains. Only http(s) URLs match.
pub fn is_signed_in_host(raw_url: &str, hosts: &[String]) -> bool {
    let Ok(parsed) = url::Url::parse(raw_url) else {
        return false;
    };
    if parsed.scheme() != "https" && parsed.scheme() != "http" {
        return false;
    }
    let Some(raw_host) = parsed.host_str() else {
        return false;
    };
    let host = raw_host.to_lowercase();
    // An IP literal is never a listed host: 127.0.0.1, and the forms URL normalizes to it (2130706433, 0x7f.0.0.1).
    if host.ends_with('.') || host.starts_with('[') || IPV4.is_match(&host) {
        return false;
    }
    hosts.iter().any(|pattern| {
        if pattern.starts_with("*.") {
            host.ends_with(&pattern[1..]) && host.len() > pattern.len() - 1
        } else {
            host == *pattern
        }
    })
}
