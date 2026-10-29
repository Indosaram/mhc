//! `team/member-extensions.test.ts`

use pretty_assertions::assert_eq;

use crate::team::member_extensions::assemble_member_extensions;

#[test]
fn given_duplicated_and_ordered_inherited_extensions_when_assembled_then_member_stays_first_and_inherited_order_is_stable() {
    let assembled = assemble_member_extensions(
        "/member.js",
        &["/provider-a.js", "/member.js", "/provider-b.js", "/provider-a.js"],
    );

    assert_eq!(
        assembled,
        vec![
            "/member.js".to_string(),
            "/provider-a.js".to_string(),
            "/provider-b.js".to_string(),
        ]
    );
}
