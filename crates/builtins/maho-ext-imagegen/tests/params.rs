use maho_ext_imagegen::params::{
    DEFAULT_IMAGE_MODEL, IMAGE_MODEL_FALLBACKS, IMAGE_MODEL_NAMES, fallback_model,
    model_display_name, parameters, supports_complexity,
};
use serde_json::json;

#[test]
fn schema_keeps_source_defaults_required_fields_and_limits() {
    let schema = parameters();
    assert_eq!(schema["required"], json!(["prompt"]));
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        schema["properties"]["model"]["default"],
        DEFAULT_IMAGE_MODEL
    );
    assert_eq!(schema["properties"]["prompt"]["maxLength"], 32000);
    assert_eq!(schema["properties"]["n"]["maximum"], 10);
    assert_eq!(schema["properties"]["output_compression"]["maximum"], 100);
    assert_eq!(schema["properties"]["reference_image_paths"]["maxItems"], 5);
    assert_eq!(
        schema["properties"]["complexity"]["anyOf"][0]["const"],
        json!("low")
    );
}

#[test]
fn gpt_image_26_is_the_default_and_the_2_5_pair_stays_selectable() {
    assert_eq!(DEFAULT_IMAGE_MODEL, "gpt-image-2.6-sunburst");
    assert_eq!(
        IMAGE_MODEL_NAMES,
        &[
            ("gpt-image-2.6-sunburst", "GPT Image 2.6 Sunburst"),
            ("gpt-image-2.6-flare", "GPT Image 2.6 Flare"),
            ("gpt-image-2.5-sunburst", "GPT Image 2.5 Sunburst"),
            ("gpt-image-2.5-flare", "GPT Image 2.5 Flare"),
            ("gpt-image-2", "GPT Image 2"),
        ]
    );
    let offered = parameters()["properties"]["model"]["anyOf"]
        .as_array()
        .expect("anyOf")
        .iter()
        .map(|entry| entry["const"].as_str().expect("const").to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        offered,
        IMAGE_MODEL_NAMES
            .iter()
            .map(|(id, _)| (*id).to_owned())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        model_display_name("gpt-image-2.6-flare"),
        Some("GPT Image 2.6 Flare")
    );
    assert_eq!(model_display_name("gpt-image-1"), None);
}

#[test]
fn each_2_6_model_falls_back_to_its_2_5_counterpart_and_only_2_6_takes_complexity() {
    assert_eq!(
        IMAGE_MODEL_FALLBACKS,
        &[
            ("gpt-image-2.6-sunburst", "gpt-image-2.5-sunburst"),
            ("gpt-image-2.6-flare", "gpt-image-2.5-flare"),
        ]
    );
    assert_eq!(
        fallback_model("gpt-image-2.6-sunburst"),
        Some("gpt-image-2.5-sunburst")
    );
    assert_eq!(fallback_model("gpt-image-2.5-sunburst"), None);
    assert!(supports_complexity("gpt-image-2.6-sunburst"));
    assert!(supports_complexity("gpt-image-2.6-flare"));
    for model in [
        "gpt-image-2.5-sunburst",
        "gpt-image-2.5-flare",
        "gpt-image-2",
    ] {
        assert!(!supports_complexity(model), "{model}");
    }
}
