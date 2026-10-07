use super::*;
use crate::tools::memory::MemoryToolParams;

fn params() -> MemoryToolParams {
    MemoryToolParams {
        command: Some("create".to_string()),
        reason: "create block".to_string(),
        file_path: Some("notes/x.md".to_string()),
        ..Default::default()
    }
}

#[test]
fn splits_a_sibling_argument_leaked_into_description() {
    let mut input = params();
    input.description =
        Some("<value></description>\n<parameter name=\"file_text\"><the body>".to_string());

    let repaired = repair_leaked_arguments(input);

    assert_eq!(repaired.repairs.len(), 1);
    assert_eq!(repaired.repairs[0].from, "description");
    assert_eq!(repaired.repairs[0].to, "file_text");
    assert_eq!(repaired.params.description.as_deref(), Some("<value>"));
    assert_eq!(repaired.params.file_text.as_deref(), Some("<the body>"));
    assert_eq!(repaired.params.reason, "create block");
}

#[test]
fn leaves_params_untouched_when_the_target_was_already_supplied() {
    let mut input = params();
    input.description = Some("<value></description>\n<parameter name=\"file_text\"><leak>".to_string());
    input.file_text = Some("declared body".to_string());

    let repaired = repair_leaked_arguments(input);

    assert!(repaired.repairs.is_empty());
    assert_eq!(
        repaired.params.description.as_deref(),
        Some("<value></description>\n<parameter name=\"file_text\"><leak>")
    );
    assert_eq!(repaired.params.file_text.as_deref(), Some("declared body"));
}

#[test]
fn ignores_a_closing_tag_that_names_an_unknown_or_self_argument() {
    let mut input = params();
    input.description = Some("<value></description>\n<parameter name=\"unknown_arg\"><leak>".to_string());
    let unknown = repair_leaked_arguments(input);
    assert!(unknown.repairs.is_empty());

    let mut self_named = params();
    self_named.description = Some("<value></description>\n<parameter name=\"description\"><leak>".to_string());
    let same = repair_leaked_arguments(self_named);
    assert!(same.repairs.is_empty());
}

#[test]
fn a_clean_call_reports_no_repairs() {
    let repaired = repair_leaked_arguments(params());
    assert!(repaired.repairs.is_empty());
    assert_eq!(repaired.params.file_path.as_deref(), Some("notes/x.md"));
    assert!(describe_repairs(&repaired.repairs).is_empty());
}

#[test]
fn describe_repairs_names_both_arguments() {
    let repairs = vec![LeakedArgumentRepair {
        from: "description".to_string(),
        to: "file_text".to_string(),
    }];
    let note = describe_repairs(&repairs);
    assert!(note.contains("'file_text' arrived inside 'description'"));
    assert!(note.contains("</description>"));
}
