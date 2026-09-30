//! Port of TypeBox 1.3.34 `format/*` (the default format registry used by `Compile`).
//!
//! Regex sources are copied verbatim from the TypeBox build; the bool is the JS `i` flag.

use std::sync::LazyLock;

use fancy_regex::Regex;
use unicode_general_category::{GeneralCategory, get_general_category};
use unicode_normalization::UnicodeNormalization;

use crate::utils::js::utf16_len;

const DURATION: (&str, bool) = (r#"^P((\d+Y(\d+M(\d+D)?)?|\d+M(\d+D)?|\d+D)(T(\d+H(\d+M(\d+S)?)?|\d+M(\d+S)?|\d+S))?|T(\d+H(\d+M(\d+S)?)?|\d+M(\d+S)?|\d+S)|\d+W)$"#, false);
const EMAIL: (&str, bool) = (r#"^(?:[a-z0-9!#$%&'*+/=?^_`{|}~-]+(?:\.[a-z0-9!#$%&'*+/=?^_`{|}~-]+)*|"(?:[^"\\]|\\[\x20-\x7e])*")@(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)*|\[(?:IPv6:[a-f0-9:]+|(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])(?:\.(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])){3})\])$"#, true);
const IDN_EMAIL: (&str, bool) = (r#"^(?:[A-Za-z0-9!#$%&'*+\/=?^_`{|}~\u{0080}-\u{10FFFF}-]+(?:\.[A-Za-z0-9!#$%&'*+\/=?^_`{|}~\u{0080}-\u{10FFFF}-]+)*|"(?:[^"\\]|\\.)*")@[\p{L}\p{N}](?:[\p{L}\p{N}-]{0,62})(?<!-)(?:\.[\p{L}\p{N}](?:[\p{L}\p{N}-]{0,62})(?<!-))*$"#, true);
const IPV4: (&str, bool) = (r#"^(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)$"#, false);
const IPV6: (&str, bool) = (r#"^(?:(?:(?:[0-9a-f]{1,4}:){6}|::(?:[0-9a-f]{1,4}:){5}|(?:[0-9a-f]{1,4})?::(?:[0-9a-f]{1,4}:){4}|(?:(?:[0-9a-f]{1,4}:)?[0-9a-f]{1,4})?::(?:[0-9a-f]{1,4}:){3}|(?:(?:[0-9a-f]{1,4}:){0,2}[0-9a-f]{1,4})?::(?:[0-9a-f]{1,4}:){2}|(?:(?:[0-9a-f]{1,4}:){0,3}[0-9a-f]{1,4})?::[0-9a-f]{1,4}:|(?:(?:[0-9a-f]{1,4}:){0,4}[0-9a-f]{1,4})?::)(?:[0-9a-f]{1,4}:[0-9a-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[0-9a-f]{1,4}:){0,5}[0-9a-f]{1,4})?::[0-9a-f]{1,4}|(?:(?:[0-9a-f]{1,4}:){0,6}[0-9a-f]{1,4})?::)$"#, true);
const JSON_POINTER_URI_FRAGMENT: (&str, bool) = (r#"^#(?:\/(?:[a-z0-9_\-.!$&'()*+,;:=@]|%[0-9a-f]{2}|~0|~1)*)*$"#, true);
const JSON_POINTER: (&str, bool) = (r#"^(?:\/(?:[^~/]|~0|~1)*)*$"#, false);
const RELATIVE_JSON_POINTER: (&str, bool) = (r#"^(?:0|[1-9][0-9]*)(?:#|(?:\/(?:[^~/]|~0|~1)*)*)$"#, false);
const URI_REFERENCE: (&str, bool) = (r#"^(?:[a-z][a-z0-9+\-.]*:(?:\/\/(?:(?:[-a-z0-9._~!$&'()*+,;=:]|%[0-9a-f]{2})*@)?(?:\[(?:(?:(?:[\da-f]{1,4}:){6}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|::(?:[\da-f]{1,4}:){5}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:[\da-f]{1,4})?::(?:[\da-f]{1,4}:){4}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,1}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){3}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,2}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){2}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,3}[\da-f]{1,4})?::[\da-f]{1,4}:(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,4}[\da-f]{1,4})?::(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,5}[\da-f]{1,4})?::[\da-f]{1,4}|(?:(?:[\da-f]{1,4}:){0,6}[\da-f]{1,4})?::)|v[0-9a-f]+\.[-a-z0-9._~!$&'()*+,;=:]+)\]|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)|(?:[-a-z0-9._~!$&'()*+,;=]|%[0-9a-f]{2})*)(?::\d*)?(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*|\/(?:(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?|(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?|(?:\/\/(?:(?:[-a-z0-9._~!$&'()*+,;=:]|%[0-9a-f]{2})*@)?(?:\[(?:(?:(?:[\da-f]{1,4}:){6}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|::(?:[\da-f]{1,4}:){5}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:[\da-f]{1,4})?::(?:[\da-f]{1,4}:){4}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,1}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){3}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,2}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){2}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,3}[\da-f]{1,4})?::[\da-f]{1,4}:(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,4}[\da-f]{1,4})?::(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,5}[\da-f]{1,4})?::[\da-f]{1,4}|(?:(?:[\da-f]{1,4}:){0,6}[\da-f]{1,4})?::)|v[0-9a-f]+\.[-a-z0-9._~!$&'()*+,;=:]+)\]|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)|(?:[-a-z0-9._~!$&'()*+,;=]|%[0-9a-f]{2})*)(?::\d*)?(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*|\/(?:(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?|(?:[-a-z0-9._~!$&'()*+,;=@]|%[0-9a-f]{2})+(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?)(?:\?(?:[-a-z0-9._~!$&'()*+,;=:@/?]|%[0-9a-f]{2})*)?(?:#(?:[-a-z0-9._~!$&'()*+,;=:@/?]|%[0-9a-f]{2})*)?$"#, true);
const URI_TEMPLATE: (&str, bool) = (r#"^(?:(?:[^\x00-\x20"<>%\\^`{|}\x7f]|%[0-9a-f]{2})|\{[+#./;?&=,!@|]?(?:[a-z0-9_]|%[0-9a-f]{2})+(?:\.(?:[a-z0-9_]|%[0-9a-f]{2})+)*(?::[1-9]\d{0,3}|\*)?(?:,(?:[a-z0-9_]|%[0-9a-f]{2})+(?:\.(?:[a-z0-9_]|%[0-9a-f]{2})+)*(?::[1-9]\d{0,3}|\*)?)*\})*$"#, true);
const URI: (&str, bool) = (r#"^[a-z][a-z0-9+\-.]*:(?:\/\/(?:(?:[-a-z0-9._~!$&'()*+,;=:]|%[0-9a-f]{2})*@)?(?:\[(?:(?:(?:[\da-f]{1,4}:){6}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|::(?:[\da-f]{1,4}:){5}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:[\da-f]{1,4})?::(?:[\da-f]{1,4}:){4}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,1}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){3}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,2}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){2}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,3}[\da-f]{1,4})?::[\da-f]{1,4}:(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,4}[\da-f]{1,4})?::(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,5}[\da-f]{1,4})?::[\da-f]{1,4}|(?:(?:[\da-f]{1,4}:){0,6}[\da-f]{1,4})?::)|v[0-9a-f]+\.[-a-z0-9._~!$&'()*+,;=:]+)\]|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)|(?:[-a-z0-9._~!$&'()*+,;=]|%[0-9a-f]{2})*)(?::\d*)?(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*|\/(?:(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?|(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?(?:\?(?:[-a-z0-9._~!$&'()*+,;=:@/?]|%[0-9a-f]{2})*)?(?:#(?:[-a-z0-9._~!$&'()*+,;=:@/?]|%[0-9a-f]{2})*)?$"#, true);
const UUID: (&str, bool) = (r#"^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$"#, true);
const IRI_INVALID_CHARS: (&str, bool) = (r#"[\x00-\x20<>\^`{|}\\]"#, false);
const IRI_REFERENCE_INVALID: (&str, bool) = (r#"[\x00-\x20\x7F\\]|%(?![0-9a-fA-F]{2})"#, false);
const DATE: (&str, bool) = (r#"^(\d\d\d\d)-(\d\d)-(\d\d)$"#, false);
const TIME: (&str, bool) = (r#"^(\d\d):(\d\d):(\d\d)(?:\.\d+)?(?:([Zz])|([+-])(\d\d):(\d\d))?$"#, false);
const IRI_INVALID_PERCENT: (&str, bool) = (r#"%(?![0-9a-fA-F]{2})"#, false);
const IRI_MALFORMED_SCHEME: (&str, bool) = (r#"^[a-zA-Z][a-zA-Z0-9+\-.]*\/\/"#, false);

/// Translates a JS `u`-mode regex source to fancy-regex: `\d`/`\w` stay ASCII as in JS.
pub(super) fn js_regex(source: &str, ignore_case: bool) -> Option<Regex> {
    let mut out = String::with_capacity(source.len() + 8);
    if ignore_case {
        out.push_str("(?i)");
    }
    let mut chars = source.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('d') => out.push_str("[0-9]"),
            Some('D') => out.push_str("[^0-9]"),
            Some('w') => out.push_str("[A-Za-z0-9_]"),
            Some('W') => out.push_str("[^A-Za-z0-9_]"),
            Some(next) => {
                out.push('\\');
                out.push(next);
            }
            None => out.push('\\'),
        }
    }
    Regex::new(&out).ok()
}

macro_rules! lazy_regex {
    ($name:ident, $source:expr) => {
        static $name: LazyLock<Option<Regex>> = LazyLock::new(|| js_regex($source.0, $source.1));
    };
}

lazy_regex!(RE_DURATION, DURATION);
lazy_regex!(RE_EMAIL, EMAIL);
lazy_regex!(RE_IDN_EMAIL, IDN_EMAIL);
lazy_regex!(RE_IPV4, IPV4);
lazy_regex!(RE_IPV6, IPV6);
lazy_regex!(RE_JSON_POINTER_URI_FRAGMENT, JSON_POINTER_URI_FRAGMENT);
lazy_regex!(RE_JSON_POINTER, JSON_POINTER);
lazy_regex!(RE_RELATIVE_JSON_POINTER, RELATIVE_JSON_POINTER);
lazy_regex!(RE_URI_REFERENCE, URI_REFERENCE);
lazy_regex!(RE_URI_TEMPLATE, URI_TEMPLATE);
lazy_regex!(RE_URI, URI);
lazy_regex!(RE_UUID, UUID);
lazy_regex!(RE_IRI_INVALID_CHARS, IRI_INVALID_CHARS);
lazy_regex!(RE_IRI_REFERENCE_INVALID, IRI_REFERENCE_INVALID);
lazy_regex!(RE_IRI_INVALID_PERCENT, IRI_INVALID_PERCENT);
lazy_regex!(RE_IRI_MALFORMED_SCHEME, IRI_MALFORMED_SCHEME);
lazy_regex!(RE_DATE, DATE);
lazy_regex!(RE_TIME, TIME);

fn test(regex: &LazyLock<Option<Regex>>, value: &str) -> bool {
    regex.as_ref().is_some_and(|re| re.is_match(value).unwrap_or(false))
}

/// `Format.Test`: unknown formats pass.
pub(super) fn test_format(format: &str, value: &str) -> bool {
    match format {
        "date-time" => is_date_time(value),
        "date" => is_date(value),
        "duration" => test(&RE_DURATION, value),
        "email" => test(&RE_EMAIL, value),
        "hostname" => is_hostname(value),
        "idn-email" => test(&RE_IDN_EMAIL, &value.nfc().collect::<String>()),
        "idn-hostname" => is_idn_hostname(value),
        "ipv4" => test(&RE_IPV4, value),
        "ipv6" => test(&RE_IPV6, value),
        "iri-reference" => {
            !test(&RE_IRI_REFERENCE_INVALID, value)
                && !test(&RE_IRI_MALFORMED_SCHEME, value)
                && url::Url::parse("http://example.com").and_then(|base| base.join(value)).is_ok()
        }
        "iri" => !test(&RE_IRI_INVALID_CHARS, value) && !test(&RE_IRI_INVALID_PERCENT, value) && url::Url::parse(value).is_ok(),
        "json-pointer-uri-fragment" => test(&RE_JSON_POINTER_URI_FRAGMENT, value),
        "json-pointer" => test(&RE_JSON_POINTER, value),
        "regex" => js_regex(value, false).is_some(),
        "relative-json-pointer" => test(&RE_RELATIVE_JSON_POINTER, value),
        "time" => is_time(value),
        "uri-reference" => test(&RE_URI_REFERENCE, value),
        "uri-template" => test(&RE_URI_TEMPLATE, value),
        "uri" => test(&RE_URI, value),
        "url" => url::Url::parse(value).is_ok(),
        "uuid" => test(&RE_UUID, value),
        _ => true,
    }
}

fn captures(regex: &LazyLock<Option<Regex>>, value: &str) -> Option<Vec<Option<String>>> {
    let captures = regex.as_ref()?.captures(value).ok()??;
    Some(captures.iter().map(|group| group.map(|m| m.as_str().to_owned())).collect())
}

fn group_number(groups: &[Option<String>], index: usize) -> u32 {
    groups.get(index).and_then(|g| g.as_deref()).and_then(|g| g.parse().ok()).unwrap_or(0)
}

fn is_date(value: &str) -> bool {
    const DAYS: [u32; 13] = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let Some(groups) = captures(&RE_DATE, value) else { return false };
    let (year, month, day) = (group_number(&groups, 1), group_number(&groups, 2) as usize, group_number(&groups, 3));
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    (1..=12).contains(&month) && day >= 1 && day <= if month == 2 && leap { 29 } else { DAYS[month] }
}

fn is_time(value: &str) -> bool {
    let Some(groups) = captures(&RE_TIME, value) else { return false };
    let has = |index: usize| groups.get(index).is_some_and(Option::is_some);
    if !has(4) && !has(5) {
        return false;
    }
    let (hr, min, sec) = (group_number(&groups, 1), group_number(&groups, 2), group_number(&groups, 3));
    if hr > 23 || min > 59 || sec > 60 {
        return false;
    }
    let (tz_h, tz_m) = (group_number(&groups, 6), group_number(&groups, 7));
    if has(5) && (tz_h > 23 || tz_m > 59) {
        return false;
    }
    if sec < 60 {
        return true;
    }
    let sign: i64 = if groups.get(5).and_then(|g| g.as_deref()) == Some("-") { -1 } else { 1 };
    let total = i64::from(hr * 60 + min) - sign * i64::from(tz_h * 60 + tz_m);
    (total % 1440 + 1440) % 1440 == 1439
}

fn is_date_time(value: &str) -> bool {
    let parts: Vec<&str> = value.split(['T', 't']).collect();
    parts.len() == 2 && is_date(parts[0]) && is_time(parts[1])
}

// ---- idna (TypeBox format/idna) ----

fn is_ace_prefixed(value: &str) -> bool {
    value.to_lowercase().starts_with("xn--")
}

fn puny_body(value: &str) -> Option<String> {
    // The ACE prefix is ASCII, so byte offset 4 is a char boundary.
    value.get(4..).map(str::to_lowercase)
}

static RE_GREEK: LazyLock<Option<regex::Regex>> = LazyLock::new(|| regex::Regex::new(r"\p{Greek}").ok());
static RE_HEBREW: LazyLock<Option<regex::Regex>> = LazyLock::new(|| regex::Regex::new(r"\p{Hebrew}").ok());
static RE_JAPANESE: LazyLock<Option<regex::Regex>> = LazyLock::new(|| regex::Regex::new(r"[\p{Hiragana}\p{Katakana}\p{Han}]").ok());
static RE_ARABIC_LETTER: LazyLock<Option<regex::Regex>> = LazyLock::new(|| regex::Regex::new(r"[\p{Arabic}\p{Syriac}\p{Thaana}\p{Mandaic}]").ok());

fn in_script(regex: &LazyLock<Option<regex::Regex>>, c: char) -> bool {
    let mut buf = [0u8; 4];
    regex.as_ref().is_some_and(|re| re.is_match(c.encode_utf8(&mut buf)))
}

fn is_letter(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::UppercaseLetter
            | GeneralCategory::LowercaseLetter
            | GeneralCategory::TitlecaseLetter
            | GeneralCategory::ModifierLetter
            | GeneralCategory::OtherLetter
    )
}

fn is_mn(c: char) -> bool {
    get_general_category(c) == GeneralCategory::NonspacingMark
}

fn is_combining_mark(c: char) -> bool {
    matches!(get_general_category(c), GeneralCategory::NonspacingMark | GeneralCategory::SpacingMark | GeneralCategory::EnclosingMark)
}

const VIRAMA: &[u32] = &[
    0x094d, 0x09cd, 0x0a4d, 0x0acd, 0x0b4d, 0x0bcd, 0x0c4d, 0x0ccd, 0x0d3b, 0x0d3c, 0x0d4d, 0x0dca, 0x1b44, 0x1baa, 0x1bab, 0xa9c0,
    0x11046, 0x1107f, 0x110b9, 0x11133, 0x11134, 0x111c0, 0x11235, 0x1134d, 0x11442, 0x114c2, 0x115bf, 0x1163f, 0x116b6, 0x11c3f,
    0x11d44, 0x11d45,
];
const RFC5892_DISALLOWED: &[u32] = &[0x0640, 0x07fa, 0x302e, 0x302f, 0x3031, 0x3032, 0x3033, 0x3034, 0x3035, 0x303b];
const CONTEXTO_EXCEPTIONS: &[u32] = &[0x00b7, 0x0375, 0x05f3, 0x05f4, 0x200c, 0x200d, 0x30fb];
const PVALID_EXCEPTIONS: &[u32] = &[0x00df, 0x03c2, 0x06fd, 0x06fe, 0x0f0b, 0x3007];

fn is_permitted_category(c: char) -> bool {
    let cp = u32::from(c);
    is_letter(c)
        || matches!(c, '-' | '+' | '.' | ',' | ':' | '/')
        || matches!(get_general_category(c), GeneralCategory::DecimalNumber | GeneralCategory::NonspacingMark | GeneralCategory::SpacingMark)
        || CONTEXTO_EXCEPTIONS.contains(&cp)
        || PVALID_EXCEPTIONS.contains(&cp)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Bidi {
    En,
    An,
    Nsm,
    R,
    Al,
    L,
    On,
}

fn bidi_class(c: char) -> Bidi {
    if c.is_ascii_digit() || ('\u{06f0}'..='\u{06f9}').contains(&c) {
        Bidi::En
    } else if ('\u{0660}'..='\u{0669}').contains(&c) {
        Bidi::An
    } else if is_mn(c) {
        Bidi::Nsm
    } else if in_script(&RE_HEBREW, c) {
        Bidi::R
    } else if in_script(&RE_ARABIC_LETTER, c) {
        Bidi::Al
    } else if is_letter(c) {
        Bidi::L
    } else {
        Bidi::On
    }
}

fn has_rtl(value: &str) -> bool {
    value.chars().any(|c| matches!(bidi_class(c), Bidi::R | Bidi::Al | Bidi::An))
}

fn has_bidi_chars(value: &str) -> bool {
    if is_ace_prefixed(value) {
        return puny_body(value).and_then(|body| idna::punycode::decode_to_string(&body)).is_some_and(|decoded| has_rtl(&decoded));
    }
    has_rtl(value)
}

fn satisfies_bidi_rule(value: &str) -> bool {
    // RE_RTL_ALLOWED / RE_LTR_ALLOWED only ever see the classes `bidi_class` can produce.
    let (mut is_rtl, mut saw_en, mut saw_an, mut first) = (false, false, false, true);
    for c in value.chars() {
        let class = bidi_class(c);
        if first {
            if !matches!(class, Bidi::L | Bidi::R | Bidi::Al) {
                return false;
            }
            is_rtl = matches!(class, Bidi::R | Bidi::Al);
            first = false;
        }
        let allowed = if is_rtl {
            matches!(class, Bidi::R | Bidi::Al | Bidi::An | Bidi::En | Bidi::On | Bidi::Nsm)
        } else {
            matches!(class, Bidi::L | Bidi::En | Bidi::On | Bidi::Nsm)
        };
        if !allowed {
            return false;
        }
        match class {
            Bidi::En => saw_en = true,
            Bidi::An => saw_an = true,
            _ => {}
        }
    }
    !(is_rtl && saw_en && saw_an)
}

fn is_unicode_label(value: &str) -> bool {
    if !value.is_ascii() && idna::punycode::encode_str(value).is_none_or(|encoded| encoded.len() + 4 > 63) {
        return false;
    }
    if has_rtl(value) && !satisfies_bidi_rule(value) {
        return false;
    }
    let chars: Vec<char> = value.chars().collect();
    if chars.first() == Some(&'-') || chars.last() == Some(&'-') || chars.get(2..4) == Some(&['-', '-'][..]) {
        return false;
    }
    if chars.first().is_some_and(|c| is_combining_mark(*c)) {
        return false;
    }
    let mut has_japanese = false;
    for (i, &c) in chars.iter().enumerate() {
        let cp = u32::from(c);
        if RFC5892_DISALLOWED.contains(&cp) || !is_permitted_category(c) {
            return false;
        }
        if in_script(&RE_JAPANESE, c) {
            has_japanese = true;
        }
        let prev = i.checked_sub(1).and_then(|p| chars.get(p)).copied();
        let next = chars.get(i + 1).copied();
        let ok = match cp {
            0x00b7 => prev == Some('l') && next == Some('l'),
            0x0375 => next.is_some_and(|n| in_script(&RE_GREEK, n)),
            0x05f3 | 0x05f4 => prev.is_some_and(|p| in_script(&RE_HEBREW, p)),
            0x200c => prev.is_some_and(|p| u32::from(p) >= 0x80 || VIRAMA.contains(&u32::from(p))),
            0x200d => prev.is_some_and(|p| VIRAMA.contains(&u32::from(p))),
            _ => true,
        };
        if !ok {
            return false;
        }
    }
    !value.contains('\u{30fb}') || has_japanese
}

fn is_puny_label(value: &str) -> bool {
    if !is_ace_prefixed(value) {
        return false;
    }
    let Some(body) = puny_body(value) else { return false };
    if body.rfind('-') == Some(0) {
        return false;
    }
    let Some(decoded) = idna::punycode::decode_to_string(&body) else { return false };
    !decoded.is_ascii() && is_unicode_label(&decoded)
}

fn is_ascii_label(value: &str) -> bool {
    let chars: Vec<char> = value.chars().collect();
    let has_reserved_hyphen_marker = chars.len() >= 4 && chars[2] == '-' && chars[3] == '-';
    !value.starts_with('-')
        && !value.ends_with('-')
        && !has_reserved_hyphen_marker
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn valid_label_length(value: &str) -> bool {
    (1..=63).contains(&utf16_len(value))
}

fn is_hostname(value: &str) -> bool {
    let length = utf16_len(value);
    if length == 0 || length > 253 || value.ends_with('.') {
        return false;
    }
    value.split('.').all(|label| valid_label_length(label) && (is_puny_label(label) || is_ascii_label(label)))
}

fn normalize_hostname(value: &str) -> String {
    let widened: String = value
        .chars()
        .map(|c| match u32::from(c) {
            cp @ 0xff01..=0xff5e => char::from_u32(cp - 0xfee0).unwrap_or(c),
            _ => c,
        })
        .collect();
    widened
        .nfc()
        .filter(|c| !matches!(u32::from(*c), 0x00ad | 0x034f | 0x180b..=0x180d | 0x200b | 0xfe00..=0xfe0f | 0xe0100..=0xe01ef))
        .map(|c| if matches!(c, '\u{002E}' | '\u{3002}' | '\u{FF0E}' | '\u{FF61}') { '.' } else { c })
        .collect()
}

fn is_idn_hostname(value: &str) -> bool {
    if value.is_empty() || value.contains(' ') {
        return false;
    }
    let normalized = normalize_hostname(value);
    if utf16_len(&normalized) > 253 {
        return false;
    }
    let labels: Vec<&str> = normalized.split('.').collect();
    let bidi = labels.iter().any(|label| has_bidi_chars(label));
    labels
        .iter()
        .all(|label| valid_label_length(label) && (is_puny_label(label) || is_unicode_label(label)) && (!bidi || satisfies_bidi_rule(label)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registry_regex_compiles() {
        for regex in [
            &RE_DURATION, &RE_EMAIL, &RE_IDN_EMAIL, &RE_IPV4, &RE_IPV6, &RE_JSON_POINTER_URI_FRAGMENT, &RE_JSON_POINTER,
            &RE_RELATIVE_JSON_POINTER, &RE_URI_REFERENCE, &RE_URI_TEMPLATE, &RE_URI, &RE_UUID, &RE_IRI_INVALID_CHARS,
            &RE_IRI_REFERENCE_INVALID, &RE_IRI_INVALID_PERCENT, &RE_IRI_MALFORMED_SCHEME, &RE_DATE, &RE_TIME,
        ] {
            assert!(regex.is_some());
        }
    }

    #[test]
    fn matches_typebox_format_verdicts() {
        // Verdicts captured from TypeBox 1.3.34 `Compile({ type: "string", format }).Check(value)`.
        let cases = [
            ("email", "nope", false),
            ("date-time", "2024-01-01T00:00:00Z", true),
            ("date", "2024-02-30", false),
            ("uri", "http://a.b/c", true),
            ("uuid", "x", false),
            ("hostname", "-bad.com", false),
            ("ipv4", "256.1.1.1", false),
            ("time", "23:59:60Z", true),
            ("ipv6", "::1", true),
            ("regex", "((", false),
            ("json-pointer", "/a~2", false),
            ("duration", "P1Y2M", true),
            ("url", "notaurl", false),
            ("unknown-format", "anything", true),
        ];
        for (format, value, expected) in cases {
            assert_eq!(test_format(format, value), expected, "{format} {value}");
        }
    }
}
