// SPDX-License-Identifier: MIT

import { test, describe, it } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { parseArgs, isSleepOnlyCall, executeScenarioRun } from "./run-scenarios.mjs";
import { SCENARIOS } from "./scenarios/index.mjs";
import { createFixtureRepo, checkFixtureTest } from "./scenarios/fixture-generator.mjs";
import { parseCounterLog } from "./collect-counters.mjs";

describe("run-scenarios", () => {
  it("defines all 7 required scenarios S1 through S7", () => {
    assert.equal(SCENARIOS.length, 7);
    const ids = SCENARIOS.map((s) => s.id);
    assert.deepEqual(ids, ["S1", "S2", "S3", "S4", "S5", "S6", "S7"]);

    for (const sc of SCENARIOS) {
      assert.ok(sc.name, `${sc.id} has name`);
      assert.ok(sc.prompt, `${sc.id} has prompt`);
      assert.ok(sc.kind, `${sc.id} has kind`);
      assert.ok(sc.expectedOutcome, `${sc.id} has expectedOutcome`);
    }

    assert.equal(SCENARIOS[0].prompt, "ulw where is fn add defined?");
    assert.equal(SCENARIOS[1].prompt, "ulw fix the failing test in math.rs");
    assert.equal(SCENARIOS[2].prompt, "ulw add a --verbose flag across cli.rs and lib.rs with tests");
    assert.equal(SCENARIOS[3].prompt, "ulw audit this repo for unused functions using parallel subagents");
    assert.equal(SCENARIOS[4].prompt, "ulw run ./slow.sh (sleeps 90s) and report its output");
    assert.equal(
      SCENARIOS[5].prompt,
      "ulw-research which sorting algorithm does the fixture's sort.rs implement and what is its worst case?"
    );
    assert.equal(
      SCENARIOS[6].prompt,
      "ulw-research write a short markdown report comparing the two parsers in the fixture"
    );
  });

  it("parses CLI arguments correctly", () => {
    const defaults = parseArgs([]);
    assert.equal(defaults.model, "mahoquot/claude-sonnet-5-5");
    assert.equal(defaults.runs, 3);
    assert.equal(defaults.maxInputTokens, 400000);
    assert.equal(defaults.maxTotalInputTokens, 12000000);
    assert.equal(defaults.dryRun, false);
    assert.equal(defaults.help, false);

    const custom = parseArgs([
      "--bin",
      "/path/to/mhc",
      "--runs",
      "5",
      "--max-input-tokens",
      "300000",
      "--dry-run",
    ]);
    assert.equal(custom.bin, "/path/to/mhc");
    assert.equal(custom.runs, 5);
    assert.equal(custom.maxInputTokens, 300000);
    assert.equal(custom.dryRun, true);
  });

  it("detects sleep-only tool calls", () => {
    assert.equal(isSleepOnlyCall("bash", { command: "sleep 90" }), true);
    assert.equal(isSleepOnlyCall("bash", { command: "sleep 10;" }), true);
    assert.equal(isSleepOnlyCall("bash", { command: "cargo test" }), false);
    assert.equal(isSleepOnlyCall("eval", { code: "await sleep(90)" }), true);
    assert.equal(isSleepOnlyCall("eval", { code: "const x = 1;" }), false);
    assert.equal(isSleepOnlyCall("read", { path: "src/main.rs" }), false);
  });

  it("generates a valid fixture repo and tests fail/pass conditions", () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "ulw-test-fixture-"));
    try {
      createFixtureRepo(tmpDir);

      assert.ok(fs.existsSync(path.join(tmpDir, "Cargo.toml")));
      assert.ok(fs.existsSync(path.join(tmpDir, "src/lib.rs")));
      assert.ok(fs.existsSync(path.join(tmpDir, "src/math.rs")));
      assert.ok(fs.existsSync(path.join(tmpDir, "src/sort.rs")));
      assert.ok(fs.existsSync(path.join(tmpDir, "src/parser_a.rs")));
      assert.ok(fs.existsSync(path.join(tmpDir, "src/parser_b.rs")));
      assert.ok(fs.existsSync(path.join(tmpDir, "src/unused.rs")));
      assert.ok(fs.existsSync(path.join(tmpDir, "slow.sh")));

      // Initial state has failing test in math.rs
      const initialCheck = checkFixtureTest(tmpDir);
      assert.equal(initialCheck.pass, false);

      // Fix the test in math.rs
      const mathPath = path.join(tmpDir, "src/math.rs");
      const content = fs.readFileSync(mathPath, "utf8");
      fs.writeFileSync(mathPath, content.replace("assert_eq!(add(2, 2), 5);", "assert_eq!(add(2, 2), 4);"), "utf8");

      const fixedCheck = checkFixtureTest(tmpDir);
      assert.equal(fixedCheck.pass, true);
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  it("parses counter log assertions", () => {
    const log = `
running 1 test
test context_transform_invocations: 0 ... ok
test message_clones: 0 ... ok
test status_calls_constant_over_300_unchanged_results ... ok
test result: ok. 3 passed; 0 failed
`;
    const parsed = parseCounterLog(log);
    assert.equal(parsed.contextTransformInvocations, 0);
    assert.equal(parsed.messageClones, 0);
    assert.equal(parsed.statusCallsConstant, true);
  });

  it("marks a scenario as passed/failed based on fixture check", async () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "ulw-test-outcome-"));
    try {
      createFixtureRepo(tmpDir);
      
      const scenario = { id: "S-TEST", name: "Test", prompt: "test", expectedOutcome: { fixtureTestPass: true } };

      // Case 1: Pass
      // Fix math.rs
      const mathPath = path.join(tmpDir, "src/math.rs");
      const content = fs.readFileSync(mathPath, "utf8");
      fs.writeFileSync(mathPath, content.replace("assert_eq!(add(2, 2), 5);", "assert_eq!(add(2, 2), 4);"), "utf8");
      
      const resultPass = await executeScenarioRun({
        bin: "true", // Mock exit
        model: "test-model",
        scenario,
        runIndex: 1,
        fixtureDir: tmpDir,
        maxInputTokens: 100,
        outDir: "/tmp",
      });
      assert.strictEqual(resultPass.outcomeMet, true);
      assert.strictEqual(resultPass.status, "completed");

      // Case 2: Fail
      // Break math.rs: remove the fix, restore the failing test
      const contentFail = fs.readFileSync(mathPath, "utf8");
      fs.writeFileSync(mathPath, contentFail.replace("assert_eq!(add(2, 2), 4);", "assert_eq!(add(2, 2), 5);"), "utf8");
      
      const resultFail = await executeScenarioRun({
        bin: "true", 
        model: "test-model",
        scenario,
        runIndex: 1,
        fixtureDir: tmpDir,
        maxInputTokens: 100,
        outDir: "/tmp",
      });
      assert.strictEqual(resultFail.outcomeMet, false);
      assert.strictEqual(resultFail.status, "failed");
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  it("ignores the fixture test when the scenario does not set fixtureTestPass", async () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "ulw-test-outcome-nofix-"));
    try {
      createFixtureRepo(tmpDir); // math.rs test still failing
      const scenario = { id: "S-READONLY", name: "Test", prompt: "test", expectedOutcome: { noGoal: true } };
      const result = await executeScenarioRun({
        bin: "true",
        model: "test-model",
        scenario,
        runIndex: 1,
        fixtureDir: tmpDir,
        maxInputTokens: 100,
        outDir: tmpDir,
      });
      assert.strictEqual(result.outcomeMet, true);
      assert.strictEqual(result.status, "completed");
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });
});
