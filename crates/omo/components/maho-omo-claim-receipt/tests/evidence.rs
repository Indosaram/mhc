//! Port of `claim-receipt/evidence.test.ts`.

use crate::support::{
    custom_message, edit_result, eval_run, host_bash, tool_call, tool_result, CellFixture,
};
use maho_omo_claim_receipt::evidence::build_timeline;
use serde_json::{json, Value};

fn checks(entries: &[Value]) -> Vec<(&'static str, &'static str, i64)> {
    build_timeline(entries)
        .checks
        .iter()
        .map(|check| {
            (
                check.kind.as_str(),
                check.outcome.as_str(),
                check.at.floor() as i64,
            )
        })
        .collect()
}

fn bash(id: &str, command: &str, text: &str, is_error: bool) -> Vec<Value> {
    vec![
        tool_call(id, "bash", json!({ "command": command })),
        tool_result(id, "bash", text, json!({}), is_error),
    ]
}

fn start(command: &str, id: &str) -> Vec<Value> {
    eval_run(
        "c1",
        &CellFixture {
            code: "await tool.bash({ run_in_background: true })",
            output: "",
            status: Some("complete"),
        },
        vec![host_bash(
            command,
            &format!("Command running in background with ID: {id}"),
            true,
            json!({ "run_in_background": true }),
        )],
    )
}

#[test]
fn an_eval_host_bash_call_reads_its_outcome_from_ok_and_the_result_preview() {
    let passed = eval_run(
        "c1",
        &CellFixture {
            code: "await tool.bash({ command: 'bun test' })",
            output: "",
            status: None,
        },
        vec![host_bash("bun test src", " 12 pass\n 0 fail", true, json!({}))],
    );
    let failed = eval_run(
        "c2",
        &CellFixture {
            code: "await tool.bash({ command: 'tsgo --noEmit' })",
            output: "",
            status: None,
        },
        vec![host_bash(
            "tsgo --noEmit",
            "error TS2322: nope",
            false,
            json!({}),
        )],
    );
    assert_eq!(checks(&passed), vec![("test", "passed", 1)]);
    assert_eq!(checks(&failed), vec![("typecheck", "failed", 1)]);
}

#[test]
fn a_cell_can_support_a_claim_but_never_contradict_one() {
    let cell = |code: &str, output: &str, status: &'static str| {
        eval_run(
            "c",
            &CellFixture {
                code,
                output,
                status: Some(status),
            },
            Vec::new(),
        )
    };
    assert_eq!(
        checks(&cell("await sh(`bun test x`)", " 35 pass\n 0 fail", "complete")),
        vec![("test", "passed", 1)]
    );
    assert_eq!(
        checks(&cell("await sh(`bun test x`)", "boom", "error")),
        vec![("test", "unknown", 1)]
    );
    assert_eq!(
        checks(&cell("await sh(`bun test x`)", " 3 pass\n 1 fail", "complete")),
        vec![("test", "unknown", 1)]
    );
    assert_eq!(
        checks(&cell("await sh(`bun test x`)", "still going", "complete")),
        vec![("test", "unknown", 1)]
    );
    assert_eq!(
        checks(&cell("await sh(`ls`)", "a\nb", "complete")),
        Vec::new()
    );
}

#[test]
fn a_deliberate_mutation_probe_that_fails_is_not_a_failed_check() {
    let probe = eval_run(
        "m1",
        &CellFixture {
            code: "const mut = orig.replace(a, b)\nawait sh(`bun test x`)",
            output: " 3 pass\n 1 fail",
            status: Some("complete"),
        },
        Vec::new(),
    );
    assert_eq!(checks(&probe), vec![("test", "unknown", 1)]);
}

#[test]
fn a_detached_cell_that_later_fails_stays_unknown() {
    let mut entries = eval_run(
        "call_8",
        &CellFixture {
            code: "await sh(`bun test x`)",
            output: "",
            status: Some("detached"),
        },
        Vec::new(),
    );
    entries.push(custom_message(
        "senpi-codemode:notification",
        "<system-reminder>Detached eval cell call_8 (py) failed.\n 1 fail</system-reminder>",
        json!({}),
    ));
    assert_eq!(
        checks(&entries),
        vec![("test", "unknown", 1), ("test", "unknown", 2)]
    );
}

