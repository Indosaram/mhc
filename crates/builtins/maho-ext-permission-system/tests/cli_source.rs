use maho_ext_permission_system::{cli::parse_permission_flag, types::{Action, Rule}};

fn rule(permission: &str, pattern: &str, action: Action) -> Rule {
    Rule { permission: permission.into(), pattern: pattern.into(), action }
}

#[test]
fn complete_flag_values_match_source_matrix() {
    // Given the ten pinned CLI flag cases, including ordered complete rules.
    let cases = [
        ("bash=allow", vec![rule("bash", "*", Action::Allow)]),
        ("bash=allow,edit=deny", vec![rule("bash", "*", Action::Allow), rule("edit", "*", Action::Deny)]),
        ("bash:git *=allow", vec![rule("bash", "git *", Action::Allow)]),
        ("*=allow", vec![rule("*", "*", Action::Allow)]),
        ("bash=allow,edit:src/*=deny,write=ask", vec![rule("bash", "*", Action::Allow), rule("edit", "src/*", Action::Deny), rule("write", "*", Action::Ask)]),
        ("bash = allow , edit = deny", vec![rule("bash", "*", Action::Allow), rule("edit", "*", Action::Deny)]),
        ("", vec![]),
        ("bash=allow,invalid,edit=deny", vec![rule("bash", "*", Action::Allow), rule("edit", "*", Action::Deny)]),
        ("bash=allow,edit=deny,write=ask", vec![rule("bash", "*", Action::Allow), rule("edit", "*", Action::Deny), rule("write", "*", Action::Ask)]),
        ("bash:rm -rf *=deny", vec![rule("bash", "rm -rf *", Action::Deny)]),
    ];
    for (input, expected) in cases {
        // When the flag is parsed.
        let actual = parse_permission_flag(input);
        // Then every field and the rule order match, not just the count.
        assert_eq!(actual, expected, "{input}");
    }
}
