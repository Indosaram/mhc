//! Port of `permission-system/always-scope.ts` (senpi 903b8336).
//!
//! The wider "Allow always" a prompt may offer for a tool WITHOUT its own permission parser, where
//! plain "Allow always" remembers only the exact argument (one URL, one input) and so asks again for
//! the next one (senpi#3403):
//! - a URL argument: that URL's DOMAIN (scheme + canonical host + non-default port), stored as one
//!   literal and matched by equality, never the whole tool;
//! - a free `input`/`pattern` argument: the tool itself, for this project;
//! - a command or a path: nothing wider.

use regex::Regex;
use std::sync::LazyLock;

/// The extra option a prompt may offer, with the patterns an answer with it approves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WiderAlwaysScope {
    /// The option label, stating the scope in plain words.
    pub label: String,
    /// The patterns an answer with this option approves for the permission.
    pub patterns: Vec<String>,
}

/// Never widened, even if a future change routes one of them through the fallback.
const NEVER_WIDENED: &[&str] = &[
    "bash",
    "eval",
    "edit",
    "write",
    "read",
    "apply_patch",
    "multiedit",
    "glob",
    "grep",
    "ls",
];

/// `http(s)://`, an ASCII DNS host (no trailing dot), an optional port, then `/`, `?`, `#` or the end.
static UNAMBIGUOUS_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(https?)://([A-Za-z0-9](?:[A-Za-z0-9.-]*[A-Za-z0-9])?)(?::([0-9]{1,5}))?(?:[/?#]|$)",
    )
    .expect("static regex")
});
/// Backslash, `@`, `%`, JS `\s` whitespace (spelled out: Rust's `\s` adds U+0085 and drops
/// U+FEFF) and controls.
static AMBIGUOUS_CHARACTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"[\\@%\t\n\x0b\x0c\r \x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}\x00-\x1f\x7f]",
    )
    .expect("static regex")
});

fn default_port(scheme: &str) -> &'static str {
    match scheme {
        "http" => "80",
        "https" => "443",
        _ => "",
    }
}

/// The one normalizer both sides of a domain grant go through: a domain origin is given only for
/// raw text every parser reads as the same host (the strict shape, no ambiguous character, and the
/// same host and port as the WHATWG parser finds).
pub fn canonical_url_origin(raw: &str) -> Option<String> {
    if AMBIGUOUS_CHARACTER.is_match(raw) {
        return None;
    }
    let captures = UNAMBIGUOUS_URL.captures(raw)?;
    let scheme = captures.get(1)?.as_str().to_ascii_lowercase();
    let host = captures.get(2)?.as_str().to_ascii_lowercase();
    let port = match captures.get(3).map(|port| port.as_str()) {
        None => String::new(),
        Some(port) if port == default_port(&scheme) => String::new(),
        Some(port) => port.parse::<u32>().ok()?.to_string(),
    };
    let url = url::Url::parse(raw).ok()?;
    let url_port = url.port().map(|port| port.to_string()).unwrap_or_default();
    if url.scheme() != scheme || url.host_str() != Some(host.as_str()) || url_port != port {
        return None;
    }
    Some(if port.is_empty() {
        format!("{scheme}://{host}")
    } else {
        format!("{scheme}://{host}:{port}")
    })
}

/// The match target a domain grant is written for: a literal without wildcard characters, prefixed
/// with `origin=` so it never collides with a user's own URL rules.
pub fn url_origin_alias(raw: &str) -> Option<String> {
    canonical_url_origin(raw).map(|origin| format!("origin={origin}"))
}

