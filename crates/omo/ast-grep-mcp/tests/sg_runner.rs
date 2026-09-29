#![cfg(unix)]

mod common;

use std::ffi::CString;
use std::io::BufRead;
use std::io::BufReader;
use std::path::Path;
use std::path::PathBuf;

use ast_grep_mcp::AbortSignal;
use ast_grep_mcp::sg_runner::MAX_MCP_PAYLOAD_BYTES;
use ast_grep_mcp::sg_runner::MAX_STDERR_BYTES;
use ast_grep_mcp::sg_runner::SgRunnerError;
use ast_grep_mcp::sg_runner::SgRunnerErrorCode;
use ast_grep_mcp::sg_runner::SgRunnerInput;
use ast_grep_mcp::sg_runner::SgRunnerResult;
use ast_grep_mcp::sg_runner::SgTruncationReason;
use ast_grep_mcp::sg_runner::spawn_sg_runner;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

const FAKE_SG: &str = r#"
mode="$1"
marker="$2"
rec() { printf '{"text":"m%s","file":"/repo/f%s.ts","range":{"byteOffset":{"start":%s,"end":%s},"start":{"line":0,"column":%s},"end":{"line":0,"column":%s}},"metaVariables":{"single":{},"multi":{}}}' "$1" "$1" "$1" "$(($1 + 1))" "$1" "$(($1 + 1))"; }
repeat() { head -c "$2" /dev/zero | tr '\0' "$1"; }
case "$mode" in
  complete)
    one="$(rec 1)"
    printf '%s\n%s' "$(rec 2)" "$(printf '%s' "$one" | cut -c1-12)"
    printf '%s\n%s\n' "$(printf '%s' "$one" | cut -c13-)" "$(rec 3)" ;;
  truncated) printf '%s\n{"text":"cut' "$(rec 1)" ;;
  oversized) printf '{"text":"'; repeat x 1048577; printf '"}\n' ;;
  malformed) printf 'garbage\n{bad json}\n' ;;
  invalid-utf8) printf '\377\n' ;;
  stderr) repeat e 65636 >&2 ;;
  stderr-invalid) head -c 65636 /dev/zero | tr '\0' '\377' >&2 ;;
  stderr-boundary) { repeat a 65535; printf '\342\202\254'; } >&2 ;;
  exit-1) exit 1 ;;
  exit-1-warning) printf 'WARNING: no matches in ignored files' >&2; exit 1 ;;
  exit-1-controlled-error) printf 'ERROR: nonexistent.ts: No such file or directory' >&2; exit 1 ;;
  exit-1-controlled-flood) repeat W 65537 >&2; printf '\nERROR: operational failure after warning flood' >&2; exit 1 ;;
  exit-1-controlled-split) repeat W 65537 >&2; printf '\nER' >&2; printf 'ROR: split operational failure' >&2; exit 1 ;;
  exit-2) printf 'invalid language' >&2; exit 2 ;;
  many)
    i=0
    while [ "$i" -lt 20 ]; do rec "$i"; printf '\n'; i=$((i + 1)); done
    exec sleep 1000 ;;
  payload-cap)
    printf '%s' "$$" > "$marker.pid"
    i=0
    while :; do
      if [ "$i" -lt 8 ]; then printf '{"text":"'; repeat x 900000; printf '","file":"/repo/f%s.ts"}\n' "$i"; i=$((i + 1)); fi
      sleep 0.05
    done ;;
  hang)
    printf '%s' "$$" > "$marker.pid"
    trap 'printf SIGTERM > "$marker.term"' TERM
    printf 'ready\n' > "$marker.ready"
    while :; do sleep 0.05; done ;;
esac
"#;

struct Fixture {
    dir: tempfile::TempDir,
    executable: String,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("omo-sg-runner-")
            .tempdir()
            .unwrap();
        let executable = common::write_script(dir.path(), "fake-sg.sh", FAKE_SG);
        Self { dir, executable }
    }

    fn workdir(&self) -> String {
        common::path_str(self.dir.path())
    }

    fn marker(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn run(
        &self,
        args: &[&str],
        max_matches: Option<u64>,
    ) -> Result<SgRunnerResult, SgRunnerError> {
        self.run_with(args, max_matches, None, None)
    }

    fn run_with(
        &self,
        args: &[&str],
        max_matches: Option<u64>,
        timeout_ms: Option<u64>,
        signal: Option<&AbortSignal>,
    ) -> Result<SgRunnerResult, SgRunnerError> {
        spawn_sg_runner(SgRunnerInput {
            sg_path: &self.executable,
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            workdir: &self.workdir(),
            env: None,
            max_matches,
            timeout_ms,
            signal,
        })
    }
}

