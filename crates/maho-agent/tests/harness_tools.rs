use maho_agent::{
    harness::{
        context::{Context, background_context, with_abort_signal},
        env::nodejs::NodeExecutionEnv,
        tools::{
            edit_diff::*, image::*, path_utils::*, post_mutate::*,
            tool_context::HasExecutionToolContext, *,
        },
        types::{
            AgentHarnessTool, AgentHarnessToolInvocation, AgentHarnessToolUpdateCallback,
            ExecutionError, ExecutionErrorCode, FileError, FileInfo, FileSystem, Shell,
            ShellExecOptions, ShellExecResult, ShellOutputTruncation, ShellOutputUpdate,
            ShellOutputView, TextLineReader, TruncationLimit,
        },
    },
    types::AgentToolResult,
};
use maho_ai::{
    types::{BoxFuture, ContentBlock},
    utils::abort::AbortController,
};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct Invocation;
impl AgentHarnessToolInvocation for Invocation {
    fn invocation_id(&self) -> &str {
        "test-result"
    }
    fn operation_id(&self) -> &str {
        "test-operation"
    }
    fn turn_id(&self) -> &str {
        "test-turn"
    }
    fn get_memo<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Option<Value>> {
        Box::pin(async { None })
    }
    fn set_memo<'a>(&'a self, _: &'a str, _: Option<Value>) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}
struct Fixture {
    dir: PathBuf,
    context: ExecutionToolContext,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "t15b-tools-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).expect("tool test operation must succeed");
        Self {
            context: ExecutionToolContext {
                env: Arc::new(NodeExecutionEnv::new(dir.to_string_lossy())),
                post_mutate: None,
            },
            dir,
        }
    }
    fn put(&self, path: &str, bytes: impl AsRef<[u8]>) {
        std::fs::write(self.dir.join(path), bytes).expect("tool test operation must succeed");
    }
    fn get(&self, path: &str) -> String {
        std::fs::read_to_string(self.dir.join(path)).expect("tool test operation must succeed")
    }
    async fn run(
        &self,
        tool: AgentHarnessTool<ExecutionToolContext>,
        input: Value,
    ) -> Result<AgentToolResult, String> {
        execute(
            tool,
            input,
            self.context.clone(),
            background_context(),
            Arc::new(|_, _| {}),
        )
        .await
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).expect("tool test operation must succeed");
    }
}
async fn execute<T: HasExecutionToolContext>(
    tool: AgentHarnessTool<T>,
    input: Value,
    turn: T,
    context: Context,
    update: AgentHarnessToolUpdateCallback,
) -> Result<AgentToolResult, String> {
    (tool.execute)(
        "call".into(),
        input,
        update,
        turn,
        Arc::new(Invocation),
        context,
    )
    .await
}
fn text(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|part| match part {
            ContentBlock::Text(part) => Some(part.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn edit(old: &str, new: &str) -> Edit {
    Edit {
        old_text: old.into(),
        new_text: new.into(),
    }
}
fn tiny_bmp() -> Vec<u8> {
    let mut bytes = vec![0; 58];
    bytes[..2].copy_from_slice(b"BM");
    bytes[2..6].copy_from_slice(&58u32.to_le_bytes());
    bytes[10..14].copy_from_slice(&54u32.to_le_bytes());
    bytes[14..18].copy_from_slice(&40u32.to_le_bytes());
    bytes[18] = 1;
    bytes[22] = 1;
    bytes[26] = 1;
    bytes[28] = 24;
    bytes[34] = 4;
    bytes
}

#[tokio::test]
async fn reads_offsets_limits_and_continuation() {
    let f = Fixture::new();
    f.put(
        "test.txt",
        (1..=100)
            .map(|i| format!("Line {i}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let result = f
        .run(
            create_read_tool(ReadToolOptions::default()),
            json!({"path":"test.txt","offset":41,"limit":20}),
        )
        .await
        .expect("tool test operation must succeed");
    let output = text(&result);
    assert!(!output.contains("Line 40"));
    assert!(output.contains("Line 41"));
    assert!(output.contains("Line 60"));
    assert!(!output.contains("Line 61"));
    assert!(output.ends_with("[40 more lines in file. Use offset=61 to continue.]"));
}
#[tokio::test]
async fn truncates_large_text_by_lines() {
    let f = Fixture::new();
    f.put(
        "large.txt",
        (1..=2500)
            .map(|i| format!("Line {i}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let result = f
        .run(
            create_read_tool(ReadToolOptions::default()),
            json!({"path":"large.txt"}),
        )
        .await
        .expect("tool test operation must succeed");
    assert!(
        text(&result).ends_with("[Showing lines 1-2000 of 2500. Use offset=2001 to continue.]")
    );
    assert_eq!(result.details["truncation"]["outputLines"], 2000);
    assert_eq!(result.details["truncation"]["totalLines"], 2500);
}
#[tokio::test]
async fn trailing_newline_does_not_exceed_line_limit() {
    let f = Fixture::new();
    f.put("exact.txt", "x\n".repeat(2000));
    let result = f
        .run(
            create_read_tool(ReadToolOptions::default()),
            json!({"path":"exact.txt"}),
        )
        .await
        .expect("tool test operation must succeed");
    assert!(result.details.is_null());
    assert!(!text(&result).contains("Use offset="));
}
#[tokio::test]
async fn rejects_offset_beyond_file() {
    let f = Fixture::new();
    f.put("short.txt", "one\ntwo\nthree");
    assert_eq!(
        f.run(
            create_read_tool(ReadToolOptions::default()),
            json!({"path":"short.txt","offset":100})
        )
        .await
        .unwrap_err(),
        "Offset 100 is beyond end of file (3 lines total)"
    );
}
#[tokio::test]
async fn detects_images_from_bytes_instead_of_extension() {
    let f = Fixture::new();
    let png = [137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82];
    f.put("image.txt", png);
    let result = f
        .run(
            create_read_tool(ReadToolOptions::default()),
            json!({"path":"image.txt"}),
        )
        .await
        .expect("tool test operation must succeed");
    assert_eq!(text(&result), "Read image file [image/png]");
    assert!(
        matches!(&result.content[1], ContentBlock::Image(image) if image.data == encode_base64(&png) && image.mime_type == "image/png")
    );
}
#[tokio::test]
async fn delegates_image_conversion_and_resize_options() {
    let f = Fixture::new();
    let bmp = tiny_bmp();
    f.put("image.bmp", &bmp);
    let seen = Arc::new(Mutex::new(None));
    let observed = seen.clone();
    let processor: ReadImageProcessor = Arc::new(move |bytes, mime, options, _| {
        *observed.lock().expect("tool test operation must succeed") = Some((bytes, mime, options.auto_resize_images));
        Box::pin(async {
            ReadImageProcessorResult::Ok {
                data: "converted".into(),
                mime_type: "image/png".into(),
                hints: vec!["converted bmp".into()],
            }
        })
    });
    let result = f
        .run(
            create_read_tool(ReadToolOptions {
                auto_resize_images: Some(false),
                image_processor: Some(processor),
            }),
            json!({"path":"image.bmp"}),
        )
        .await
        .expect("tool test operation must succeed");
    assert_eq!(
        *seen.lock().expect("tool test operation must succeed"),
        Some((bmp, "image/bmp".into(), false))
    );
    assert!(text(&result).ends_with("converted bmp"));
    assert!(matches!(&result.content[1], ContentBlock::Image(image) if image.data == "converted"));
}
#[tokio::test]
async fn writes_files_and_creates_parents() {
    let f = Fixture::new();
    let result = f
        .run(
            create_write_tool(),
            json!({"path":"nested/dir/file.txt","content":"hello"}),
        )
        .await
        .expect("tool test operation must succeed");
    assert_eq!(text(&result), "Successfully wrote to nested/dir/file.txt");
    assert_eq!(f.get("nested/dir/file.txt"), "hello");
}
#[tokio::test]
async fn applies_disjoint_edits_with_both_diff_formats() {
    let f = Fixture::new();
    f.put("edit.txt", "alpha\nbeta\ngamma\ndelta\n");
    let result = f.run(create_edit_tool(), json!({"path":"edit.txt","edits":[{"oldText":"alpha\n","newText":"ALPHA\n"},{"oldText":"gamma\n","newText":"GAMMA\n"}]})).await.expect("tool test operation must succeed");
    assert_eq!(f.get("edit.txt"), "ALPHA\nbeta\nGAMMA\ndelta\n");
    assert_eq!(
        text(&result),
        "Successfully replaced 2 block(s) in edit.txt."
    );
    assert!(result.details["diff"].as_str().expect("tool test operation must succeed").contains("GAMMA"));
    assert!(
        result.details["patch"]
            .as_str()
            .expect("tool test operation must succeed")
            .contains("+ALPHA\n")
    );
}
#[tokio::test]
async fn rejects_overlapping_original_matches() {
    let f = Fixture::new();
    f.put("edit.txt", "one\ntwo\nthree\n");
    let result = f.run(create_edit_tool(), json!({"path":"edit.txt","edits":[{"oldText":"one\ntwo\n","newText":"ONE\nTWO\n"},{"oldText":"two\nthree\n","newText":"TWO\nTHREE\n"}]})).await;
    assert!(result.unwrap_err().contains("overlap"));
    assert_eq!(f.get("edit.txt"), "one\ntwo\nthree\n");
}
#[tokio::test]
async fn rejects_missing_and_duplicate_targets() {
    let f = Fixture::new();
    f.put("edit.txt", "foo foo foo");
    for (old, error) in [
        ("bar", "Could not find the exact text"),
        ("foo", "Found 3 occurrences"),
    ] {
        assert!(
            f.run(
                create_edit_tool(),
                json!({"path":"edit.txt","edits":[{"oldText":old,"newText":"baz"}]})
            )
            .await
            .unwrap_err()
            .contains(error)
        );
    }
}
#[tokio::test]
async fn serializes_canonical_and_symlink_edits() {
    let f = Fixture::new();
    f.put("target.txt", "alpha\nbeta\ngamma\n");
    std::os::unix::fs::symlink("target.txt", f.dir.join("link.txt")).expect("tool test operation must succeed");
    let (a, b) = tokio::join!(
        f.run(
            create_edit_tool(),
            json!({"path":"target.txt","edits":[{"oldText":"alpha","newText":"ALPHA"}]})
        ),
        f.run(
            create_edit_tool(),
            json!({"path":"link.txt","edits":[{"oldText":"beta","newText":"BETA"}]})
        )
    );
    a.expect("tool test operation must succeed");
    b.expect("tool test operation must succeed");
    assert_eq!(f.get("target.txt"), "ALPHA\nBETA\ngamma\n");
}
#[tokio::test]
async fn edits_regular_files_through_symlinks() {
    let f = Fixture::new();
    f.put("target.txt", "before\n");
    std::os::unix::fs::symlink("target.txt", f.dir.join("link.txt")).expect("tool test operation must succeed");
    f.run(
        create_edit_tool(),
        json!({"path":"link.txt","edits":[{"oldText":"before","newText":"after"}]}),
    )
    .await
    .expect("tool test operation must succeed");
    assert_eq!(f.get("target.txt"), "after\n");
}
#[tokio::test]
async fn preserves_bom_and_crlf() {
    let f = Fixture::new();
    f.put("edit.txt", "\u{feff}one\r\ntwo\r\n");
    f.run(
        create_edit_tool(),
        json!({"path":"edit.txt","edits":[{"oldText":"two","newText":"TWO"}]}),
    )
    .await
    .expect("tool test operation must succeed");
    assert_eq!(f.get("edit.txt"), "\u{feff}one\r\nTWO\r\n");
}
#[tokio::test]
async fn executes_shell_stdout_and_stderr() {
    let f = Fixture::new();
    let result = f
        .run(
            create_bash_tool(BashToolOptions::default()),
            json!({"command":"printf out; printf err >&2"}),
        )
        .await
        .expect("tool test operation must succeed");
    assert!(text(&result).contains("out"));
    assert!(text(&result).contains("err"));
}
#[tokio::test]
async fn reports_nonzero_shell_exit() {
    let f = Fixture::new();
    let error = f
        .run(
            create_bash_tool(BashToolOptions::default()),
            json!({"command":"printf failed; exit 7"}),
        )
        .await
        .unwrap_err();
    assert!(error.contains("failed"));
    assert!(error.ends_with("Command exited with code 7"));
}
#[tokio::test]
async fn reports_shell_timeout() {
    let f = Fixture::new();
    let error = f
        .run(
            create_bash_tool(BashToolOptions::default()),
            json!({"command":"sleep 2","timeout":0.01}),
        )
        .await
        .unwrap_err();
    assert!(error.contains("Command timed out after 0.01 seconds"));
}
#[tokio::test]
async fn oversized_final_line_reports_total_size() {
    let f = Fixture::new();
    let result = f
        .run(
            create_bash_tool(BashToolOptions::default()),
            json!({"command":"printf '%060000d' 0"}),
        )
        .await
        .expect("tool test operation must succeed");
    assert!(text(&result).contains("Showing last 50.0KB of line 1 (line is 58.6KB). Full output:"));
}
#[tokio::test]
async fn supports_shell_command_prefixes() {
    let f = Fixture::new();
    let result = f
        .run(
            create_bash_tool(BashToolOptions {
                command_prefix: Some("value=hello".into()),
                prepare: None,
            }),
            json!({"command":"printf $value"}),
        )
        .await
        .expect("tool test operation must succeed");
    assert_eq!(text(&result), "hello");
}
#[tokio::test]
async fn prepares_command_cwd_and_explicit_environment() {
    let f = Fixture::new();
    std::fs::create_dir(f.dir.join("workspace")).expect("tool test operation must succeed");
    let expected = f.dir.join("workspace").to_string_lossy().into_owned();
    let target = expected.clone();
    let prepare: BashPrepare<ExecutionToolContext> = Arc::new(move |execution, _, _| {
        execution.cwd = target.clone();
        execution.env.insert("EXPLICIT".into(), json!("explicit"));
        execution.inherit_env = false;
        execution
            .command
            .push_str("\nprintf '%s:%s:%s' \"$prefix\" \"$EXPLICIT\" \"$PWD\"");
        Box::pin(async { Ok(()) })
    });
    let result = f
        .run(
            create_bash_tool(BashToolOptions {
                command_prefix: Some("prefix=ready".into()),
                prepare: Some(prepare),
            }),
            json!({"command":":"}),
        )
        .await
        .expect("tool test operation must succeed");
    assert_eq!(text(&result), format!("ready:explicit:{expected}"));
}
#[tokio::test]
async fn coalesces_updates_and_spills_full_output() {
    let f = Fixture::new();
    let updates = Arc::new(Mutex::new(Vec::new()));
    let observed = updates.clone();
    let result = execute(
        create_bash_tool(BashToolOptions::default()),
        json!({"command":"i=1; while [ $i -le 3000 ]; do echo line-$i; i=$((i + 1)); done"}),
        f.context.clone(),
        background_context(),
        Arc::new(move |update, _| observed.lock().expect("tool test operation must succeed").push(update)),
    )
    .await
    .expect("tool test operation must succeed");
    assert!(updates.lock().expect("tool test operation must succeed").len() < 25);
    assert_eq!(result.details["truncation"]["totalLines"], 3000);
    assert_eq!(result.details["truncation"]["outputLines"], 2000);
    assert!(text(&result).contains("line-3000"));
    let path = result.details["fullOutputPath"].as_str().expect("tool test operation must succeed");
    let full = std::fs::read_to_string(path).expect("tool test operation must succeed");
    assert!(full.contains("line-1\nline-2"));
    assert!(full.contains("line-2999\nline-3000"));
    std::fs::remove_file(path).expect("tool test operation must succeed");
}

#[test]
fn normalizes_line_endings() {
    assert_eq!(normalize_to_lf("a\r\nb\rc\n"), "a\nb\nc\n");
}
#[test]
fn detects_first_line_ending() {
    assert_eq!(detect_line_ending("a\r\nb\n"), "\r\n");
    assert_eq!(detect_line_ending("a\nb\r\n"), "\n");
}
#[test]
fn restores_crlf() {
    assert_eq!(restore_line_endings("a\nb\n", "\r\n"), "a\r\nb\r\n");
}
#[test]
fn strips_bom() {
    assert_eq!(strip_bom("\u{feff}a"), ("\u{feff}", "a"));
    assert_eq!(strip_bom("a"), ("", "a"));
}
#[test]
fn fuzzy_normalizes_unicode() {
    assert_eq!(
        normalize_for_fuzzy_match("“Ａ”\u{a0}— ‘b’  \n"),
        "\"A\" - 'b'\n"
    );
}
#[test]
fn exact_match_precedes_fuzzy() {
    let result = fuzzy_find_text("a  \na\n", "a  ");
    assert_eq!(result.index, Some(0));
    assert!(!result.used_fuzzy_match);
}
#[test]
fn fuzzy_match_returns_normalized_base() {
    let result = fuzzy_find_text("“hello”", "\"hello\"");
    assert!(result.used_fuzzy_match);
    assert_eq!(result.content_for_replacement, "\"hello\"");
}
#[test]
fn missing_fuzzy_match_preserves_base() {
    let result = fuzzy_find_text("“hello”", "absent");
    assert!(!result.found);
    assert_eq!(result.content_for_replacement, "“hello”");
}
#[test]
fn rejects_empty_old_text() {
    assert_eq!(
        apply_edits_to_normalized_content("a", &[edit("", "b")], "x").unwrap_err(),
        "oldText must not be empty in x."
    );
}
#[test]
fn rejects_empty_old_text_in_multiple_edits() {
    assert_eq!(
        apply_edits_to_normalized_content("a", &[edit("a", "b"), edit("", "c")], "x").unwrap_err(),
        "edits[1].oldText must not be empty in x."
    );
}
#[test]
fn rejects_identical_single_replacement() {
    assert!(
        apply_edits_to_normalized_content("a", &[edit("a", "a")], "x")
            .unwrap_err()
            .contains("replacement produced identical content")
    );
}
#[test]
fn rejects_identical_multiple_replacements() {
    assert_eq!(
        apply_edits_to_normalized_content("ab", &[edit("a", "a"), edit("b", "b")], "x")
            .unwrap_err(),
        "No changes made to x. The replacements produced identical content."
    );
}
#[test]
fn edits_match_original_not_intermediate() {
    assert!(
        apply_edits_to_normalized_content("a", &[edit("a", "b"), edit("b", "c")], "x")
            .unwrap_err()
            .contains("Could not find edits[1]")
    );
}
#[test]
fn duplicate_fuzzy_matches_are_ambiguous() {
    assert!(
        apply_edits_to_normalized_content("‘a’\n'a'", &[edit("'a'", "b")], "x")
            .unwrap_err()
            .contains("Found 2 occurrences")
    );
}
#[test]
fn fuzzy_edits_preserve_untouched_lines() {
    let result =
        apply_edits_to_normalized_content("“one”  \n“keep”  \n", &[edit("\"one\"", "ONE")], "x")
            .expect("tool test operation must succeed");
    assert_eq!(result.new_content, "ONE\n“keep”  \n");
}
#[test]
fn adjacent_replacements_are_allowed() {
    assert_eq!(
        apply_edits_to_normalized_content("abcd", &[edit("ab", "A"), edit("cd", "B")], "x")
            .expect("tool test operation must succeed")
            .new_content,
        "AB"
    );
}
#[test]
fn reversed_replacements_keep_offsets() {
    assert_eq!(
        apply_edits_to_normalized_content("abcd", &[edit("cd", "LONG"), edit("ab", "x")], "x")
            .expect("tool test operation must succeed")
            .new_content,
        "xLONG"
    );
}
#[test]
fn display_diff_has_line_numbers() {
    let result = generate_diff_string("one\ntwo\n", "ONE\ntwo\n", 4);
    assert_eq!(result.diff, "-1 one\n+1 ONE\n 2 two");
    assert_eq!(result.first_changed_line, Some(1));
}
#[test]
fn unchanged_diff_is_empty() {
    let result = generate_diff_string("one\n", "one\n", 4);
    assert_eq!(result.diff, "");
    assert_eq!(result.first_changed_line, None);
}
#[test]
fn distant_diff_context_is_elided() {
    let old = (1..=20).map(|i| format!("{i}\n")).collect::<String>();
    let new = old.replacen("10\n", "TEN\n", 1);
    assert!(generate_diff_string(&old, &new, 2).diff.contains(" ..."));
}
#[test]
fn base64_handles_padding() {
    assert_eq!(encode_base64(b""), "");
    assert_eq!(encode_base64(b"f"), "Zg==");
    assert_eq!(encode_base64(b"fo"), "Zm8=");
    assert_eq!(encode_base64(b"foo"), "Zm9v");
}
#[test]
fn rejects_jpeg_ls() {
    assert_eq!(
        detect_supported_image_mime_type(&[255, 216, 255, 247]),
        None
    );
    assert_eq!(
        detect_supported_image_mime_type(&[255, 216, 255]),
        Some("image/jpeg")
    );
}
#[test]
fn detects_gif_and_webp() {
    assert_eq!(
        detect_supported_image_mime_type(b"GIF89a"),
        Some("image/gif")
    );
    assert_eq!(
        detect_supported_image_mime_type(b"RIFF1234WEBP"),
        Some("image/webp")
    );
}
#[test]
fn validates_bmp_header() {
    let mut bmp = tiny_bmp();
    assert_eq!(detect_supported_image_mime_type(&bmp), Some("image/bmp"));
    bmp[26] = 2;
    assert_eq!(detect_supported_image_mime_type(&bmp), None);
}
#[test]
fn rejects_incomplete_png() {
    assert_eq!(
        detect_supported_image_mime_type(&[137, 80, 78, 71, 13, 10, 26, 10]),
        None
    );
}
#[test]
fn prepares_legacy_edit_arguments() {
    assert_eq!(
        edit::prepare_edit_arguments(json!({"path":"x","oldText":"a","newText":"b"})),
        json!({"path":"x","edits":[{"oldText":"a","newText":"b"}]})
    );
}
#[test]
fn prepares_stringified_edit_array() {
    assert_eq!(
        edit::prepare_edit_arguments(
            json!({"path":"x","edits":"[{\"oldText\":\"a\",\"newText\":\"b\"}]"})
        )["edits"],
        json!([{"oldText":"a","newText":"b"}])
    );
}
#[test]
fn prepares_single_edit_object() {
    assert_eq!(
        edit::prepare_edit_arguments(json!({"edits":{"oldText":"a","newText":"b"}}))["edits"],
        json!([{"oldText":"a","newText":"b"}])
    );
}
#[test]
fn invalid_edit_json_remains_invalid() {
    assert_eq!(
        edit::prepare_edit_arguments(json!({"edits":"not json"})),
        json!({"edits":"not json"})
    );
}
#[test]
fn timeout_rejects_nonfinite_and_nonpositive() {
    for value in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(bash::validate_timeout(Some(value)).is_err());
    }
}
#[test]
fn timeout_rejects_overflow() {
    assert!(bash::validate_timeout(Some(2_147_483.648)).is_err());
    assert!(bash::validate_timeout(Some(2_147_483.647)).is_ok());
}
#[test]
fn absent_timeout_is_allowed() {
    assert!(bash::validate_timeout(None).is_ok());
}
#[tokio::test]
async fn resolves_at_prefix_and_unicode_spaces() {
    let f = Fixture::new();
    assert_eq!(
        resolve_tool_path(f.context.env.as_ref(), "@a\u{a0}b", &background_context())
            .await
            .expect("tool test operation must succeed"),
        f.dir.join("a b").to_string_lossy()
    );
}
#[tokio::test]
async fn resolves_decomposed_read_filename() {
    let f = Fixture::new();
    f.put("e\u{301}.txt", "x");
    assert!(
        resolve_read_tool_path(f.context.env.as_ref(), "é.txt", &background_context())
            .await
            .expect("tool test operation must succeed")
            .ends_with("e\u{301}.txt")
    );
}
#[tokio::test]
async fn resolves_smart_quote_read_filename() {
    let f = Fixture::new();
    f.put("it’s.txt", "x");
    assert!(
        resolve_read_tool_path(f.context.env.as_ref(), "it's.txt", &background_context())
            .await
            .expect("tool test operation must succeed")
            .ends_with("it’s.txt")
    );
}
#[tokio::test]
async fn omits_bmp_without_processor() {
    let f = Fixture::new();
    f.put("x.bmp", tiny_bmp());
    let result = f
        .run(
            create_read_tool(ReadToolOptions::default()),
            json!({"path":"x.bmp"}),
        )
        .await
        .expect("tool test operation must succeed");
    assert_eq!(result.content.len(), 1);
    assert!(text(&result).contains("Image omitted"));
}
#[tokio::test]
async fn preserves_image_processor_error_as_text() {
    let f = Fixture::new();
    f.put("x.bmp", tiny_bmp());
    let result = f
        .run(
            create_read_tool(ReadToolOptions {
                image_processor: Some(Arc::new(|_, _, _, _| {
                    Box::pin(async {
                        ReadImageProcessorResult::Err {
                            message: "conversion failed".into(),
                        }
                    })
                })),
                auto_resize_images: None,
            }),
            json!({"path":"x.bmp"}),
        )
        .await
        .expect("tool test operation must succeed");
    assert_eq!(
        text(&result),
        "Read image file [image/bmp]\nconversion failed"
    );
}
#[tokio::test]
async fn rejects_empty_edit_list() {
    let f = Fixture::new();
    assert!(
        f.run(create_edit_tool(), json!({"path":"x","edits":[]}))
            .await
            .unwrap_err()
            .contains("at least one replacement")
    );
}
#[tokio::test]
async fn preaborted_write_does_not_land() {
    let f = Fixture::new();
    let controller = AbortController::new();
    controller.abort(None);
    let result = execute(
        create_write_tool(),
        json!({"path":"x","content":"a"}),
        f.context.clone(),
        with_abort_signal(controller.signal(), &background_context()),
        Arc::new(|_, _| {}),
    )
    .await;
    assert!(result.is_err());
    assert!(!f.dir.join("x").exists());
}
#[test]
fn notes_preserve_present_empty_strings() {
    assert_eq!(
        append_post_mutate_note("written", &[None, Some(String::new()), Some("note".into())]),
        "written\n\nnote"
    );
}

enum Script {
    Native,
    Late,
    Checkpoints,
    Timeout,
}
struct ControlledEnv {
    inner: NodeExecutionEnv,
    script: Script,
    late: Mutex<Option<maho_agent::harness::types::ShellOutputUpdateHandler>>,
    started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    writes: AtomicUsize,
}
impl ControlledEnv {
    fn new(f: &Fixture, script: Script) -> Self {
        Self {
            inner: NodeExecutionEnv::new(f.dir.to_string_lossy()),
            script,
            late: Mutex::new(None),
            started: Mutex::new(None),
            release: Mutex::new(None),
            writes: AtomicUsize::new(0),
        }
    }
}
macro_rules! forward_fs {
    ($($name:ident($($arg:ident:$ty:ty),*) -> $out:ty;)*) => { $(fn $name<'a>(&'a self, $($arg:$ty,)* context:&'a Context)->BoxFuture<'a,Result<$out,FileError>> { self.inner.$name($($arg,)*context) })* };
}
impl FileSystem for ControlledEnv {
    fn cwd(&self) -> &str {
        self.inner.cwd()
    }
    forward_fs! {
        absolute_path(path:&'a str)->String;
        join_path(parts:Vec<String>)->String;
        read_text_file(path:&'a str)->String;
        open_text_line_reader(path:&'a str)->Box<dyn TextLineReader>;
        read_text_lines(path:&'a str,max_lines:Option<u64>)->Vec<String>;
        read_binary_file(path:&'a str)->Vec<u8>;
        append_file(path:&'a str,content:&'a [u8])->();
        rename_file(source:&'a str,destination:&'a str)->();
        file_info(path:&'a str)->FileInfo;
        list_dir(path:&'a str)->Vec<FileInfo>;
        canonical_path(path:&'a str)->String;
        exists(path:&'a str)->bool;
        create_dir(path:&'a str,recursive:Option<bool>)->();
        remove(path:&'a str,recursive:Option<bool>,force:Option<bool>)->();
        create_temp_dir(prefix:Option<String>)->String;
        create_temp_file(prefix:Option<String>,suffix:Option<String>)->String;
    }
    fn write_file<'a>(
        &'a self,
        path: &'a str,
        content: &'a [u8],
        context: &'a Context,
    ) -> BoxFuture<'a, Result<(), FileError>> {
        Box::pin(async move {
            let first = self.writes.fetch_add(1, Ordering::SeqCst) == 0;
            if first {
                if let Some(started) = self.started.lock().expect("tool test operation must succeed").take() {
                    started.send(()).expect("tool test operation must succeed");
                }
                let release = self.release.lock().expect("tool test operation must succeed").take();
                if let Some(release) = release {
                    release.await.expect("tool test operation must succeed");
                }
                self.inner
                    .write_file(path, content, &background_context())
                    .await
            } else {
                self.inner.write_file(path, content, context).await
            }
        })
    }
    fn cleanup<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, ()> {
        FileSystem::cleanup(&self.inner, context)
    }
}
async fn emit(options: &ShellExecOptions, text: &str, spill: Option<String>) -> ShellExecResult {
    use maho_agent::harness::utils::truncate::{TruncatedBy, TruncationOptions, truncate_tail};
    let t = truncate_tail(text, TruncationOptions::default());
    let truncation = ShellOutputTruncation {
        truncated: t.truncated,
        truncated_by: t.truncated_by.map(|by| match by {
            TruncatedBy::Lines => TruncationLimit::Lines,
            TruncatedBy::Bytes => TruncationLimit::Bytes,
        }),
        total_lines: t.total_lines,
        total_bytes: t.total_bytes,
        output_lines: t.output_lines,
        output_bytes: t.output_bytes,
        last_line_partial: t.last_line_partial,
        first_line_exceeds_limit: t.first_line_exceeds_limit,
        max_lines: t.max_lines,
        max_bytes: t.max_bytes,
    };
    if let Some(update) = &options.on_update {
        update(
            ShellOutputUpdate::Replace {
                output: ShellOutputView {
                    text: t.content,
                    truncation: truncation.clone(),
                    spill_path: spill.clone(),
                    last_line_bytes: None,
                },
            },
            background_context(),
        )
        .await;
    }
    ShellExecResult {
        exit_code: 0,
        truncation,
        spill_path: spill,
        last_line_bytes: None,
    }
}
impl Shell for ControlledEnv {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        options: Option<ShellExecOptions>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<ShellExecResult, ExecutionError>> {
        Box::pin(async move {
            let options = options.expect("tool test operation must succeed");
            match self.script {
                Script::Native => self.inner.exec(command, Some(options), context).await,
                Script::Late => {
                    let result = emit(&options, "before\n", None).await;
                    *self.late.lock().expect("tool test operation must succeed") = options.on_update;
                    Ok(result)
                }
                Script::Checkpoints => {
                    emit(&options, "one\n", None).await;
                    tokio::time::advance(std::time::Duration::from_millis(2100)).await;
                    emit(&options, "one\ntwo\n", None).await;
                    tokio::time::advance(std::time::Duration::from_millis(100)).await;
                    emit(&options, "one\ntwo\nthree\n", None).await;
                    tokio::time::advance(std::time::Duration::from_millis(2000)).await;
                    Ok(emit(&options, "one\ntwo\nthree\nfour\n", None).await)
                }
                Script::Timeout => {
                    let output = (1..=2001)
                        .map(|i| format!("line-{i}\n"))
                        .collect::<String>();
                    let path = format!("{}/timeout.log", self.inner.cwd());
                    std::fs::write(&path, &output).expect("tool test operation must succeed");
                    emit(&options, &output, Some(path)).await;
                    Err(ExecutionError::new(
                        ExecutionErrorCode::Timeout,
                        "controlled timeout",
                    ))
                }
            }
        })
    }
    fn cleanup<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, ()> {
        Shell::cleanup(&self.inner, context)
    }
}
#[tokio::test]
async fn ignores_callbacks_after_shell_settles() {
    let f = Fixture::new();
    let env = Arc::new(ControlledEnv::new(&f, Script::Late));
    let updates = Arc::new(Mutex::new(Vec::new()));
    let seen = updates.clone();
    let result = execute(
        create_bash_tool(BashToolOptions::default()),
        json!({"command":"late"}),
        ExecutionToolContext {
            env: env.clone(),
            post_mutate: None,
        },
        background_context(),
        Arc::new(move |update, _| seen.lock().expect("tool test operation must succeed").push(text(&update))),
    )
    .await
    .expect("tool test operation must succeed");
    let callback = env.late.lock().expect("tool test operation must succeed").take().expect("tool test operation must succeed");
    let options = ShellExecOptions {
        on_update: Some(callback),
        ..Default::default()
    };
    emit(&options, "before\nlate\n", None).await;
    assert_eq!(text(&result), "before\n");
    assert!(
        updates
            .lock()
            .expect("tool test operation must succeed")
            .iter()
            .all(|update| !update.contains("late"))
    );
}
#[tokio::test(start_paused = true)]
async fn requests_distinct_checkpoints_at_two_second_intervals() {
    let f = Fixture::new();
    let env = Arc::new(ControlledEnv::new(&f, Script::Checkpoints));
    let checkpoints = Arc::new(Mutex::new(Vec::new()));
    let seen = checkpoints.clone();
    execute(
        create_bash_tool(BashToolOptions::default()),
        json!({"command":"controlled"}),
        ExecutionToolContext {
            env,
            post_mutate: None,
        },
        background_context(),
        Arc::new(move |update, options| {
            if options.and_then(|options| options.checkpoint) == Some(true) {
                seen.lock().expect("tool test operation must succeed").push(text(&update));
            }
        }),
    )
    .await
    .expect("tool test operation must succeed");
    assert_eq!(
        *checkpoints.lock().expect("tool test operation must succeed"),
        vec!["one\ntwo\n", "one\ntwo\nthree\nfour\n"]
    );
}
#[tokio::test]
async fn preserves_truncated_output_on_timeout() {
    let f = Fixture::new();
    let env = Arc::new(ControlledEnv::new(&f, Script::Timeout));
    let error = execute(
        create_bash_tool(BashToolOptions::default()),
        json!({"command":"controlled","timeout":0.05}),
        ExecutionToolContext {
            env,
            post_mutate: None,
        },
        background_context(),
        Arc::new(|_, _| {}),
    )
    .await
    .unwrap_err();
    assert!(error.contains("Command timed out after 0.05 seconds"));
    assert!(error.contains("Full output:"));
    assert!(error.contains("line-2001"));
    let full = f.get("timeout.log");
    assert!(full.starts_with("line-1\nline-2"));
    assert!(full.ends_with("line-2000\nline-2001\n"));
}
async fn aborted_mutation_keeps_queue(editing: bool) {
    let f = Fixture::new();
    f.put("file.txt", "alpha\nbeta\n");
    let mut env = ControlledEnv::new(&f, Script::Native);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    env.started = Mutex::new(Some(started_tx));
    env.release = Mutex::new(Some(release_rx));
    let env = Arc::new(env);
    let turn = ExecutionToolContext {
        env: env.clone(),
        post_mutate: None,
    };
    let controller = AbortController::new();
    let (first_tool, first_input, second_tool, second_input) = if editing {
        (
            create_edit_tool(),
            json!({"path":"file.txt","edits":[{"oldText":"alpha","newText":"ALPHA"}]}),
            create_edit_tool(),
            json!({"path":"file.txt","edits":[{"oldText":"beta","newText":"BETA"}]}),
        )
    } else {
        (
            create_write_tool(),
            json!({"path":"file.txt","content":"first\n"}),
            create_write_tool(),
            json!({"path":"file.txt","content":"second\n"}),
        )
    };
    let first = execute(
        first_tool,
        first_input,
        turn.clone(),
        with_abort_signal(controller.signal(), &background_context()),
        Arc::new(|_, _| {}),
    );
    tokio::pin!(first);
    assert!(futures::poll!(&mut first).is_pending());
    tokio::time::timeout(std::time::Duration::from_secs(2), started_rx)
        .await
        .expect("tool test operation must succeed")
        .expect("tool test operation must succeed");
    controller.abort(None);
    let second = execute(
        second_tool,
        second_input,
        turn,
        background_context(),
        Arc::new(|_, _| {}),
    );
    tokio::pin!(second);
    assert!(futures::poll!(&mut second).is_pending());
    assert_eq!(env.writes.load(Ordering::SeqCst), 1);
    release_tx.send(()).expect("tool test operation must succeed");
    let (first, second) = tokio::join!(first, second);
    assert_eq!(first.expect_err("first mutation must observe cancellation"), "Operation aborted");
    second.expect("tool test operation must succeed");
    assert_eq!(
        f.get("file.txt"),
        if editing { "ALPHA\nBETA\n" } else { "second\n" }
    );
}
#[tokio::test]
async fn aborted_write_holds_queue_until_write_settles() {
    aborted_mutation_keeps_queue(false).await;
}
#[tokio::test]
async fn aborted_edit_holds_queue_until_write_settles() {
    aborted_mutation_keeps_queue(true).await;
}
