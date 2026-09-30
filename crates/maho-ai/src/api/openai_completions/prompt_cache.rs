//! Port of senpi packages/ai/src/api/openai-prompt-cache.ts.
//!
//! Node-private copy: `src/api/openai_prompt_cache.rs` is owned by node 12-misc.

pub const OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH: usize = 64;

pub fn clamp_openai_prompt_cache_key(key: Option<&str>) -> Option<String> {
    let key = key?;
    if key.chars().count() <= OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH {
        return Some(key.to_owned());
    }
    Some(key.chars().take(OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH).collect())
}