#[test]
fn vitest_summaries_and_vp_test_runs_are_read() {
    let vitest = eval_run(
        "vt",
        &CellFixture {
            code: "await sh(`vp run --filter server test`)",
            output: " Test Files  3 passed (3)\n      Tests  8 passed (8)",
            status: None,
        },
        Vec::new(),
    );
    let red = eval_run(
        "vr",
        &CellFixture {
            code: "await tool.bash({ command: 'vp test' })",
            output: "",
            status: None,
        },
        vec![host_bash(
            "vp test x",
            "      Tests  1 failed | 12 passed (13)",
            true,
            json!({}),
        )],
    );
    assert_eq!(checks(&vitest), vec![("test", "passed", 1)]);
    assert_eq!(checks(&red), vec![("test", "failed", 1)]);
}

#[test]
fn ci_status_and_combined_check_commands_are_recognised() {
    let ci = eval_run(
        "g1",
        &CellFixture {
            code: "await tool.bash({ command: 'gh pr checks 12' })",
            output: "",
            status: None,
        },
        vec![host_bash(
            "gh pr checks 12",
            "build\tpass\t1m\thttps://x\ntest\tpass\t2m\thttps://y",
            true,
            json!({}),
        )],
    );
    let pending = eval_run(
        "g2",
        &CellFixture {
            code: "await tool.bash({ command: 'gh pr checks 12' })",
            output: "",
            status: None,
        },
        vec![host_bash(
            "gh pr checks 12",
            "build\tpass\t1m\thttps://x\ntest\tpending\t0\thttps://y",
            true,
            json!({}),
        )],
    );
    let vp = eval_run(
        "v1",
        &CellFixture {
            code: "await sh(`vp check`)",
            output: "Found 0 errors and 3 warnings",
            status: None,
        },
        Vec::new(),
    );
    assert_eq!(checks(&ci), vec![("test", "passed", 1)]);
    assert_eq!(checks(&pending), vec![("test", "unknown", 1)]);
    assert_eq!(
        checks(&vp),
        vec![("typecheck", "passed", 1), ("lint", "passed", 1)]
    );
}

#[test]
fn a_detached_cell_resolves_from_its_completion_notification() {
    let mut entries = eval_run(
        "call_7",
        &CellFixture {
            code: "await sh(`bun run typecheck`)",
            output: "",
            status: Some("detached"),
        },
        Vec::new(),
    );
    entries.push(custom_message(
        "senpi-codemode:notification",
        "<system-reminder>Detached eval cell call_7 (js) completed.\nTYPECHECK_EXIT=0</system-reminder>",
        json!({}),
    ));
    assert_eq!(
        checks(&entries),
        vec![("typecheck", "unknown", 1), ("typecheck", "passed", 2)]
    );
}

#[test]
fn the_matching_terminal_notification_decides_the_outcome_at_its_own_position() {
    let ok = [
        start("bun test", "bash_2"),
        vec![custom_message(
            "senpi-terminal:notification",
            "<system-reminder>Background terminal session bash_2 finished: completed (exit code 0).</system-reminder>",
            json!({}),
        )],
    ]
    .concat();
    let bad = [
        start("bun test", "bash_2"),
        vec![custom_message(
            "senpi-terminal:notification",
            "<system-reminder>Background terminal session bash_2 finished: completed (exit code 1).</system-reminder>",
            json!({}),
        )],
    ]
    .concat();
    let killed = [
        start("bun test", "bash_2"),
        vec![custom_message(
            "senpi-terminal:notification",
            "<system-reminder>Background terminal session bash_2 finished: killed (exit code 0).</system-reminder>",
            json!({}),
        )],
    ]
    .concat();
    let other = [
        start("bun test", "bash_2"),
        vec![custom_message(
            "senpi-terminal:notification",
            "<system-reminder>Background terminal session bash_9 finished: completed (exit code 0).</system-reminder>",
            json!({}),
        )],
    ]
    .concat();
    assert_eq!(checks(&ok), vec![("test", "passed", 2)]);
    assert_eq!(checks(&bad), vec![("test", "failed", 2)]);
    assert_eq!(checks(&killed), Vec::new());
    assert_eq!(checks(&other), Vec::new());
}