fn texts(result: &SgRunnerResult) -> Vec<&str> {
    result
        .records
        .iter()
        .map(|record| record["text"].as_str().unwrap())
        .collect()
}

fn process_alive(marker: &Path) -> bool {
    let pid: libc::pid_t = std::fs::read_to_string(marker.with_extension("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal 0 only probes for existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn err_code(result: Result<SgRunnerResult, SgRunnerError>) -> SgRunnerErrorCode {
    result.expect_err("runner must reject").code
}

#[test]
fn complete_chunked_ndjson_is_fully_parsed() {
    let fixture = Fixture::new();
    let result = fixture.run(&["complete"], Some(10)).unwrap();
    assert_eq!(texts(&result), vec!["m2", "m1", "m3"]);
    assert!(!result.truncated);
    assert_eq!(result.reason, None);
    assert_eq!(result.salvaged_records, 0);
    assert_eq!(result.at_least_matches, 3);
}

#[test]
fn partial_final_line_salvages_complete_records() {
    let fixture = Fixture::new();
    let result = fixture.run(&["truncated"], Some(10)).unwrap();
    assert_eq!(result.records.len(), 1);
    assert!(result.truncated);
    assert_eq!(result.reason, Some(SgTruncationReason::SgOutputTruncated));
    assert_eq!(result.salvaged_records, 1);
}

#[test]
fn stream_failures_are_classified() {
    let fixture = Fixture::new();
    assert_eq!(
        err_code(fixture.run(&["oversized"], None)),
        SgRunnerErrorCode::OutputTooLarge
    );
    assert_eq!(
        err_code(fixture.run(&["malformed"], None)),
        SgRunnerErrorCode::OutputParseFailed
    );
    assert_eq!(
        err_code(fixture.run(&["invalid-utf8"], None)),
        SgRunnerErrorCode::EncodingError
    );
    assert_eq!(fixture.run(&[], None).unwrap().records, Vec::new());
}

#[test]
fn exit_2_rejects_as_sg_failed() {
    let fixture = Fixture::new();
    let error = fixture.run(&["exit-2"], None).unwrap_err();
    assert_eq!(error.code, SgRunnerErrorCode::SgFailed);
    assert_eq!(error.stderr, "invalid language");
}

#[test]
fn exit_1_without_matches_is_successful() {
    let fixture = Fixture::new();
    let result = fixture.run(&["exit-1"], None).unwrap();
    assert!(result.records.is_empty());
    assert_eq!(result.exit_code, Some(1));
    assert!(!result.truncated);
}

#[test]
fn exit_1_with_operational_diagnostic_rejects() {
    let fixture = Fixture::new();
    let error = fixture.run(&["exit-1-controlled-error"], None).unwrap_err();
    assert_eq!(error.code, SgRunnerErrorCode::SgFailed);
    assert_eq!(
        error.stderr,
        "ERROR: nonexistent.ts: No such file or directory"
    );
}

#[test]
fn error_line_after_stderr_cap_rejects() {
    let fixture = Fixture::new();
    assert_eq!(
        err_code(fixture.run(&["exit-1-controlled-flood"], None)),
        SgRunnerErrorCode::SgFailed
    );
}

#[test]
fn error_prefix_split_across_writes_rejects() {
    let fixture = Fixture::new();
    assert_eq!(
        err_code(fixture.run(&["exit-1-controlled-split"], None)),
        SgRunnerErrorCode::SgFailed
    );
}

#[test]
fn exit_1_with_warning_only_is_valid_no_match() {
    let fixture = Fixture::new();
    let result = fixture.run(&["exit-1-warning"], None).unwrap();
    assert!(result.records.is_empty());
    assert_eq!(result.exit_code, Some(1));
    assert_eq!(result.stderr, "WARNING: no matches in ignored files");
}

#[test]
fn match_limit_terminates_with_honest_lower_bound() {
    let fixture = Fixture::new();
    let result = fixture.run(&["many"], Some(2)).unwrap();
    assert_eq!(result.records.len(), 2);
    assert!(result.truncated);
    assert_eq!(result.reason, Some(SgTruncationReason::MatchLimit));
    assert_eq!(result.at_least_matches, 3);
}

#[test]
fn aggregate_payload_cap_truncates_and_terminates_child() {
    let fixture = Fixture::new();
    let marker = fixture.marker("payload-cap-child");
    let result = fixture
        .run(&["payload-cap", marker.to_str().unwrap()], Some(500))
        .unwrap();
    assert!(result.truncated);
    assert_eq!(result.reason, Some(SgTruncationReason::OutputCap));
    assert_eq!(result.at_least_matches, result.records.len() + 1);
    assert_eq!(result.max_payload_bytes, MAX_MCP_PAYLOAD_BYTES);
    let serialized =
        Value::Array(result.records.iter().cloned().map(Value::Object).collect()).to_string();
    assert!(serialized.len() <= MAX_MCP_PAYLOAD_BYTES);
    assert!(!process_alive(&marker));
}

#[test]
fn excessive_stderr_is_capped_by_bytes() {
    let fixture = Fixture::new();
    let result = fixture.run(&["stderr"], None).unwrap();
    assert_eq!(result.stderr.len(), MAX_STDERR_BYTES);
}

#[test]
fn invalid_and_split_utf8_stderr_stays_within_cap() {
    let fixture = Fixture::new();
    let invalid = fixture.run(&["stderr-invalid"], None).unwrap();
    let boundary = fixture.run(&["stderr-boundary"], None).unwrap();
    assert!(invalid.stderr.len() <= MAX_STDERR_BYTES);
    assert_eq!(boundary.stderr, "a".repeat(MAX_STDERR_BYTES - 1));
}

fn make_fifo(path: &Path) {
    let c_path = CString::new(path.to_str().unwrap()).unwrap();
    // SAFETY: c_path is a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
}

#[test]
fn abort_escalates_to_sigkill_after_grace_and_no_process_remains() {
    let fixture = Fixture::new();
    let marker = fixture.marker("child");
    make_fifo(&marker.with_extension("ready"));
    let signal = AbortSignal::new();
    let aborter = {
        let signal = signal.clone();
        let ready = marker.with_extension("ready");
        std::thread::spawn(move || {
            let mut line = String::new();
            BufReader::new(std::fs::File::open(ready).unwrap())
                .read_line(&mut line)
                .unwrap();
            signal.abort("test abort");
        })
    };
    let error = fixture
        .run_with(
            &["hang", marker.to_str().unwrap()],
            None,
            Some(5_000),
            Some(&signal),
        )
        .unwrap_err();
    aborter.join().unwrap();
    assert_eq!(error.code, SgRunnerErrorCode::Aborted);
    assert_eq!(
        std::fs::read_to_string(marker.with_extension("term")).unwrap(),
        "SIGTERM"
    );
    assert!(!process_alive(&marker));
}

#[test]
fn whole_call_budget_expiry_terminates_hung_child() {
    let fixture = Fixture::new();
    let marker = fixture.marker("timeout-child");
    let error = fixture
        .run_with(&["hang", marker.to_str().unwrap()], None, Some(500), None)
        .unwrap_err();
    assert_eq!(error.code, SgRunnerErrorCode::Timeout);
    assert_eq!(
        std::fs::read_to_string(marker.with_extension("term")).unwrap(),
        "SIGTERM"
    );
    assert!(!process_alive(&marker));
}

#[test]
fn missing_binary_is_a_clear_error_not_a_panic() {
    let error = spawn_sg_runner(SgRunnerInput {
        sg_path: "/definitely/missing/sg",
        args: vec![],
        workdir: ".",
        env: None,
        max_matches: None,
        timeout_ms: None,
        signal: None,
    })
    .unwrap_err();
    assert_eq!(error.code, SgRunnerErrorCode::SgFailed);
    assert!(!error.message.is_empty());
    assert_eq!(json!(error.code.as_str()), json!("SG_FAILED"));
}
