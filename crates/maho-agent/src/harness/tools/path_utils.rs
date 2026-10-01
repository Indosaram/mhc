use crate::harness::{context::Context, types::ExecutionEnv};
use unicode_normalization::UnicodeNormalization;
fn normalize_tool_path(path: &str) -> String {
    let normalized: String = path
        .chars()
        .map(|c| match c {
            '\u{a0}' | '\u{2000}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}' => ' ',
            _ => c,
        })
        .collect();
    normalized
        .strip_prefix('@')
        .unwrap_or(&normalized)
        .to_owned()
}
pub async fn resolve_tool_path(
    env: &dyn ExecutionEnv,
    path: &str,
    context: &Context,
) -> Result<String, String> {
    env.absolute_path(&normalize_tool_path(path), context)
        .await
        .map_err(|e| e.to_string())
}
pub async fn resolve_read_tool_path(
    env: &dyn ExecutionEnv,
    path: &str,
    context: &Context,
) -> Result<String, String> {
    let resolved = resolve_tool_path(env, path, context).await?;
    let mut time_variant = String::new();
    let mut chars = resolved.chars().peekable();
    while let Some(c) = chars.next() {
        let next: String = chars.clone().take(3).collect();
        time_variant.push(
            if c == ' ' && (next.eq_ignore_ascii_case("AM.") || next.eq_ignore_ascii_case("PM.")) {
                '\u{202f}'
            } else {
                c
            },
        );
    }
    let nfd: String = resolved.nfd().collect();
    let variants = [
        resolved.clone(),
        time_variant,
        nfd.clone(),
        resolved.replace('\'', "\u{2019}"),
        nfd.replace('\'', "\u{2019}"),
    ];
    let mut seen = std::collections::HashSet::new();
    for variant in variants {
        if seen.insert(variant.clone())
            && env
                .exists(&variant, context)
                .await
                .map_err(|e| e.to_string())?
        {
            return Ok(variant);
        }
    }
    Ok(resolved)
}
