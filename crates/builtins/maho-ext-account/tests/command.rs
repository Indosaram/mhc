use maho_ext_account::{parse_command, AccountAction};

#[test]
fn provider_command_preserves_generic_pin_semantics() {
    assert_eq!(parse_command(""), None);
    assert_eq!(parse_command("provider"), Some(("provider".into(), AccountAction::List)));
    assert_eq!(parse_command("provider pin unpin"), Some(("provider".into(), AccountAction::Pin("unpin".into()))));
    assert_eq!(parse_command("provider remove"), Some(("provider".into(), AccountAction::Usage)));
    assert_eq!(parse_command("provider rename id display  name"), Some(("provider".into(), AccountAction::DisplayName("rename id display  name".into()))));
}
