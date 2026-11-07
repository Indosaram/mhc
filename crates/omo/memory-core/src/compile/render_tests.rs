use pretty_assertions::assert_eq;

use super::{
    CompiledSystemFile, mark_memory_block, render_system_tree, replace_memory_block,
    strip_memory_block,
};

#[test]
fn given_an_identity_and_block_when_marked_then_exact_sentinel_wrapper_is_returned() {
    let marked = mark_memory_block("agent-1", "compiled");
    assert_eq!(
        marked,
        "<!-- senpi-memory:agent-1:begin -->\ncompiled\n<!-- senpi-memory:agent-1:end -->"
    );
}

#[test]
fn given_repeated_legacy_blocks_when_replaced_then_every_matching_region_is_transformed() {
    let old = mark_memory_block("agent-1", "old");
    let replacement = mark_memory_block("agent-1", "new");
    let prompt = format!("before\n{old}\nmiddle\n{old}\nafter");
    let result = replace_memory_block(&prompt, &replacement).unwrap();
    assert_eq!(
        result,
        format!("before\n{replacement}\nmiddle\n{replacement}\nafter")
    );
}

#[test]
fn given_no_matching_block_when_replaced_then_the_sentinel_block_is_appended() {
    let replacement = mark_memory_block("agent-1", "new");
    let result = replace_memory_block("base prompt\n", &replacement).unwrap();
    assert_eq!(result, format!("base prompt\n\n{replacement}"));
}

#[test]
fn given_marked_regions_when_stripped_then_only_surrounding_prompt_content_remains() {
    let prompt = format!(
        "base\n\n{}\n\n{}",
        mark_memory_block("one", "first"),
        mark_memory_block("two", "second")
    );
    assert_eq!(strip_memory_block(&prompt), "base");
}

fn system_file(relative_path: &str, body: &str) -> CompiledSystemFile {
    CompiledSystemFile {
        relative_path: relative_path.to_string(),
        body: body.to_string(),
        description: String::new(),
    }
}

#[test]
fn projection_redacts_label_and_path() {
    // `sk-live-ABC123` matches the OpenAI key shape: `\bsk-(?:proj-)?[-_A-Za-z0-9]+\b`.
    let files = [system_file(
        "system/notes-sk-live-ABC123/sk-live-DEF456.md",
        "body",
    )];
    assert_eq!(
        render_system_tree(&files),
        [
            "<notes-***>",
            "  <***>",
            "    <projection>$MEMORY_DIR/system/notes-***/***.md</projection>",
            "    body",
            "  </***>",
            "</notes-***>",
        ]
        .join("\n")
    );
}

#[test]
fn projection_does_not_redact_body() {
    let files = [system_file(
        "system/clean/readme.md",
        "keep sk-live-ABC123 raw",
    )];
    assert_eq!(
        render_system_tree(&files),
        [
            "<clean>",
            "  <readme>",
            "    <projection>$MEMORY_DIR/system/clean/readme.md</projection>",
            "    keep sk-live-ABC123 raw",
            "  </readme>",
            "</clean>",
        ]
        .join("\n")
    );
}

#[test]
fn projection_leaves_clean_label_and_path_unchanged() {
    let files = [system_file("system/clean/notes.md", "plain")];
    assert_eq!(
        render_system_tree(&files),
        [
            "<clean>",
            "  <notes>",
            "    <projection>$MEMORY_DIR/system/clean/notes.md</projection>",
            "    plain",
            "  </notes>",
            "</clean>",
        ]
        .join("\n")
    );
}
