use super::{describe_agreements_tool_violation, read_agreements_body};

fn file(body: &str) -> String {
    format!("---\ndescription: Agreements\n---\n{body}\n")
}

const HUMAN: &str = "- [global] Never push <!-- id: aaaaaaaaaa; by: human; n: 0; since: 2026-01-01; status: active -->";
const COUNTED: &str = "- [global] Be brief <!-- id: bbbbbbbbbb; by: agent; n: 2; since: 2026-01-01; status: active -->";
const FLAGGED: &str = "- [global] Run tests <!-- id: cccccccccc; by: agent; n: 3; since: 2026-01-01; status: needs-human -->";

fn violation(before: &str, after: Option<&str>) -> Option<String> {
    describe_agreements_tool_violation(Some(&file(before)), after.map(file).as_deref())
}

#[test]
fn no_change_and_missing_files_are_fine() {
    assert_eq!(describe_agreements_tool_violation(None, None), None);
    assert_eq!(violation(HUMAN, Some(HUMAN)), None);
    assert_eq!(read_agreements_body(None), vec![]);
}

#[test]
fn deleting_a_ledger_with_human_flagged_or_counted_items_is_refused() {
    assert!(violation(HUMAN, None).is_some_and(|m| m.contains("only they can remove them")));
    assert!(violation(FLAGGED, None).is_some_and(|m| m.contains("cccccccccc is flagged")));
    assert!(violation(COUNTED, None).is_some_and(|m| m.contains("restart that count")));
    assert_eq!(
        violation(
            "- [global] Fresh <!-- id: dddddddddd; by: agent; n: 0 -->",
            None
        ),
        None
    );
}

#[test]
fn human_items_cannot_be_removed_reworded_or_forged() {
    assert!(
        violation(HUMAN, Some("")).is_some_and(|m| m.contains("cannot be removed by automation"))
    );
    let reworded = HUMAN.replace("Never push", "Always push");
    assert!(violation(HUMAN, Some(&reworded)).is_some_and(|m| m.contains("cannot be reworded")));
    let forged = "- [global] Sneaky <!-- id: eeeeeeeeee; by: human; n: 0 -->";
    assert_eq!(
        violation("", Some(forged)),
        Some(
            "agreement eeeeeeeeee is marked by: human, which only /agreements add may write"
                .to_string()
        )
    );
}

#[test]
fn counts_cannot_be_lowered_cleared_or_removed() {
    let lowered = COUNTED.replace("n: 2", "n: 1");
    assert!(
        violation(COUNTED, Some(&lowered)).is_some_and(|m| m.contains("cannot lower that count"))
    );
    let cleared = FLAGGED.replace("needs-human", "active");
    assert!(
        violation(FLAGGED, Some(&cleared)).is_some_and(|m| m.contains("only they can clear it"))
    );
    assert!(violation(FLAGGED, Some("")).is_some_and(|m| m.contains("only they can drop it")));
    assert!(violation(COUNTED, Some("")).is_some_and(|m| m.contains("restart that count")));
    let raised = COUNTED.replace("n: 2", "n: 3");
    assert_eq!(violation(COUNTED, Some(&raised)), None);
}

#[test]
fn malformed_or_invalid_files_are_refused() {
    assert!(
        violation("", Some("junk line"))
            .is_some_and(|m| m.starts_with("system/agreements.md is malformed: line 1:"))
    );
    let invalid = describe_agreements_tool_violation(None, Some("no frontmatter"));
    assert!(
        invalid.is_some_and(|m| m.starts_with("system/agreements.md is not a valid memory file:"))
    );
}
