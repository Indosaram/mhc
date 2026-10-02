use maho_ext_builtin_loose::gpt_account::{parse_action, AccountAction};

#[test]
fn account_dispatch_preserves_aliases_and_missing_id_behavior() {
    assert_eq!(parse_action("").expect("parse"), AccountAction::List);
    assert_eq!(parse_action("pin unpin").expect("parse"), AccountAction::Unpin);
    assert_eq!(parse_action("pin slot ignored").expect("parse"), AccountAction::Pin("slot".into()));
    assert_eq!(parse_action("remove").expect("parse"), AccountAction::Usage);
    assert_eq!(parse_action("other").expect("parse"), AccountAction::Usage);
}
