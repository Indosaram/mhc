use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub fn fold_tool_name(name: &str) -> String {
    name.to_lowercase().replace(['-', '_'], "")
}

pub fn tool_name_forms(name: &str) -> Vec<&str> {
    let mut forms = vec![name];
    if name
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("mcp__"))
    {
        let rest = &name[5..];
        let bytes = rest.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] != b'_' {
                index += 1;
                continue;
            }
            let start = index;
            while index < bytes.len() && bytes[index] == b'_' {
                index += 1;
            }
            if index - start >= 2 && start > 0 && index < bytes.len() {
                forms.push(&rest[index..]);
            }
        }
        return forms;
    }
    if name
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("mcp_"))
    {
        let rest = &name[4..];
        if let Some((server, tool)) = rest.split_once('_')
            && !server.is_empty()
            && !tool.is_empty()
        {
            forms.push(tool);
        }
    }
    forms
}

fn unique_match(names: &BTreeSet<String>, matches: impl Fn(&str) -> bool) -> Option<&str> {
    let mut found = names.iter().filter(|name| matches(name));
    let first = found.next()?;
    if found.next().is_some() {
        None
    } else {
        Some(first)
    }
}

/// Some OpenAI-compatible gateways rewrite a tool name to `<prefix>_<sha256(name)[..12]>` on the
/// way out. The prefix is arbitrary and sometimes corrupted (`clnt_`, `clint_`, `clit_`), but the
/// digest is stable, so a model that echoes the rewritten name back can still be routed to the
/// tool it meant instead of failing as an unknown tool.
fn digest_suffix(requested: &str) -> Option<String> {
    let (_, suffix) = requested.rsplit_once('_')?;
    (suffix.len() == 12 && suffix.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| suffix.to_ascii_lowercase())
}

fn digest_prefix(name: &str) -> String {
    hex::encode(Sha256::digest(name.as_bytes()))[..12].to_owned()
}

fn resolve_digest_alias(requested: &str, names: &BTreeSet<String>) -> Option<String> {
    let suffix = digest_suffix(requested)?;
    unique_match(names, |name| digest_prefix(name) == suffix).map(str::to_owned)
}

pub fn resolve_tool_name_match(
    requested: &str,
    available: impl IntoIterator<Item = String>,
) -> Option<String> {
    let names: BTreeSet<String> = available.into_iter().collect();
    for form in tool_name_forms(requested) {
        if names.contains(form) {
            return Some(form.to_owned());
        }
        let key = fold_tool_name(form);
        if let Some(name) = unique_match(&names, |name| fold_tool_name(name) == key) {
            return Some(name.to_owned());
        }
        if let Some(name) = unique_match(&names, |name| {
            tool_name_forms(name)
                .into_iter()
                .skip(1)
                .any(|form| fold_tool_name(form) == key)
        }) {
            return Some(name.to_owned());
        }
    }
    resolve_digest_alias(requested, &names)
}

#[cfg(test)]
mod tests {
    use super::resolve_tool_name_match;

    #[test]
    fn resolves_gateway_suffixes_and_registered_server_forms() {
        for (requested, available, expected) in [
            ("MCP__my_server__Memory", vec!["memory"], Some("memory")),
            (
                "mcp__my_srv__x__Tool",
                vec!["x__tool", "Tool"],
                Some("x__tool"),
            ),
            ("mcp__my_srv__x__Tool", vec!["tool"], Some("tool")),
            (
                "create_issue",
                vec!["mcp_github_create_issue"],
                Some("mcp_github_create_issue"),
            ),
            ("mcp__sandbox__run_bash", vec!["bash"], None),
            (
                "create_issue",
                vec!["mcp_a_create_issue", "mcp_b_create_issue"],
                None,
            ),
            ("mcp__a__READ", vec!["read", "Read"], None),
            (
                "mcp__a__READ",
                vec!["mcp__a__read", "READ"],
                Some("mcp__a__read"),
            ),
            ("Read", vec!["Read", "read"], Some("Read")),
            ("read", vec!["read", "read"], Some("read")),
            ("mcp__a___Read", vec!["read"], Some("read")),
            ("clnt_1bbd174404ef", vec!["eval"], Some("eval")),
            ("clit_1bbd174404ef", vec!["eval"], Some("eval")),
            ("clnt_1bbd174404ef", vec!["read"], None),
            ("clnt_1bbd174404e", vec!["eval"], None),
        ] {
            // Given: registered tools and a provider-emitted name.
            let available = available.into_iter().map(str::to_owned);
            // When: resolving the name through the shared matching algorithm.
            let resolved = resolve_tool_name_match(requested, available);
            // Then: use the most specific unique match, never a guessed suffix.
            assert_eq!(resolved.as_deref(), expected, "{requested}");
        }
    }
}
