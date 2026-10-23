//! Port of senpi packages/coding-agent/src/core/share/share-redaction.ts.
//!
//! Redaction for content that leaves the machine (`/share`). Two stages, in order: identity first
//! (the real home directory, the account name and the host name, in every spelling a path takes
//! inside JSON, URLs and Windows text), then generic shapes (credentials, private keys, emails,
//! addresses). Every replacement is counted from its own callback, so the preview can say what was
//! removed. The rewrite is idempotent: running it on its own output changes nothing and counts nothing.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use fancy_regex::{Captures, Regex, RegexBuilder};

use super::share_entropy::redact_generated_values;
use super::share_sensitive_key::is_sensitive_key_name;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShareRedactionContext {
    pub home_directory: String,
    pub user_name: String,
    pub host_name: String,
    pub extra_literals: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ShareRedactionCategory {
    HomePath,
    UserName,
    HostName,
    PrivateKey,
    Credential,
    Email,
    IpAddress,
    Image,
}

impl ShareRedactionCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HomePath => "home path",
            Self::UserName => "user name",
            Self::HostName => "host name",
            Self::PrivateKey => "private key",
            Self::Credential => "credential",
            Self::Email => "email",
            Self::IpAddress => "ip address",
            Self::Image => "image",
        }
    }
}

pub type ShareRedactionTally = BTreeMap<ShareRedactionCategory, usize>;

pub fn count(tally: &mut ShareRedactionTally, category: ShareRedactionCategory) {
    *tally.entry(category).or_insert(0) += 1;
}

/// This machine's identity: home directory, account name and host name.
pub fn current_share_redaction_context() -> ShareRedactionContext {
    ShareRedactionContext {
        home_directory: crate::config::home_dir(),
        user_name: account_name(),
        host_name: host_name(),
        extra_literals: Vec::new(),
    }
}

fn account_name() -> String {
    #[cfg(unix)]
    if let Ok(Some(user)) = nix::unistd::User::from_uid(nix::unistd::Uid::current()) {
        return user.name;
    }
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default()
}

fn host_name() -> String {
    if let Ok(name) = std::env::var("HOSTNAME")
        && !name.trim().is_empty()
    {
        return name.trim().to_owned();
    }
    if let Ok(name) = std::fs::read_to_string("/etc/hostname")
        && !name.trim().is_empty()
    {
        return name.trim().to_owned();
    }
    std::env::var("COMPUTERNAME").unwrap_or_default()
}

const MIN_LITERAL_LENGTH: usize = 3;
const IDENTITY_CHAR: &str = r"[\p{L}\p{N}\p{M}_.-]";
const WORD_CHAR: &str = r"[\p{L}\p{N}\p{M}]";
/// Every spelling a separator takes in exported text: `/`, `\`, JSON `\\`, `%2F`/`%5C`.
const ANY_SEPARATOR: &str = r"(?:/|\\{1,2}|%2[Ff]|%5[Cc])";
/// Account names too generic to scrub as words without garbling ordinary text.
const COMMON_NAMES: [&str; 10] = [
    "admin", "user", "users", "test", "home", "root", "runner", "guest", "default", "shared",
];

type Rewrite = fn(&Captures) -> String;

struct Patterns {
    other_home: Regex,
    private_key: Regex,
    credential_rewrites: Vec<(Regex, Rewrite)>,
    key_value: Regex,
    key_collection: Regex,
    key_yaml_list: Regex,
    yaml_item: Regex,
    already_redacted: Regex,
    quoted: Regex,
    data_uri: Regex,
    email: Regex,
    ipv4: Regex,
    placeholder: Regex,
}

fn compile(pattern: &str) -> Result<Regex, Box<fancy_regex::Error>> {
    RegexBuilder::new(pattern)
        .backtrack_limit(500_000_000)
        .build()
        .map_err(Box::new)
}

fn group<'a>(caps: &'a Captures, index: usize) -> &'a str {
    caps.get(index).map_or("", |found| found.as_str())
}

fn redacted_whole(_: &Captures) -> String {
    "<redacted>".to_owned()
}

fn rewrite_scheme(caps: &Captures) -> String {
    format!("{}{}<redacted>", group(caps, 1), group(caps, 2))
}