#[test]
fn a_monitor_that_failed_to_start_or_timed_out_is_not_a_failed_check() {
    let refused = eval_run(
        "c9",
        &CellFixture {
            code: "await tool.monitor(spec)",
            output: "",
            status: None,
        },
        vec![json!({ "name": "monitor", "ok": false, "args": { "command": "bun test" }, "resultPreview": "" })],
    );
    let timed_out = [
        start("bun test", "bash_7"),
        vec![custom_message(
            "senpi-monitor:notification",
            "<system-reminder>Monitor event(suites): watcher timed_out (exit code 1)</system-reminder>",
            json!({ "monitors": [{ "id": "bash_7" }] }),
        )],
    ]
    .concat();
    let test_timeout = [
        start("bun test", "bash_8"),
        vec![custom_message(
            "senpi-monitor:notification",
            "<system-reminder>Monitor event(t): (fail) x > y [5001ms] timed out after 5000ms\n 3 pass\n 1 fail\nwatcher completed (exit code 1)</system-reminder>",
            json!({ "monitors": [{ "id": "bash_8" }] }),
        )],
    ]
    .concat();
    assert_eq!(checks(&refused), Vec::new());
    assert_eq!(checks(&timed_out), Vec::new());
    assert_eq!(checks(&test_timeout), vec![("test", "failed", 2)]);
}

#[test]
fn a_monitor_event_for_that_id_decides_it_including_one_bundled_inside_a_wake() {
    let content = "<system-reminder>Monitor event(run tests): TEST_EXIT=0\nwatcher completed (exit code 0)</system-reminder>";
    let direct = [
        start("bun test", "bash_3"),
        vec![custom_message(
            "senpi-monitor:notification",
            content,
            json!({ "monitors": [{ "id": "bash_3" }] }),
        )],
    ]
    .concat();
    let wake_content = format!("prefix\n{content}");
    let wake = [
        start("bun test", "bash_3"),
        vec![custom_message(
            "omo-senpi:wake",
            &wake_content,
            json!([{
                "customType": "senpi-monitor:notification",
                "details": { "monitors": [{ "id": "bash_3" }] },
                "contentRange": [7, 7 + content.len()]
            }]),
        )],
    ]
    .concat();
    assert_eq!(checks(&direct), vec![("test", "passed", 2)]);
    assert_eq!(checks(&wake), vec![("test", "passed", 2)]);
}

#[test]
fn key_value_summaries_are_read_by_their_value_never_by_the_number_before_them() {
    let settle = |line: &str| {
        [
            start("while true; do o=$(gh pr checks 9); done", "bash_4"),
            vec![custom_message(
                "senpi-monitor:notification",
                &format!(
                    "<system-reminder>Monitor event(CI settle): {line}\nwatcher completed (exit code 0)</system-reminder>"
                ),
                json!({ "monitors": [{ "id": "bash_4" }] }),
            )],
        ]
        .concat()
    };
    assert_eq!(checks(&settle("CI_SETTLED total=46 fail=0")), vec![("test", "passed", 2)]);
    assert_eq!(
        checks(&settle("CI_SETTLED #1197 fail=[] #1198 fail=[]")),
        vec![("test", "passed", 2)]
    );
    assert_eq!(checks(&settle("CI_SETTLED total=46 fail=2")), vec![("test", "failed", 2)]);
    assert_eq!(
        checks(&settle("CI_SETTLED #1197 fail=[lint] #1198 fail=[]")),
        vec![("test", "failed", 2)]
    );
}

