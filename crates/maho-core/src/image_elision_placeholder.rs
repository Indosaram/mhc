//! Port of senpi `packages/coding-agent/src/core/image-elision-placeholder.ts`.
//!
//! `messages.rs` keeps its own copy of the placeholder writer for `elide_old_images` (it predates this
//! module and is owned elsewhere); the constant is shared, and the id reader lives only here.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub use crate::messages::IMAGE_ELISION_PLACEHOLDER;

const IMAGE_ID_LENGTH: usize = 12;

static ID_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^ \(image ids?: ([0-9a-f]{{{IMAGE_ID_LENGTH}}}(?:, [0-9a-f]{{{IMAGE_ID_LENGTH}}})*)\)$"
    ))
    .unwrap_or_else(|e| panic!("image id suffix: {e}"))
});

pub fn image_content_id(data: &str) -> String {
    hex::encode(Sha256::digest(data.as_bytes()))[..IMAGE_ID_LENGTH].to_owned()
}

fn elision_placeholder(ids: &[String]) -> Value {
    let label = if ids.len() == 1 { "id" } else { "ids" };
    json!({"type": "text", "text": format!("{IMAGE_ELISION_PLACEHOLDER} (image {label}: {})", ids.join(", "))})
}

/// The image ids a placeholder text names, or `None` when the text is not a placeholder with ids.
pub fn elided_image_ids(text: &str) -> Option<Vec<String>> {
    let rest = text.strip_prefix(IMAGE_ELISION_PLACEHOLDER)?;
    let captures = ID_SUFFIX.captures(rest)?;
    Some(
        captures
            .get(1)?
            .as_str()
            .split(", ")
            .map(str::to_owned)
            .collect(),
    )
}

/// Placeholder ids let the subscription lane's sent-stream digest match a placeholder to the image it
/// replaced (senpi#2768).
pub fn replace_elided_images(content: &[Value], is_elided: impl Fn(usize) -> bool) -> Vec<Value> {
    let mut replaced = Vec::with_capacity(content.len());
    let mut pending: Vec<String> = Vec::new();
    for (index, block) in content.iter().enumerate() {
        if block.get("type").and_then(Value::as_str) == Some("image") && is_elided(index) {
            pending.push(image_content_id(
                block
                    .get("data")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ));
            continue;
        }
        if !pending.is_empty() {
            replaced.push(elision_placeholder(&pending));
            pending.clear();
        }
        replaced.push(block.clone());
    }
    if !pending.is_empty() {
        replaced.push(elision_placeholder(&pending));
    }
    replaced
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_round_trip_their_ids() {
        let content = vec![
            json!({"type":"image","data":"first","mimeType":"image/png"}),
            json!({"type":"image","data":"second","mimeType":"image/png"}),
            json!({"type":"text","text":"between"}),
            json!({"type":"image","data":"third","mimeType":"image/png"}),
        ];
        let replaced = replace_elided_images(&content, |_| true);
        assert_eq!(replaced.len(), 3);
        let first = replaced[0]["text"].as_str().unwrap_or_default();
        assert_eq!(
            elided_image_ids(first),
            Some(vec![image_content_id("first"), image_content_id("second")])
        );
        assert_eq!(replaced[1]["text"], "between");
        assert_eq!(
            elided_image_ids(replaced[2]["text"].as_str().unwrap_or_default()),
            Some(vec![image_content_id("third")])
        );
    }

    #[test]
    fn plain_and_malformed_texts_have_no_ids() {
        assert_eq!(elided_image_ids("hello"), None);
        assert_eq!(elided_image_ids(IMAGE_ELISION_PLACEHOLDER), None);
        assert_eq!(
            elided_image_ids(&format!("{IMAGE_ELISION_PLACEHOLDER} (image id: xyz)")),
            None
        );
        assert_eq!(image_content_id("first").len(), 12);
    }

    #[test]
    fn kept_images_stay_in_place() {
        let content = vec![
            json!({"type":"image","data":"a"}),
            json!({"type":"image","data":"b"}),
        ];
        let replaced = replace_elided_images(&content, |index| index == 1);
        assert_eq!(replaced[0], content[0]);
        assert_eq!(
            elided_image_ids(replaced[1]["text"].as_str().unwrap_or_default()),
            Some(vec![image_content_id("b")])
        );
    }
}
