//! Blocker classification and occurrence calculation.

use crate::domain_types::{UlwLoopItem, UlwLoopPlan};

pub fn normalize_blocker_evidence(evidence: &str) -> String {
    let lower = evidence.to_ascii_lowercase();
    // remove URLs
    let mut words = Vec::new();
    for word in lower.split_whitespace() {
        if !word.starts_with("http://") && !word.starts_with("https://") {
            // clean punctuation: ` ' " ( ) [ ] { } : , ;
            let cleaned: String = word
                .chars()
                .map(|c| match c {
                    '`' | '\'' | '"' | '(' | ')' | '[' | ']' | '{' | '}' | ':' | ',' | ';' => ' ',
                    other => other,
                })
                .collect();
            for sub in cleaned.split_whitespace() {
                words.push(sub.to_string());
            }
        }
    }
    words.join(" ")
}

pub fn classify_external_authorization_blocker(evidence: &str) -> Option<String> {
    let normalized = normalize_blocker_evidence(evidence);
    if normalized.is_empty() {
        return None;
    }
    let words: Vec<&str> = normalized.split_whitespace().collect();

    let has_auth = words.iter().any(|w| {
        w.starts_with("auth")
            || w.starts_with("credential")
            || *w == "token"
            || w.starts_with("permission")
            || w.starts_with("scope")
            || *w == "access"
            || *w == "unauthorized"
            || *w == "forbidden"
            || *w == "401"
            || *w == "403"
    });
    if !has_auth {
        return None;
    }

    let has_missing = words.iter().any(|w| {
        *w == "unset"
            || *w == "missing"
            || *w == "required"
            || *w == "requires"
            || *w == "without"
            || w.starts_with("omit")
            || *w == "not set"
            || *w == "not available"
            || *w == "package"
            || *w == "packages"
    }) || normalized.contains("not set")
        || normalized.contains("not available")
        || normalized.contains("read package")
        || normalized.contains("read packages");

    if !has_missing {
        return None;
    }

    let has_ghcr = words.iter().any(|w| {
        *w == "ghcr"
            || w.starts_with("ghcr")
            || w.starts_with("ghcr.")
            || w.starts_with("ghcr/")
            || *w == "imagepullsecret"
            || *w == "anonymous"
    }) || normalized.contains("github container registry")
        || normalized.contains("read packages")
        || normalized.contains("package api")
        || normalized.contains("container image");

    if !has_ghcr {
        return Some("EXTERNAL_AUTHORIZATION_REQUIRED".to_string());
    }

    let status_401 = words.iter().any(|w| *w == "401" || *w == "unauthorized")
        || normalized.contains("anonymous pull")
        || normalized.contains("authentication required");

    let status_403 = words.iter().any(|w| *w == "403" || *w == "forbidden")
        || normalized.contains("read packages")
        || normalized.contains("package api");

    let status_part = if status_401 && status_403 {
        "HTTP_401_ANONYMOUS+HTTP_403_NO_READ_PACKAGES"
    } else if status_401 {
        "HTTP_401_ANONYMOUS"
    } else if status_403 {
        "HTTP_403_NO_READ_PACKAGES"
    } else {
        "AUTHORIZATION_REQUIRED"
    };

    Some(format!("GHCR_PULL_ACCESS:{status_part}:GHCR_VISIBILITY_OR_CREDENTIAL_REQUIRED"))
}

pub fn same_blocker_occurrences(plan: &UlwLoopPlan, signature: &str) -> usize {
    plan.goals
        .iter()
        .filter(|goal| {
            goal.blocker_signature.as_deref() == Some(signature)
        })
        .count()
}
