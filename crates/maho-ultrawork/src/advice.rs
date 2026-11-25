//! A prohibition names a mode or a list of modes, not the rest of the prompt.

use regex::{Captures, Regex};
use std::{borrow::Cow, sync::LazyLock};

static ADVICE: LazyLock<Regex> = LazyLock::new(|| {
    // Include the native mass aliases: forbidding a mode should work under every name
    // that actually arms it. A comma before prose is not a list separator.
    let item = r"(?:mass[\s_-]*ulw|ulw[\s_-]*mass|mulw|meth|(?:mass[-\s]?)?ultrawork|ulw)(?:[-_](?:loop|plan|research|execute|mass))?(?-u:\b)";
    let prohibition = r"(?:avoid(?:ing)?|skip(?:ping)?|without|no|never|do\s+not|don['’]t|dont)";
    let qualifier = r"(?:(?:the|using|use\s+of)\s+)?";
    Regex::new(&format!(
        r"(?i)(?-u:\b){prohibition}\s+{qualifier}{item}(?:(?:\s*,\s*{item})*\s*,?\s*(?:and|or|nor|/)\s*{item})?"
    ))
    .expect("valid keyword advice pattern")
});

/// Blank named prohibitions without shifting UTF-8 byte offsets or joining adjacent words.
pub fn mask_keyword_advice(text: &str) -> Cow<'_, str> {
    ADVICE.replace_all(text, |caps: &Captures<'_>| {
        caps[0]
            .bytes()
            .map(|b| match b {
                b'\r' | b'\n' => char::from(b),
                _ => '\0',
            })
            .collect::<String>()
    })
}