#[test]
fn echo_grep_rg_and_git_messages_are_not_check_runs() {
    assert_eq!(checks(&bash("m1", "echo bun test", "bun test", false)), Vec::new());
    assert_eq!(
        checks(&bash("m2", "grep -rn vitest packages/", "a.ts: FAIL vitest", false)),
        vec![("lookup", "passed", 1)]
    );
    assert_eq!(
        checks(&bash("m3", "rg -n \"bun test\" docs/", "docs/x.md: FAIL: 2 fail", false)),
        vec![("lookup", "passed", 1)]
    );
    assert_eq!(
        checks(&bash("m4", "git commit -m \"fix lint and tsc\"", "[main 1] fix lint", false)),
        Vec::new()
    );
    let host = eval_run(
        "m5",
        &CellFixture {
            code: "await tool.bash({ command: 'echo bun test' })",
            output: "",
            status: None,
        },
        vec![host_bash("echo bun test && grep vitest x", " 1 fail", true, json!({}))],
    );
    assert_eq!(checks(&host), vec![("lookup", "passed", 1)]);
}

#[test]
fn real_invocations_behind_cd_env_timeout_bash_c_and_heavy_slot_still_count() {
    assert_eq!(
        checks(&bash("r1", "cd pkg && CI=1 timeout -k 10 300 bun test src", " 3 pass\n 0 fail", false)),
        vec![("test", "passed", 1)]
    );
    assert_eq!(
        checks(&bash("r2", "bash -c 'bunx tsgo --noEmit -p x'", "", true)),
        vec![("typecheck", "failed", 1)]
    );
    assert_eq!(
        checks(&bash(
            "r3",
            "HEAVY_BUDGET=600 ~/.omo/omocat/heavy-slot.sh lab -- bun run test:senpi",
            " 9 pass\n 0 fail",
            false
        )),
        vec![("test", "passed", 1)]
    );
    assert_eq!(
        checks(&bash("r4", "npx vitest run && pnpm run lint", "Found 0 errors", false)),
        vec![("test", "passed", 1), ("lint", "passed", 1)]
    );
}

#[test]
fn runners_task_tools_scripts_and_wrapper_flags_still_reach_the_real_check() {
    let kinds = |command: &str| -> Vec<&'static str> {
        checks(&bash("w", command, " 3 pass\n 0 fail", false))
            .into_iter()
            .map(|(kind, _, _)| kind)
            .collect()
    };
    assert_eq!(kinds("uv run pytest tests/"), vec!["test"]);
    assert_eq!(kinds("python -m pytest -q"), vec!["test"]);
    assert_eq!(kinds("poetry run pytest"), vec!["test"]);
    assert_eq!(kinds("python3 -m mypy src"), vec!["typecheck"]);
    assert_eq!(kinds("make test"), vec!["test"]);
    assert_eq!(kinds("just lint"), vec!["lint"]);
    assert_eq!(kinds("bun run scripts/qa/claim-receipt-e2e.mjs out"), vec!["run"]);
    assert_eq!(kinds("bash packages/x/scripts/qa/run.sh"), vec!["run"]);
    assert_eq!(kinds("./scripts/qa/run.sh"), vec!["run"]);
    assert_eq!(kinds("env -i HOME=/h PATH=/p bun test x"), vec!["test"]);
    assert_eq!(kinds("time -p bun test x"), vec!["test"]);
}

#[test]
fn heredoc_bodies_are_file_contents_not_commands() {
    assert_eq!(
        checks(&bash("h", "cat > run.sh <<'EOF'\nbun test x\nEOF", "", false)),
        Vec::new()
    );
}

