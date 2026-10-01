use std::collections::BTreeMap;

use maho_interactive::components::assistant_render_descriptors::{
    AssistantRenderDescriptorOptions, DescriptorKind, create_assistant_render_descriptors,
};
use maho_interactive::theme::{ColorMode, Theme};
use serde_json::Value;

fn theme() -> Theme {
    Theme::builtin("dark", ColorMode::Color256).expect("builtin theme")
}

fn fixtures() -> Vec<Value> {
    serde_json::from_str(include_str!("golden/components32-descriptors.json")).expect("pinned fixture")
}

fn kind_name(kind: DescriptorKind) -> &'static str {
    match kind {
        DescriptorKind::Spacer => "spacer",
        DescriptorKind::TextMarkdown => "text-md",
        DescriptorKind::ThinkingMarkdown => "thinking-md",
        DescriptorKind::ThinkingLabel => "thinking-label",
        DescriptorKind::ProviderNativeSummary => "provider-native-summary",
        DescriptorKind::ProviderNativeBody => "provider-native-body",
        DescriptorKind::ErrorText => "error-text",
    }
}

#[test]
fn assistant_render_descriptors_match_pinned_senpi_for_every_branch() {
    let theme = theme();
    for case in fixtures() {
        let name = case["name"].as_str().expect("name");
        let message = &case["message"];
        let options = &case["options"];
        let overrides: BTreeMap<usize, bool> = options["thinkingVisibilityOverrides"]
            .as_array()
            .expect("overrides")
            .iter()
            .map(|pair| {
                let pair = pair.as_array().expect("pair");
                (pair[0].as_u64().expect("run") as usize, pair[1].as_bool().expect("hidden"))
            })
            .collect();
        let descriptors = create_assistant_render_descriptors(
            message,
            &AssistantRenderDescriptorOptions {
                expanded: options["expanded"].as_bool().expect("expanded"),
                provider_error_owned: options["providerErrorOwned"].as_bool().expect("providerErrorOwned"),
                hidden_thinking_label: options["hiddenThinkingLabel"].as_str().expect("hiddenThinkingLabel"),
                hide_thinking_block: options["hideThinkingBlock"].as_bool().expect("hideThinkingBlock"),
                thinking_visibility_overrides: &overrides,
                has_tool_calls: options["hasToolCalls"].as_bool().expect("hasToolCalls"),
            },
            &theme,
        );
        let actual: Vec<Value> = descriptors
            .iter()
            .map(|descriptor| match descriptor.thinking_run {
                Some(run) => serde_json::json!({ "kind": kind_name(descriptor.kind), "text": descriptor.text, "thinkingRun": run }),
                None => serde_json::json!({ "kind": kind_name(descriptor.kind), "text": descriptor.text }),
            })
            .collect();
        let expected = case["descriptors"].as_array().expect("descriptors").clone();
        assert_eq!(actual, expected, "descriptors for {name}");
    }
}
