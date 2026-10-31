//! Port of senpi packages/ai/src/api/cloudflare-workers-ai-system-one.lazy.ts.
//!
//! senpi defers `import("./cloudflare-workers-ai-system-one.ts")` until the first classify call. Rust links the
//! module statically, so the lazy wrapper is a `ProviderClassifier` that forwards to
//! `crate::models::cloudflare_workers_ai_system_one::classify` on each call, with the same observable behaviour.

use crate::types::{
    BoxFuture, ClassifierContext, ClassifierModel, ClassifierOptions, ClassifierResult,
    ProviderClassifier,
};
use std::sync::Arc;

struct LazyClassifier;

impl ProviderClassifier for LazyClassifier {
    fn classify<'a>(
        &'a self,
        model: &'a ClassifierModel,
        context: &'a ClassifierContext,
        options: Option<ClassifierOptions>,
    ) -> BoxFuture<'a, ClassifierResult> {
        Box::pin(crate::models::cloudflare_workers_ai_system_one::classify(
            model, context, options,
        ))
    }
}

pub fn cloudflare_workers_ai_system_one_api() -> Arc<dyn ProviderClassifier> {
    Arc::new(LazyClassifier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn forwards_to_the_statically_linked_classifier() {
        let model: ClassifierModel = serde_json::from_value(serde_json::json!({
            "type": "classifier", "id": "m", "name": "m", "api": "cloudflare-workers-ai-system-one",
            "provider": "fixture", "baseUrl": "http://127.0.0.1:9", "input": ["text"],
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}, "contextWindow": 1
        }))
        .expect("model");
        let context = ClassifierContext {
            state: serde_json::Map::new(),
            questions: indexmap::IndexMap::new(),
        };
        let result = cloudflare_workers_ai_system_one_api()
            .classify(&model, &context, None)
            .await;
        assert_eq!(result.model, "m");
        assert_eq!(result.api, "cloudflare-workers-ai-system-one");
    }
}
