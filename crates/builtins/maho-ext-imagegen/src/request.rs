//! Port of senpi imagegen/request.ts.
use crate::auth::{ImageGenCandidate, ImageGenCandidates};
use maho_ai::types::{
    AssistantImages, ImagesStopReason, ProviderDiagnosticCategory, ProviderDiagnosticEvidence,
};
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

/// Rejections a model retry must never mask: billing, auth, limits, moderation and transport.
const NON_RETRYABLE_CODES: &[&str] = &[
    "authentication_error",
    "invalid_api_key",
    "permission_error",
    "billing_error",
    "insufficient_quota",
    "usage_limit_reached",
    "usage_not_included",
    "rate_limit_error",
    "rate_limit_exceeded",
    "content_policy_violation",
    "moderation_blocked",
    "content_filter",
    "safety_violation",
];

const UNSERVED_MODEL_CODES: &[&str] = &[
    "model_not_found",
    "model_not_supported",
    "model_unsupported",
    "unsupported_model",
    "invalid_model",
    "unknown_model",
];

fn mentions_content_policy(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "moderation",
        "content policy",
        "content_policy",
        "safety system",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

fn is_rejected_model_request(diagnostic: &maho_ai::types::ProviderDiagnostic) -> bool {
    diagnostic.evidence != ProviderDiagnosticEvidence::StructuredTransport
        && matches!(
            diagnostic.category,
            ProviderDiagnosticCategory::InvalidRequest | ProviderDiagnosticCategory::Unknown
        )
        && diagnostic
            .http_status
            .is_none_or(|status| matches!(status, 400 | 404 | 422))
}

/// The matching 2.5 id when a 2.6 request failed because the route does not serve that 2.6 id; auth,
/// billing, limit, transport, transient and moderation failures keep their single attempt.
pub fn unsupported_model_fallback(
    images: &AssistantImages,
    requested_model: &str,
) -> Option<&'static str> {
    if images.stop_reason != ImagesStopReason::Error {
        return None;
    }
    let fallback = crate::params::fallback_model(requested_model)?;
    let message = images.error_message.as_deref().unwrap_or_default();
    if mentions_content_policy(message) {
        return None;
    }
    let named_code = images
        .provider_diagnostic
        .as_ref()
        .and_then(|diagnostic| diagnostic.code.as_deref());
    if named_code.is_some_and(|code| NON_RETRYABLE_CODES.contains(&code)) {
        return None;
    }
    if let Some(diagnostic) = images.provider_diagnostic.as_ref()
        && !is_rejected_model_request(diagnostic)
    {
        return None;
    }
    let names_unserved_model = named_code.is_some_and(|code| UNSERVED_MODEL_CODES.contains(&code))
        || maho_ai::is_model_config_error(Some(message));
    names_unserved_model.then_some(fallback)
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
