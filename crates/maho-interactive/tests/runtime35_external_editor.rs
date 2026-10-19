use maho_interactive::external_editor::*;

#[tokio::test]
async fn external_editor_edits_private_prompt_and_cleans_directory() {
    let directory = tempfile::tempdir().expect("directory");
    let capture = directory.path().join("path");
    let (editor, script) = if cfg!(windows) {
        let script = directory.path().join("editor.js");
        std::fs::write(&script, "const fs=require('fs');fs.writeFileSync(process.argv[2],process.argv[3]);fs.writeFileSync(process.argv[3],'\\ufeffedited\\n');").expect("script");
        ("node", script)
    } else {
        let script = directory.path().join("editor.sh");
        std::fs::write(&script, "printf '%s' \"$2\" > \"$1\"\nprintf '\\357\\273\\277edited\\n' > \"$2\"\n").expect("script");
        ("/bin/sh", script)
    };
    let command = format!("{} {} {}", editor, script.display(), capture.display());
    let result = edit_in_external_editor(&command, "original").await.expect("edit");
    assert_eq!(result, ExternalEditorResult::Complete { content: "edited".into() });
    let path = std::fs::read_to_string(capture).expect("capture");
    assert!(!std::path::Path::new(&path).parent().expect("parent").exists());
}

#[tokio::test]
async fn external_editor_nonzero_exit_is_distinct_from_launch_failure() {
    let command = if cfg!(windows) { "node -e process.exit(1)" } else { "/usr/bin/false" };
    assert_eq!(edit_in_external_editor(command, "original").await.expect("edit"), ExternalEditorResult::Failed);
}

#[tokio::test]
async fn external_editor_can_clear_prompt() {
    let directory = tempfile::tempdir().expect("directory");
    let (editor, script) = if cfg!(windows) {
        let script = directory.path().join("editor.js");
        std::fs::write(&script, "const fs=require('fs');fs.writeFileSync(process.argv[2],'');").expect("script");
        ("node", script)
    } else {
        let script = directory.path().join("editor.sh");
        std::fs::write(&script, ": > \"$1\"\n").expect("script");
        ("/bin/sh", script)
    };
    let command = format!("{} {}", editor, script.display());
    assert_eq!(edit_in_external_editor(&command, "original").await.expect("edit"), ExternalEditorResult::Complete { content: String::new() });
}

#[tokio::test]
async fn nonexistent_editor_is_launch_failure() {
    let directory = tempfile::tempdir().expect("directory");
    let command = directory.path().join("missing").to_string_lossy().into_owned();
    assert_eq!(edit_in_external_editor(&command, "original").await.expect("edit"), ExternalEditorResult::LaunchFailed);
}

#[tokio::test]
async fn edit_file_keeps_changes_after_unsuccessful_editor() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("prompt.md");
    let (editor, script) = if cfg!(windows) {
        let script = directory.path().join("editor.js");
        std::fs::write(&script, "const fs=require('fs');fs.writeFileSync(process.argv[2],'edited');process.exit(7);").expect("script");
        ("node", script)
    } else {
        let script = directory.path().join("editor.sh");
        std::fs::write(&script, "printf edited > \"$1\"\nexit 7\n").expect("script");
        ("/bin/sh", script)
    };
    let command = format!("{} {}", editor, script.display());
    assert_eq!(edit_file_in_external_editor(&command, &path).await.expect("edit"), EditFileResult::Exited { code: 7 });
    assert_eq!(std::fs::read_to_string(path).expect("read"), "edited");
}