#[test]
fn an_eval_cell_counts_only_shell_strings_that_invoke_a_check() {
    let cell = |code: &str, output: &str| {
        eval_run(
            "e",
            &CellFixture {
                code,
                output,
                status: None,
            },
            Vec::new(),
        )
    };
    assert_eq!(
        checks(&cell("print(\"run bun test later\")", " 3 pass\n 0 fail")),
        Vec::new()
    );
    assert_eq!(
        checks(&cell("await sh(`grep -c vitest log.txt`)", " 3 pass\n 0 fail")),
        Vec::new()
    );
    assert_eq!(
        checks(&cell(
            "log(\"bun test passed\")\nconst label = \"tsc --noEmit\"",
            " 3 pass\n 0 fail"
        )),
        Vec::new()
    );
    assert_eq!(
        checks(&cell(
            "r = subprocess.run(['bash','-c','bun test x'])",
            " 3 pass\n 0 fail"
        )),
        vec![("test", "passed", 1)]
    );
    assert_eq!(
        checks(&cell("await tool.bash({ command: 'bun test x' })", " 3 pass\n 0 fail")),
        vec![("test", "passed", 1)]
    );
    assert_eq!(
        checks(&cell(
            "print(await q(`gh pr checks 213 --repo o/r 2>&1 | head`))",
            "lint\tpass\t1m\te2e\tpass\t2m"
        )),
        vec![("test", "passed", 1)]
    );
    assert_eq!(
        checks(&cell(
            "const p = Bun.spawn(['gh', 'pr', 'checks', String(n)], { stdout: 'pipe' })",
            "lint\tpass\t1m"
        )),
        vec![("test", "passed", 1)]
    );
    let looped: Vec<&'static str> = checks(&cell(
        "for c in [\"bun test packages/x\", \"tsgo --noEmit\"]:\n    sh(c)",
        " 3 pass\n 0 fail",
    ))
    .into_iter()
    .map(|(kind, _, _)| kind)
    .collect();
    assert_eq!(looped, vec!["test", "typecheck"]);
    assert_eq!(
        checks(&cell(
            "tool.edit({ edits: [{ newText: \"bun test packages/x\" }] })\nPath(p).write_text(\"bun test x\")\nexpect(argv).toEqual([\"bun\", \"test\"])",
            " 3 pass\n 0 fail"
        )),
        Vec::new()
    );
    assert_eq!(
        checks(&cell(
            "render(\"bun test x\")\nconst hint = done ? \"tsc --noEmit\" : \"\"",
            " 3 pass\n 0 fail"
        )),
        Vec::new()
    );
    let runners = "await sh2(`bun test a`)\nrd(\"vp check\")\nos.system(\"tsgo --noEmit\")\nconst cmd = \"bun run build\"\nawait tool.monitor({ command: `bun run lint` })";
    let mut runner_kinds: Vec<&'static str> = checks(&cell(runners, " 3 pass\n 0 fail"))
        .into_iter()
        .map(|(kind, _, _)| kind)
        .collect();
    runner_kinds.sort_unstable();
    assert_eq!(runner_kinds, vec!["build", "lint", "test", "typecheck"]);
    let text_names = "summary = \"bun test x passed\"\nstatus: \"tsgo --noEmit ok\"\nshow(\"bun test passed\")\nruntime(\"bun test x\")\nx.should(\"bun test x\")\ncommand_output = \"bun test x\"\nf([\"a\"], \"bun test x\")";
    assert_eq!(checks(&cell(text_names, " 3 pass\n 0 fail")), Vec::new());
    let argv_with_message = "subprocess.run([\"git\", \"commit\", \"-m\", \"bun test x green\"])\nsh([\"gh\", \"pr\", \"comment\", \"1\", \"-b\", \"tsgo --noEmit passes\"])";
    assert_eq!(checks(&cell(argv_with_message, " 3 pass\n 0 fail")), Vec::new());
    let names: Vec<&'static str> = checks(&cell(
        "s = f\"bun test {x}\"\ncmds = [\"bun test a\"]\nsh(cmds[0])",
        " 3 pass\n 0 fail",
    ))
    .into_iter()
    .map(|(kind, _, _)| kind)
    .collect();
    assert_eq!(names, vec!["test"]);
    assert_eq!(
        checks(&cell(
            "write(f'{R}/run.sh', 'bun test x')\nopen(f, 'w').write('tsc --noEmit')\nassert 'bun test' in s",
            " 3 pass\n 0 fail"
        )),
        Vec::new()
    );
    assert_eq!(
        checks(&cell("await sh(`cd ${WT} && bun test x`)", " 3 pass\n 0 fail")),
        vec![("test", "passed", 1)]
    );
    let python = "r = subprocess.run(f\"\"\"cd {W0} &&\n CI=1 bun test packages/x 2>&1 | tail -3\"\"\", shell=True)";
    assert_eq!(checks(&cell(python, " 3 pass\n 0 fail")), vec![("test", "passed", 1)]);
}

