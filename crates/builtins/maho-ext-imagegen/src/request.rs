//! Port of senpi imagegen/request.ts.
use crate::auth::{ImageGenCandidate, ImageGenCandidates};
use maho_ai::types::{AssistantImages, ImagesStopReason};
use std::future::Future;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedImageProvider {
    pub provider_id: String,
    pub base_url: String,
    pub http_status: u16,
}
#[derive(Debug)]
pub enum ImageRequestOutcome {
    Images {
        auth: Box<ImageGenCandidate>,
        images: Box<AssistantImages>,
        skipped: Vec<SkippedImageProvider>,
    },
    Exhausted {
        skipped: Vec<SkippedImageProvider>,
    },
}

fn missing_images_endpoint_status(images: &AssistantImages) -> Option<u16> {
    if images.stop_reason != ImagesStopReason::Error {
        return None;
    }
    images
        .provider_diagnostic
        .as_ref()?
        .http_status
        .filter(|status| matches!(status, 404 | 405))
}

/// Sends the request to `first`, then moves to the next candidate only while the current one is a
/// scanned gateway whose server reports no images endpoint (HTTP 404/405, read from the structured
/// provider diagnostic, never from message text). Any other result is final.
pub async fn request_images_across_candidates<E, F, Fut>(
    first: ImageGenCandidate,
    rest: &mut ImageGenCandidates<'_>,
    mut request: F,
) -> Result<ImageRequestOutcome, E>
where
    F: FnMut(&ImageGenCandidate) -> Fut,
    Fut: Future<Output = Result<AssistantImages, E>>,
{
    let mut skipped = Vec::new();
    let mut auth = first;
    loop {
        let images = request(&auth).await?;
        let status = if auth.fallthrough {
            missing_images_endpoint_status(&images)
        } else {
            None
        };
        let Some(status) = status else {
            return Ok(ImageRequestOutcome::Images {
                auth: Box::new(auth),
                images: Box::new(images),
                skipped,
            });
        };
        skipped.push(SkippedImageProvider {
            provider_id: auth
                .resolved
                .provider_id
                .clone()
                .unwrap_or_else(|| "openai".into()),
            base_url: auth.resolved.base_url.clone(),
            http_status: status,
        });
        let Some(next) = rest.next().await else {
            return Ok(ImageRequestOutcome::Exhausted { skipped });
        };
        auth = next;
    }
}

fn describe(skipped: &[SkippedImageProvider]) -> String {
    skipped
        .iter()
        .map(|entry| {
            format!(
                "{} ({}, HTTP {})",
                entry.provider_id, entry.base_url, entry.http_status
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn exhausted_providers_message(skipped: &[SkippedImageProvider]) -> String {
    [
        format!("Error: none of the configured OpenAI-compatible providers serves image generation. Tried: {}.", describe(skipped)),
        "Set PI_IMAGE_GEN_PROVIDER to a provider whose endpoint has /images, override the \"openai\" provider in models.json with such a baseUrl, or store an OpenAI API key for provider \"openai\".".to_owned(),
    ].join(" ")
}

pub fn skipped_providers_note(skipped: &[SkippedImageProvider]) -> Option<String> {
    if skipped.is_empty() {
        return None;
    }
    Some(format!(
        "Note: skipped {}: no image generation endpoint. Pin a provider with PI_IMAGE_GEN_PROVIDER to skip this check.",
        describe(skipped)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn skipped() -> Vec<SkippedImageProvider> {
        vec![
            SkippedImageProvider {
                provider_id: "gw".into(),
                base_url: "https://gw/v1".into(),
                http_status: 404,
            },
            SkippedImageProvider {
                provider_id: "g2".into(),
                base_url: "https://g2/v1".into(),
                http_status: 405,
            },
        ]
    }
    #[test]
    fn messages_list_every_skipped_provider() {
        assert_eq!(
            skipped_providers_note(&skipped()).as_deref(),
            Some(
                "Note: skipped gw (https://gw/v1, HTTP 404), g2 (https://g2/v1, HTTP 405): no image generation endpoint. Pin a provider with PI_IMAGE_GEN_PROVIDER to skip this check."
            )
        );
        assert!(exhausted_providers_message(&skipped()).starts_with("Error: none of the configured OpenAI-compatible providers serves image generation. Tried: gw (https://gw/v1, HTTP 404), g2 (https://g2/v1, HTTP 405). Set PI_IMAGE_GEN_PROVIDER"));
        assert_eq!(skipped_providers_note(&[]), None);
    }
}
