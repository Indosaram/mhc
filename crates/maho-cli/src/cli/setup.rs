//! Native equivalent of the API registration triggered by the upstream CLI imports.
pub fn register_builtin_apis() {
    use maho_ai::{api, api_registry::register_builtin_api_provider as register};
    register("anthropic-messages", api::anthropic_messages_lazy::anthropic_messages_api());
    register("azure-openai-responses", api::azure_openai_responses_lazy::azure_openai_responses_api());
    register("bedrock-converse-stream", api::bedrock_converse_stream_lazy::bedrock_converse_stream_api());
    register("cursor-agent", std::sync::Arc::new(api::cursor_agent_lazy::cursor_agent_api()));
    register("devin-agent", api::devin_agent_lazy::devin_agent_api());
    register("google-generative-ai", api::google_generative_ai_lazy::google_generative_ai_api());
    register("google-vertex", api::google_vertex_lazy::google_vertex_api());
    register("mistral-conversations", api::mistral_conversations_lazy::mistral_conversations_api());
    register("openai-codex-responses", api::openai_codex_responses_lazy::openai_codex_responses_api());
    register("openai-completions", api::openai_completions_lazy::open_ai_completions_api());
    register("openai-responses", api::openai_responses_lazy::openai_responses_api());
    register("pi-messages", api::pi_messages_lazy::pi_messages_api());
}