#[test]
fn read_grep_and_symbol_lookups_are_recorded_as_passed_lookups() {
    let read = vec![
        tool_call("k1", "read", json!({ "path": "src/a.ts" })),
        tool_result("k1", "read", "export const x = 1", json!({}), false),
    ];
    let grep = vec![
        tool_call("k2", "bash", json!({ "command": "rg -n flag src/config.ts" })),
        tool_result("k2", "bash", "12: flag", json!({}), false),
    ];
    let missing = vec![
        tool_call("k3", "read", json!({ "path": "src/b.ts" })),
        tool_result("k3", "read", "ENOENT", json!({}), true),
    ];
    assert_eq!(checks(&read), vec![("lookup", "passed", 1)]);
    assert_eq!(checks(&grep), vec![("lookup", "passed", 1)]);
    assert_eq!(checks(&missing), Vec::new());
}

#[test]
fn a_check_the_harness_refused_before_it_ran_is_not_a_failed_check() {
    let missing = vec![
        tool_call("b1", "bash", json!({ "command": "bun test" })),
        tool_result(
            "b1",
            "bash",
            "Tool bash not found. Run bash inside an eval cell.",
            json!({}),
            true,
        ),
    ];
    let denied = eval_run(
        "b2",
        &CellFixture {
            code: "await tool.bash({ command: 'bun test' })",
            output: "",
            status: None,
        },
        vec![host_bash(
            "bun test",
            "Permission denied by policy: bash",
            false,
            json!({}),
        )],
    );
    let ran = vec![
        tool_call("b3", "bash", json!({ "command": "bun test" })),
        tool_result("b3", "bash", " 2 pass\n 1 fail", json!({}), true),
    ];
    assert_eq!(checks(&missing), vec![("test", "unknown", 1)]);
    assert_eq!(checks(&denied), vec![("test", "unknown", 1)]);
    assert_eq!(checks(&ran), vec![("test", "failed", 1)]);
}

#[test]
fn a_clean_lsp_report_is_a_passed_lsp_check() {
    let entries = vec![
        tool_call("l1", "lsp_diagnostics", json!({ "filePath": "src" })),
        tool_result(
            "l1",
            "lsp_diagnostics",
            "Files scanned: 6\nFiles with errors: 0\nTotal diagnostics: 0",
            json!({}),
            false,
        ),
    ];
    assert_eq!(checks(&entries), vec![("lsp", "passed", 1)]);
}

#[test]
fn code_edits_are_relevant_and_notes_or_evidence_edits_are_not() {
    let timeline = build_timeline(
        &[
            edit_result("e1", "src/a.ts"),
            edit_result("e2", ".omo/evidence/run/summary.md"),
            edit_result("e3", "/Users/Shared/omo-runs/lane/rerun.sh"),
        ]
        .concat(),
    );
    let edits: Vec<(Vec<String>, bool)> = timeline
        .edits
        .iter()
        .map(|edit| (edit.paths.clone(), edit.relevant))
        .collect();
    assert_eq!(
        edits,
        vec![
            (vec!["src/a.ts".to_owned()], true),
            (vec![".omo/evidence/run/summary.md".to_owned()], false),
            (vec!["/Users/Shared/omo-runs/lane/rerun.sh".to_owned()], false),
        ]
    );
}

#[test]
fn writes_and_git_commands_inside_eval_cells_are_not_edits() {
    let edits = |code: &str| {
        eval_run(
            "w",
            &CellFixture {
                code,
                output: " 3 pass\n 0 fail",
                status: None,
            },
            Vec::new(),
        )
    };
    assert!(build_timeline(&edits(
        "await Bun.write(`${W}/zzScratch.test.ts`, s)\nawait sh(`bun test`)"
    ))
    .edits
    .is_empty());
    assert!(build_timeline(&edits("open(p, 'w').write(brief)")).edits.is_empty());
    assert!(build_timeline(&edits(
        "await sh(`git merge-tree --write-tree HEAD origin/dev`)"
    ))
    .edits
    .is_empty());
}
