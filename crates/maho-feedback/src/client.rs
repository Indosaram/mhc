//! Client-supplied additions to a draft (spec 4.3, RPC only): identity values a front end knows
//! about the user, and extra `logs` / `crash` text.

use crate::model::SectionId;
use crate::reports::Redacted;
use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

/// Longest kept display or device name, in UTF-16 units.
pub const NAME_MAX_UTF16: usize = 120;

fn pattern(cell: &'static OnceLock<Option<Regex>>, source: &str) -> Option<&'static Regex> {
    cell.get_or_init(|| Regex::new(source).ok()).as_ref()
}

fn id_pattern() -> Option<&'static Regex> {
    static CELL: OnceLock<Option<Regex>> = OnceLock::new();
    pattern(&CELL, r"^[A-Za-z0-9_.:@-]{1,128}$")
}

fn email_pattern() -> Option<&'static Regex> {
    static CELL: OnceLock<Option<Regex>> = OnceLock::new();
    pattern(&CELL, r"^[^\s@]{1,64}@[^\s@]{1,190}$")
}

/// Whether `value` is a well-formed identifier (account, user, plan, device id).
pub fn valid_id(value: &str) -> bool {
    id_pattern().is_some_and(|re| re.is_match(value))
}

/// Whether `value` is a well-formed e-mail address.
pub fn valid_email(value: &str) -> bool {
    email_pattern().is_some_and(|re| re.is_match(value))
}

/// Control-character runs collapsed to one space, trimmed; `None` when empty or longer than
/// [`NAME_MAX_UTF16`] UTF-16 units.
pub fn normalize_name(value: &str) -> Option<String> {
    let mut out = String::with_capacity(value.len());
    let mut in_run = false;
    for c in value.chars() {
        if c.is_control() {
            if !in_run {
                out.push(' ');
            }
            in_run = true;
        } else {
            out.push(c);
            in_run = false;
        }
    }
    let trimmed = out.trim();
    (!trimmed.is_empty() && trimmed.encode_utf16().count() <= NAME_MAX_UTF16)
        .then(|| trimmed.to_owned())
}

/// Identity values a front end supplied, after validation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientIdentity {
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub plan: Option<String>,
    pub device_id: Option<String>,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub device_name: Option<String>,
}

impl ClientIdentity {
    /// From the RPC `identity` object. A value is kept only when it is well formed and the upload
    /// redactor finds nothing in it; `redact` returns `None` when no redactor is available.
    pub fn from_json(value: Option<&Value>, redact: &dyn Fn(&str) -> Option<Redacted>) -> Self {
        let Some(Value::Object(object)) = value else {
            return Self::default();
        };
        let clean = |text: &str| redact(text).is_some_and(|out| out.count == 0 && out.text == text);
        let string = |key: &str| object.get(key).and_then(Value::as_str);
        let id = |key: &str| {
            string(key)
                .filter(|text| valid_id(text) && clean(*text))
                .map(str::to_owned)
        };
        let name = |key: &str| {
            string(key)
                .and_then(normalize_name)
                .filter(|text| clean(text.as_str()))
        };
        let email = string("email")
            .filter(|text| valid_email(text))
            .filter(|text| {
                text.rsplit_once('@')
                    .is_some_and(|(local, domain)| clean(local) && clean(domain))
            })
            .map(str::to_owned);
        Self {
            account_id: id("accountId"),
            user_id: id("userId"),
            plan: id("plan"),
            device_id: id("deviceId"),
            email,
            display_name: name("displayName"),
            device_name: name("deviceName"),
        }
    }

    /// `(label, value)` pairs of the kept values, in a fixed order.
    pub fn labelled(&self) -> Vec<(&'static str, String)> {
        [
            ("Account", &self.account_id),
            ("User", &self.user_id),
            ("Plan", &self.plan),
            ("Device ID", &self.device_id),
            ("Email", &self.email),
            ("Display name", &self.display_name),
            ("Device name", &self.device_name),
        ]
        .into_iter()
        .filter_map(|(label, value)| value.clone().map(|value| (label, value)))
        .collect()
    }
}

/// The accepted client sections (`logs`, `crash` with string text), in input order.
pub fn client_sections(value: Option<&Value>) -> Vec<(SectionId, String)> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let id = SectionId::parse(item.get("id")?.as_str()?)?;
            let text = item.get("text")?.as_str()?;
            matches!(id, SectionId::Logs | SectionId::Crash).then(|| (id, text.to_owned()))
        })
        .collect()
}
