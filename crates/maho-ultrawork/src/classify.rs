//! Pure prompt classification: counts, routing, lifecycle stage and suppression reason.

use crate::keywords::{Scan, mention_spans, scan};
use crate::mask::mask;
use crate::state::SessionArming;
use crate::texts::DIRECTIVE_OPEN_TAG;
use maho_ext_api::InputSource;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// First arming, or a re-arm because the contract revision changed.
    Initial,
    /// Already armed: only a short notice goes out.
    Reminder,
    /// First mention after a compaction: the full contract again.
    CompactRearm,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Direct,
    SkillExpansion,
    SkillArguments,
    Embedded,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suppression {
    Disabled,
    ChildSession,
    ExtensionSource,
    NoVisibleKeyword,
    CompoundIdentifier,
    ProhibitivePhrasing,
    EmbeddedDirective,
    HandledBySkill,
    SkillWithoutKeyword,
}

/// Everything [`classify`] looks at.
#[derive(Debug, Clone, Copy)]
pub struct ClassifyInput<'a> {
    pub text: &'a str,
    pub source: InputSource,
    pub snapshot: SessionArming,
    pub disabled: bool,
}

/// Result of classifying one prompt. Counts and flags never depend on the arming mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Classification {
    pub short_form_seen: bool,
    pub long_form_seen: bool,
    pub visible_mentions: usize,
    pub effective: bool,
    pub stage: Stage,
    pub route: Route,
    pub suppression: Option<Suppression>,
    pub mass: bool,
    /// The prompt already carries the contract (embedded wrapper or direct skill load).
    pub already_delivered: bool,
}

impl Classification {
    fn suppressed(scan: Scan, route: Route, reason: Suppression) -> Self {
        Self {
            short_form_seen: scan.short_form,
            long_form_seen: scan.long_form,
            visible_mentions: scan.mentions,
            effective: false,
            stage: Stage::None,
            route,
            suppression: Some(reason),
            mass: false,
            already_delivered: false,
        }
    }
}

fn stage_for(snapshot: SessionArming) -> Stage {
    let current = crate::contract_revision();
    match (snapshot.armed, snapshot.compaction_pending) {
        (true, _) if snapshot.revision != Some(current) => Stage::Initial,
        (true, _) => Stage::Reminder,
        (false, true) => Stage::CompactRearm,
        (false, false) => Stage::Initial,
    }
}

fn skill_command(text: &str) -> Option<(&str, &str)> {
    let rest = text.trim_start().strip_prefix("/skill:")?;
    let split = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some((&rest[..split], &rest[split..]))
}

/// Fast path for callers: does any visible keyword exist in `text`?
pub fn has_visible_keyword(text: &str) -> bool {
    scan(&mask(text, DIRECTIVE_OPEN_TAG).text).mentions > 0
}

/// A keyword mention the engine arms on, as byte offsets into the original text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeywordSpan {
    pub start: usize,
    /// End exclusive.
    pub end: usize,
    /// The mention is a mass alias (`mass ulw`, `mulw`, `meth`, ...).
    pub mass: bool,
}

/// Every mention [`has_visible_keyword`] counts, in text order. Masking keeps byte offsets, so the
/// spans index `text`. The editor paints exactly these, so painting and arming share one rule.
pub fn visible_keyword_spans(text: &str) -> Vec<KeywordSpan> {
    mention_spans(&mask(text, DIRECTIVE_OPEN_TAG).text)
        .into_iter()
        .map(|(start, end, mass)| KeywordSpan { start, end, mass })
        .collect()
}

pub fn classify(input: ClassifyInput<'_>) -> Classification {
    classify_with_session_role(input, None)
}

/// Known delegated roles suppress arming, including explicit skill expansion.
pub fn classify_with_session_role(
    input: ClassifyInput<'_>,
    session_role: Option<&str>,
) -> Classification {
    if input.disabled {
        return Classification::suppressed(Scan::default(), Route::None, Suppression::Disabled);
    }
    if session_role.is_some_and(|role| !role.is_empty()) {
        return Classification::suppressed(Scan::default(), Route::None, Suppression::ChildSession);
    }
    let (body, skill_route) = match skill_command(input.text) {
        Some(("ultrawork", args)) => {
            let found = scan(&mask(args, DIRECTIVE_OPEN_TAG).text);
            let mut out = Classification::suppressed(
                found,
                Route::SkillExpansion,
                Suppression::HandledBySkill,
            );
            out.already_delivered = true;
            return out;
        }
        Some((_, args)) => (args, Route::SkillArguments),
        None => (input.text, Route::Direct),
    };
    let masked = mask(body, DIRECTIVE_OPEN_TAG);
    let found = scan(&masked.text);
    if input.source == InputSource::Extension {
        return Classification::suppressed(found, Route::None, Suppression::ExtensionSource);
    }
    if masked.embedded_directive {
        let mut out =
            Classification::suppressed(found, Route::Embedded, Suppression::EmbeddedDirective);
        out.already_delivered = true;
        return out;
    }
    if found.mentions == 0 {
        let reason = if skill_route == Route::SkillArguments {
            Suppression::SkillWithoutKeyword
        } else if found.compound_rejected || scan(&masked.before_prohibitive).compound_rejected {
            Suppression::CompoundIdentifier
        } else if scan(&masked.before_prohibitive).mentions > 0 {
            Suppression::ProhibitivePhrasing
        } else {
            Suppression::NoVisibleKeyword
        };
        let route = if skill_route == Route::SkillArguments {
            Route::None
        } else {
            skill_route
        };
        return Classification::suppressed(found, route, reason);
    }
    Classification {
        short_form_seen: found.short_form,
        long_form_seen: found.long_form,
        visible_mentions: found.mentions,
        effective: true,
        stage: stage_for(input.snapshot),
        route: skill_route,
        suppression: None,
        mass: found.mass,
        already_delivered: false,
    }
}