fn rewrite_userinfo(caps: &Captures) -> String {
    format!("{}<redacted>@", group(caps, 1))
}

fn rewrite_cookie(caps: &Captures) -> String {
    format!("{}{}<redacted>", group(caps, 1), group(caps, 2))
}

impl Patterns {
    fn build() -> Result<Self, Box<fancy_regex::Error>> {
        let sep = ANY_SEPARATOR;
        let mut credential_rewrites: Vec<(Regex, Rewrite)> = Vec::new();
        // A bare scheme word is prose unless the value looks like a credential (has a digit or symbol). A value that
        // starts with an earlier placeholder is still a value: the placeholder is replaced along with it.
        credential_rewrites.push((compile(r"(?i)\b(Bearer|Basic|Token)(\s{1,5})(?:<redacted>)?(?=[A-Za-z]*[0-9._~+/=-])[A-Za-z0-9._~+/=-]{8,}")?, rewrite_scheme));
        credential_rewrites.push((
            compile(r"\b([A-Za-z][A-Za-z0-9+.-]{0,20}://[^\s/:@]{1,64}:)[^\s/@]{1,256}@")?,
            rewrite_userinfo,
        ));
        for pattern in [
            r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
            r"\b(?:sk|pk|rk)-[A-Za-z0-9_-]{16,}",
            r"\bgh[pousr]_[A-Za-z0-9]{20,}",
            r"\bgithub_pat_[A-Za-z0-9_]{20,}",
            r"\bxox[abprs]-[A-Za-z0-9-]{10,}",
            r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
            r"\bAIza[0-9A-Za-z_-]{35}\b",
            r"\b(?:sk|pk|rk)_(?:live|test)_[A-Za-z0-9]{10,}",
            r"\bhf_[A-Za-z0-9]{30,}",
            r"\bnpm_[A-Za-z0-9]{36}\b",
            r"\bglpat-[A-Za-z0-9_-]{20,}",
        ] {
            credential_rewrites.push((compile(pattern)?, redacted_whole));
        }
        credential_rewrites.push((
            compile(r#"(?i)(?<![A-Za-z0-9-])((?:Set-)?Cookie)(\s{0,3}:\s{0,3})[^\n"]+"#)?,
            rewrite_cookie,
        ));
        Ok(Self {
            other_home: compile(&format!(
                r#"(?i)(?:[A-Za-z]:)?{sep}(?:Users|home){sep}[^\s/\\'"%:)<>\]]{{1,64}}"#
            ))?,
            private_key: compile(
                r"-----BEGIN [A-Z0-9 ]{0,40}PRIVATE KEY(?: BLOCK)?-----(?:[\s\S]*?-----END [A-Z0-9 ]{0,40}PRIVATE KEY(?: BLOCK)?-----|[\s\S]*$)",
            )?,
            credential_rewrites,
            // `key: value` / `key=value` in text. A quoted value runs to its closing quote; an unquoted one to the end of
            // the line or the next `,;})`, so a value of several words is replaced whole.
            key_value: compile(
                r#"([A-Za-z0-9_.\[\]-]{1,128})((?:\\?["'])?[ \t]{0,3}[:=][ \t]{0,3})(?:(\\?["'])((?:(?!\3)[^\n]){0,2000})\3|([^\s"'\\&,;}\[{(=)][^\n"'\\&,;}()]{0,1999}))"#,
            )?,
            // A sensitive key whose value is an inline array or object: every quoted string inside is a credential.
            key_collection: compile(
                r#"((["']?)([A-Za-z0-9_.-]{1,128})\2[ \t]{0,3}[:=][ \t]{0,3})(\[[^\]\n]{0,2000}\]|\{[^}\n]{0,2000}\})"#,
            )?,
            // A YAML key followed by a block list (`tokens:` then `  - value` lines).
            key_yaml_list: compile(
                r"(?m)^([ \t]*)([A-Za-z0-9_.-]{1,128}):[ \t]*\r?\n((?:\1[ \t]*-[ \t]+[^\n]*(?:\r?\n|$))+)",
            )?,
            yaml_item: compile(r"(?m)^([ \t]*-[ \t]+)(\S[^\n]*)")?,
            // Counting only: a value that is nothing but a scheme word and placeholders was already counted.
            already_redacted: compile(
                r"(?i)^(?:(?:Bearer|Basic|Token|Digest)\s+)?(?:<redacted>\s*)+$",
            )?,
            quoted: compile(r#""(?:[^"\\\n]|\\.)*"|'[^'\n]*'"#)?,
            data_uri: compile(r"(?i)data:[a-z]+/[a-z0-9.+-]+;base64,[A-Za-z0-9+/=]+")?,
            email: compile(
                r"(?<![A-Za-z0-9._%+-])[A-Za-z0-9._%+-]{1,64}(?:@|%40)[A-Za-z0-9-]{1,63}(?:\.[A-Za-z0-9-]{1,63}){1,5}",
            )?,
            ipv4: compile(
                r"(?<![0-9])(?<![0-9]\.)(?:[0-9]{1,3}\.){3}[0-9]{1,3}(?![0-9])(?!\.[0-9])",
            )?,
            // Placeholders this redactor writes. The literal pass runs last, so it must not rewrite one an earlier pass produced.
            placeholder: compile(
                r"<(?:redacted(?: private key)?|email|ip|host|user)>|\[image omitted from share\]",
            )?,
        })
    }
}

fn patterns() -> Option<&'static Patterns> {
    static PATTERNS: OnceLock<Option<Patterns>> = OnceLock::new();
    PATTERNS.get_or_init(|| Patterns::build().ok()).as_ref()
}

/// `String.replace(regex, fn)`. A match error (backtrack limit) fails closed: the rest of the text is dropped.
fn replace_all(
    pattern: &Regex,
    text: &str,
    mut replace: impl FnMut(&Captures) -> String,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for captures in pattern.captures_iter(text) {
        let Ok(captures) = captures else {
            out.push_str("<redacted>");
            return out;
        };
        let Some(whole) = captures.get(0) else {
            continue;
        };
        out.push_str(&text[last..whole.start()]);
        out.push_str(&replace(&captures));
        last = whole.end();
    }
    out.push_str(&text[last..]);
    out
}

fn escape_regexp(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 4);
    for ch in value.chars() {
        if ".*+?^${}()|[]\\".contains(ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn home_pattern(home_directory: &str) -> Option<Regex> {
    let mut segments: Vec<&str> = home_directory
        .split(['\\', '/'])
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.is_empty() {
        return None;
    }
    let drive = segments
        .first()
        .filter(|first| {
            first.len() == 2
                && first.ends_with(':')
                && first
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic())
        })
        .copied();
    if drive.is_some() {
        segments.remove(0);
    }
    let body = segments
        .iter()
        .map(|segment| escape_regexp(segment).replace(' ', r"(?: |%20|\+)"))
        .collect::<Vec<_>>()
        .join(ANY_SEPARATOR);
    let prefix = match drive {
        None => ANY_SEPARATOR.to_owned(),
        Some(drive) => format!("{}{ANY_SEPARATOR}", escape_regexp(drive)),
    };
    compile(&format!(
        "(?i)(?:file:{ANY_SEPARATOR}{{0,3}})?{prefix}{body}(?!{IDENTITY_CHAR})"
    ))
    .ok()
}

fn literal_pattern(value: &str) -> Option<Regex> {
    let trimmed = value.trim();
    if trimmed.chars().count() < MIN_LITERAL_LENGTH
        || COMMON_NAMES.contains(&trimmed.to_lowercase().as_str())
    {
        return None;
    }
    compile(&format!(
        "(?i)(?<!{WORD_CHAR}){}(?!{WORD_CHAR})",
        escape_regexp(trimmed)
    ))
    .ok()
}

fn keeps_address(address: &str) -> bool {
    address.starts_with("127.") || address == "0.0.0.0"
}

fn redact_quoted(quoted: &str, tally: &mut ShareRedactionTally) -> String {
    let quote = quoted.chars().next().unwrap_or('"');
    if quoted.chars().count() <= 2 {
        return quoted.to_owned();
    }
    let replaced = format!("{quote}<redacted>{quote}");
    if replaced != quoted {
        count(tally, ShareRedactionCategory::Credential);
    }
    replaced
}

/// Replaces `pattern` in `text` everywhere except inside placeholders an earlier pass wrote.
fn replace_outside_placeholders(
    placeholder: &Regex,
    text: &str,
    pattern: &Regex,
    replace: &mut dyn FnMut() -> String,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for found in placeholder.find_iter(text) {
        let Ok(found) = found else { continue };
        out.push_str(&replace_all(pattern, &text[last..found.start()], |_| {
            replace()
        }));
        out.push_str(found.as_str());
        last = found.end();
    }
    out.push_str(&replace_all(pattern, &text[last..], |_| replace()));
    out
}

pub fn redact_share_text(
    text: &str,
    context: &ShareRedactionContext,
    tally: &mut ShareRedactionTally,
) -> String {
    if text.is_empty() {
        return String::new();
    }
    // Without its patterns the redactor cannot vouch for anything: nothing leaves.
    let Some(p) = patterns() else {
        return "<redacted>".to_owned();
    };
    let mut result = text.to_owned();

    if let Some(home) = home_pattern(&context.home_directory) {
        result = replace_all(&home, &result, |_| {
            count(tally, ShareRedactionCategory::HomePath);
            "~".to_owned()
        });
    }
    result = replace_all(&p.other_home, &result, |caps| {
        count(tally, ShareRedactionCategory::HomePath);
        let matched = group(caps, 0);
        let keep = matched.trim_end_matches(|c| !matches!(c, '/' | '\\' | '%'));
        format!("{keep}<user>")
    });
    result = replace_all(&p.data_uri, &result, |_| {
        count(tally, ShareRedactionCategory::Image);
        "[image omitted from share]".to_owned()
    });
    result = replace_all(&p.private_key, &result, |_| {
        count(tally, ShareRedactionCategory::PrivateKey);
        "<redacted private key>".to_owned()
    });
    result = replace_all(&p.key_collection, &result, |caps| {
        if !is_sensitive_key_name(group(caps, 3)) {
            return group(caps, 0).to_owned();
        }
        let redacted = replace_all(&p.quoted, group(caps, 4), |quoted| {
            redact_quoted(group(quoted, 0), tally)
        });
        format!("{}{redacted}", group(caps, 1))
    });
    result = replace_all(&p.key_yaml_list, &result, |caps| {
        if !is_sensitive_key_name(group(caps, 2)) {
            return group(caps, 0).to_owned();
        }
        let items = replace_all(&p.yaml_item, group(caps, 3), |item| {
            if group(item, 2) != "<redacted>" {
                count(tally, ShareRedactionCategory::Credential);
            }
            format!("{}<redacted>", group(item, 1))
        });
        format!("{}{}:\n{items}", group(caps, 1), group(caps, 2))
    });
    for (pattern, rewrite) in &p.credential_rewrites {
        result = replace_all(pattern, &result, |caps| {
            let replaced = rewrite(caps);
            if replaced != group(caps, 0) {
                count(tally, ShareRedactionCategory::Credential);
            }
            replaced
        });
    }
    result = replace_all(&p.key_value, &result, |caps| {
        let whole = group(caps, 0);
        let quote = caps.get(3).map(|found| found.as_str());
        let bare = caps.get(5).map(|found| found.as_str());
        if !is_sensitive_key_name(group(caps, 1)) || (quote.is_none() && bare == Some("<redacted>"))
        {
            return whole.to_owned();
        }
        let (key, separator) = (group(caps, 1), group(caps, 2));
        let (replaced, value) = match quote {
            None => {
                let bare = bare.unwrap_or("");
                let trailing = &bare[bare.trim_end().len()..];
                (format!("{key}{separator}<redacted>{trailing}"), bare)
            }
            Some(quote) => (
                format!("{key}{separator}{quote}<redacted>{quote}"),
                group(caps, 4),
            ),
        };
        if replaced != whole && !p.already_redacted.is_match(value.trim()).unwrap_or(false) {
            count(tally, ShareRedactionCategory::Credential);
        }
        replaced
    });
    result = redact_generated_values(&result, &mut || {
        count(tally, ShareRedactionCategory::Credential)
    });

    result = replace_all(&p.email, &result, |_| {
        count(tally, ShareRedactionCategory::Email);
        "<email>".to_owned()
    });
    result = replace_all(&p.ipv4, &result, |caps| {
        let address = group(caps, 0);
        if keeps_address(address) {
            return address.to_owned();
        }
        count(tally, ShareRedactionCategory::IpAddress);
        "<ip>".to_owned()
    });

    let short_host = context.host_name.split('.').next().unwrap_or("").to_owned();
    let mut literals: Vec<(&str, &str, ShareRedactionCategory)> = vec![
        (
            &context.host_name,
            "<host>",
            ShareRedactionCategory::HostName,
        ),
        (&short_host, "<host>", ShareRedactionCategory::HostName),
        (
            &context.user_name,
            "<user>",
            ShareRedactionCategory::UserName,
        ),
    ];
    literals.extend(
        context
            .extra_literals
            .iter()
            .map(|value| (value.as_str(), "<user>", ShareRedactionCategory::UserName)),
    );
    for (value, placeholder, category) in literals {
        let Some(pattern) = literal_pattern(value) else {
            continue;
        };
        result = replace_outside_placeholders(&p.placeholder, &result, &pattern, &mut || {
            count(tally, category);
            placeholder.to_owned()
        });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> ShareRedactionContext {
        ShareRedactionContext {
            home_directory: "/home/alice".to_owned(),
            user_name: "alice".to_owned(),
            host_name: "devbox.local".to_owned(),
            extra_literals: Vec::new(),
        }
    }

    fn redact(text: &str) -> (String, ShareRedactionTally) {
        let mut tally = ShareRedactionTally::new();
        (redact_share_text(text, &context(), &mut tally), tally)
    }

    #[test]
    fn patterns_compile() {
        assert!(patterns().is_some());
    }

    #[test]
    fn home_path_becomes_tilde_and_other_homes_become_user() {
        let (text, tally) = redact("open /home/alice/project/a.rs and /Users/bob/x");
        assert_eq!(text, "open ~/project/a.rs and /Users/<user>/x");
        assert_eq!(tally.get(&ShareRedactionCategory::HomePath), Some(&2));
    }

    #[test]
    fn credentials_private_keys_emails_and_addresses_are_replaced() {
        let (text, tally) = redact(
            "Authorization: Bearer abcdef123456\nmail bob@example.com ip 10.1.2.3 keep 127.0.0.1",
        );
        assert!(text.contains("Bearer <redacted>") || text.contains("Authorization: <redacted>"));
        assert!(text.contains("<email>") && text.contains("<ip>") && text.contains("127.0.0.1"));
        assert_eq!(tally.get(&ShareRedactionCategory::Email), Some(&1));
        let (key, tally) =
            redact("-----BEGIN RSA PRIVATE KEY-----\nabc\n-----END RSA PRIVATE KEY-----");
        assert_eq!(key, "<redacted private key>");
        assert_eq!(tally.get(&ShareRedactionCategory::PrivateKey), Some(&1));
    }

    #[test]
    fn sensitive_keys_lose_their_values() {
        let (text, _) = redact(r#"{"apiKey": "hunter2hunter2", "name": "x"}"#);
        assert!(text.contains(r#""apiKey": "<redacted>""#) && text.contains(r#""name": "x""#));
    }

    #[test]
    fn data_uris_are_omitted_and_identity_literals_are_scrubbed() {
        let (text, tally) = redact("img data:image/png;base64,AAAA host devbox user alice");
        assert!(
            text.contains("[image omitted from share]")
                && text.contains("<host>")
                && text.contains("<user>")
        );
        assert_eq!(tally.get(&ShareRedactionCategory::Image), Some(&1));
    }

    #[test]
    fn redaction_is_idempotent_and_leaves_placeholders_alone() {
        let (once, _) = redact("token=abcdef1234567890 mail bob@example.com /home/alice/x 8.8.8.8");
        let mut tally = ShareRedactionTally::new();
        let twice = redact_share_text(&once, &context(), &mut tally);
        assert_eq!(once, twice);
        assert!(tally.is_empty());
        let mut literal_context = context();
        literal_context.extra_literals = vec!["redacted".to_owned()];
        let mut tally = ShareRedactionTally::new();
        assert_eq!(
            redact_share_text("<redacted>", &literal_context, &mut tally),
            "<redacted>"
        );
    }
}
