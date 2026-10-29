use model_core::ParsedModelString;
use model_core::parse_model_string;
use pretty_assertions::assert_eq;

fn parsed(provider_id: &str, model_id: &str, variant: Option<&str>) -> Option<ParsedModelString> {
    Some(ParsedModelString {
        provider_id: provider_id.to_string(),
        model_id: model_id.to_string(),
        variant: variant.map(str::to_string),
    })
}

#[test]
fn provider_prefixed_max_suffix_extracts_the_level() {
    assert_eq!(
        parse_model_string("openai/gpt-5.6-sol:max"),
        parsed("openai", "gpt-5.6-sol", Some("max"))
    );
}

#[test]
fn provider_prefixed_high_suffix_extracts_the_level() {
    assert_eq!(
        parse_model_string("anthropic/claude-fable-5:high"),
        parsed("anthropic", "claude-fable-5", Some("high"))
    );
}

#[test]
fn non_level_colon_suffix_stays_part_of_the_model_id() {
    assert_eq!(
        parse_model_string("openrouter/openai/gpt:free"),
        parsed("openrouter", "openai/gpt:free", None)
    );
}

#[test]
fn legacy_paren_and_space_syntaxes_still_extract_the_variant() {
    assert_eq!(
        parse_model_string("anthropic/claude-opus-4-8(xhigh)"),
        parsed("anthropic", "claude-opus-4-8", Some("xhigh"))
    );
    assert_eq!(
        parse_model_string("openai/gpt-5.6-sol high"),
        parsed("openai", "gpt-5.6-sol", Some("high"))
    );
}
