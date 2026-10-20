use super::system_one_shared::{SystemOneTransport, classify_system_one};
use crate::types::{ClassifierContext, ClassifierModel, ClassifierOptions, ClassifierResult};
use serde_json::{Value, json};

pub struct CloudflareWorkersAiSystemOne;

impl SystemOneTransport for CloudflareWorkersAiSystemOne {
    fn api(&self) -> &str {
        "cloudflare-workers-ai-system-one"
    }
    fn label(&self) -> &str {
        "Cloudflare Workers AI"
    }
    fn url(&self, model: &ClassifierModel) -> String {
        format!("{}/run", model.base_url.trim_end_matches('/'))
    }
    fn payload(&self, model: &ClassifierModel, request: Value) -> Value {
        json!({"model": model.id, "input": request})
    }
    fn output(&self, body: Value) -> Result<Value, String> {
        let unexpected = || "Cloudflare Workers AI returned an unexpected response".to_owned();
        if !body.is_object() {
            return Err(unexpected());
        }
        if body["success"] == false {
            let messages: Vec<_> = body["errors"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| e["message"].as_str())
                .collect();
            return Err(if messages.is_empty() {
                "Cloudflare Workers AI request failed".into()
            } else {
                format!("Cloudflare Workers AI error: {}", messages.join("; "))
            });
        }
        let run = &body["result"];
        if !run.is_object() {
            return Err(unexpected());
        }
        if run["state"] != "Completed" {
            return Err(format!(
                "Cloudflare Workers AI run did not complete (state: {})",
                run["state"]
                    .as_str()
                    .map_or_else(|| run["state"].to_string(), str::to_owned)
            ));
        }
        if !run["result"].is_object() {
            return Err(unexpected());
        }
        Ok(run["result"].clone())
    }
}

pub async fn classify(
    model: &ClassifierModel,
    context: &ClassifierContext,
    options: Option<ClassifierOptions>,
) -> ClassifierResult {
    classify_system_one(&CloudflareWorkersAiSystemOne, model, context, options).await
}