pub fn wider_always_scope(tool_name: &str, arg_key: &str, arg: &str) -> Option<WiderAlwaysScope> {
    if NEVER_WIDENED.contains(&tool_name) {
        return None;
    }
    if arg_key == "url" {
        let origin = canonical_url_origin(arg)?;
        let host = origin
            .find("//")
            .map_or(origin.as_str(), |index| &origin[index + 2..]);
        return Some(WiderAlwaysScope {
            label: format!("Allow {tool_name} on {host} always in this project"),
            patterns: vec![format!("origin={origin}")],
        });
    }
    if arg_key == "input" || arg_key == "pattern" {
        return Some(WiderAlwaysScope {
            label: format!("Allow {tool_name} always in this project"),
            patterns: vec!["*".into()],
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        evaluate::evaluate,
        types::{Action, Rule},
    };

    /// Decides `url` under the domain grant offered for `grant_url`, as the permission system does.
    fn allowed_under(grant_url: &str, url: &str) -> bool {
        let scope = wider_always_scope("fetch_fixture", "url", grant_url).expect("domain scope");
        let rules: Vec<Rule> = scope
            .patterns
            .iter()
            .map(|pattern| Rule {
                permission: "fetch_fixture".into(),
                pattern: pattern.clone(),
                action: Action::Allow,
            })
            .collect();
        let mut targets = vec![url.to_owned()];
        targets.extend(url_origin_alias(url));
        targets.iter().any(|target| {
            evaluate("fetch_fixture", target, &[rules.as_slice()]).action == Action::Allow
        })
    }

    #[test]
    fn allows_only_the_grants_own_origin_written_any_way_the_url_parser_normalizes_it() {
        let grant = "https://example.invalid/a";
        for url in [
            "https://example.invalid",
            "https://example.invalid/deep/path",
            "https://example.invalid/?x=1",
            "https://example.invalid?x=1",
            "https://EXAMPLE.invalid/x",
            "https://example.invalid:443/x",
        ] {
            assert!(allowed_under(grant, url), "{url}");
        }
    }

    #[test]
    fn refuses_look_alike_hosts_other_ports_other_schemes_and_paths_that_mention_the_host() {
        let grant = "https://example.invalid/a";
        for url in [
            "https://example.invalid.evil.test/",
            "https://example.invalid@evil.test/",
            "https://evil.test/example.invalid",
            "https://evil.test/.example.invalid/x",
            "https://example.invalid:8443/",
            "http://example.invalid/",
        ] {
            assert!(!allowed_under(grant, url), "{url}");
        }
    }

    #[test]
    fn refuses_any_raw_url_other_parsers_could_read_as_another_host() {
        let grant = "https://example.invalid/a";
        for url in [
            "https://example.invalid\\@evil.test/",
            "https://example.invalid\\.evil.test/",
            "https://user:pass@example.invalid/x",
            "https://example%2Einvalid/x",
            " https://example.invalid/x",
            "https://example.invalid/x\t",
            "https://exa\tmple.invalid/x",
            "https://example.invalid\r\n.evil.test/",
            "https://\u{ff45}xample.invalid/x",
        ] {
            assert!(!allowed_under(grant, url), "{url:?}");
        }
        assert_eq!(
            wider_always_scope(
                "fetch_fixture",
                "url",
                "https://example.invalid\\@evil.test/"
            ),
            None
        );
        assert_eq!(
            wider_always_scope("fetch_fixture", "url", "https://user@example.invalid/"),
            None
        );
    }

    #[test]
    fn never_offers_a_domain_scope_for_a_host_with_glob_or_regex_characters() {
        assert_eq!(
            wider_always_scope("fetch_fixture", "url", "https://*.example.invalid/a"),
            None
        );
        assert_eq!(
            wider_always_scope("fetch_fixture", "url", "https://*/a"),
            None
        );
        assert_eq!(
            wider_always_scope("fetch_fixture", "url", "https://exa[mple.invalid/"),
            None
        );
        assert_eq!(
            wider_always_scope("fetch_fixture", "url", "https://exa?mple.invalid/")
                .map(|scope| scope.label),
            Some("Allow fetch_fixture on exa always in this project".into())
        );
    }

    #[test]
    fn gives_an_idn_host_a_scope_only_when_it_is_written_in_punycode() {
        assert_eq!(
            wider_always_scope("fetch_fixture", "url", "https://b\u{fc}cher.example/a"),
            None
        );
        assert_eq!(
            wider_always_scope("fetch_fixture", "url", "https://xn--bcher-kva.example/a")
                .map(|scope| scope.label),
            Some("Allow fetch_fixture on xn--bcher-kva.example always in this project".into())
        );
        assert!(allowed_under(
            "https://xn--bcher-kva.example/a",
            "https://XN--BCHER-KVA.example/b"
        ));
        assert!(!allowed_under(
            "https://xn--bcher-kva.example/a",
            "https://b\u{fc}cher.example/c"
        ));
    }

    #[test]
    fn refuses_ipv6_literals_trailing_dot_hosts_and_an_at_anywhere() {
        assert_eq!(
            wider_always_scope("fetch_fixture", "url", "https://[::1]/a"),
            None
        );
        assert_eq!(
            wider_always_scope("fetch_fixture", "url", "https://example.invalid./a"),
            None
        );
        assert!(!allowed_under(
            "https://example.invalid/a",
            "https://example.invalid./b"
        ));
        assert!(!allowed_under(
            "https://example.invalid/a",
            "https://example.invalid/path@evil.test"
        ));
    }

    #[test]
    fn keeps_a_non_default_port_in_the_scope_and_its_label() {
        assert_eq!(
            wider_always_scope("fetch_fixture", "url", "https://example.invalid:8443/a")
                .map(|scope| scope.label),
            Some("Allow fetch_fixture on example.invalid:8443 always in this project".into())
        );
        assert!(!allowed_under(
            "https://example.invalid:8443/a",
            "https://example.invalid/"
        ));
    }

    #[test]
    fn offers_nothing_for_a_command_a_path_a_non_http_url_or_a_built_in_tool() {
        assert_eq!(
            wider_always_scope("shell_fixture", "command", "echo hi"),
            None
        );
        assert_eq!(wider_always_scope("tool", "path", "/etc/passwd"), None);
        assert_eq!(
            wider_always_scope("tool", "url", "file:///etc/passwd"),
            None
        );
        assert_eq!(wider_always_scope("bash", "input", "x"), None);
    }
}
