use maho_ext_account::{parse_command, AccountAction};

#[test]
fn provider_command_preserves_generic_pin_semantics() {
    assert_eq!(parse_command(""), None);
    assert_eq!(parse_command("provider"), Some(("provider".into(), AccountAction::List)));
    assert_eq!(parse_command("provider pin unpin"), Some(("provider".into(), AccountAction::Pin("unpin".into()))));
    assert_eq!(parse_command("provider remove"), Some(("provider".into(), AccountAction::Usage)));
    assert_eq!(parse_command("provider rename id display  name"), Some(("provider".into(), AccountAction::DisplayName("rename id display  name".into()))));
}

#[test]
fn status_lists_each_model_block_with_its_iso_expiry() {
    use maho_ext_account::{account_status, iso_timestamp};
    use maho_ext_api::{CredentialAccountBlockedModel, CredentialAccountSource, CredentialAccountSummary};
    assert_eq!(iso_timestamp(0), "1970-01-01T00:00:00.000Z");
    assert_eq!(iso_timestamp(1_709_251_199_999), "2024-02-29T23:59:59.999Z");
    assert_eq!(iso_timestamp(4_102_444_800_000), "2100-01-01T00:00:00.000Z");
    let account = CredentialAccountSummary {
        name: "work".into(),
        display_name: None,
        source: CredentialAccountSource::Login,
        blocked: false,
        pinned: true,
        blocked_models: vec![CredentialAccountBlockedModel { model: "fable".into(), until: 1_709_251_199_999 }],
        unused: false,
    };
    assert_eq!(
        account_status(&account),
        "work | login | available | blocked for fable until 2024-02-29T23:59:59.999Z | pinned"
    );
}
