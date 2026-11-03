// SPDX-License-Identifier: MIT

import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { spawn } from "node:child_process";
import readline from "node:readline";
import { SCENARIOS } from "./scenarios/index.mjs";
import { createFixtureRepo, checkFixtureTest } from "./scenarios/fixture-generator.mjs";
import { createWaveTracker } from "./wave-tracker.mjs";
import { parseArgs, printHelp, isSleepOnlyCall, DEFAULT_MAX_WALL_MS } from "./runner-helpers.mjs";

export { parseArgs, printHelp, isSleepOnlyCall };

/**
 * Executes a single scenario run.
 * @param {object} options
 * @returns {Promise<object>}
 */
export async function executeScenarioRun({
  bin,
  model,
  scenario,
  runIndex,
  fixtureDir,
  maxInputTokens,
  outDir,
  maxWallMs = DEFAULT_MAX_WALL_MS,
}) {
  const toolCallNames = new Set();
  let inputTokens = 0;
  let outputTokens = 0;
  let sleepOnlyUsed = false;
  let createGoalCalled = false;
  const tracker = createWaveTracker();
  const referencesRead = new Set();
  let reportToolsRan = false;
  let overBudget = false;
  let timedOut = false;
  let finalText = "";
  const toolArgs = [];

  // `-p` ends the parent turn at its first wait for children; rpc mode stays alive until we close stdin.
  const headlessWait = scenario.waitForChildren === true;
  const childArgs = headlessWait
    ? ["--mode", "rpc", "--model", model]
    : ["-p", "--mode", "json", "--model", model, scenario.prompt];

  return new Promise((resolve) => {
    const child = spawn(bin, childArgs, {
      cwd: fixtureDir,
      env: { ...process.env, RUST_LOG: "warn" },
      stdio: [headlessWait ? "pipe" : "ignore", "pipe", "pipe"],
    });
    if (headlessWait) {
      child.stdin.write(`${JSON.stringify({ type: "prompt", message: scenario.prompt })}\n`);
    }

    const wallTimer = setTimeout(() => {
      timedOut = true;
      child.kill("SIGTERM");
      setTimeout(() => child.kill("SIGKILL"), 2000).unref();
    }, maxWallMs);

    const rl = readline.createInterface({ input: child.stdout, crlfDelay: Infinity });

    rl.on("line", (line) => {
      const trimmed = line.trim();
      if (!trimmed) return;

      try {
        const event = JSON.parse(trimmed);
        if (headlessWait) {
          tracker.observe(event, trimmed);
          if (tracker.isSettled()) child.stdin.end();
        }

        if (event.type === "message_end" && event.message?.role === "assistant" && event.message.usage) {
          const u = event.message.usage;
          inputTokens += (u.input || 0) + (u.cacheRead || 0) + (u.cacheWrite || 0);
          outputTokens += u.output || 0;
          const txt = (event.message.content || []).filter((c) => c.type === "text").map((c) => c.text).join("\n");
          if (txt) finalText = txt;
        } else if (event.usage) {
          if (typeof event.usage.input === "number") inputTokens += event.usage.input;
          if (typeof event.usage.output === "number") outputTokens += event.usage.output;
        } else if (event.stats) {
          if (typeof event.stats.inputTokens === "number") inputTokens += event.stats.inputTokens;
          if (typeof event.stats.outputTokens === "number") outputTokens += event.stats.outputTokens;
        }

        if (inputTokens > maxInputTokens && !overBudget) {
          overBudget = true;
          child.kill("SIGTERM");
          setTimeout(() => child.kill("SIGKILL"), 2000);
          return;
        }

        if (event.type === "tool_execution_start" && event.args) {
          event.input = event.args;
          toolArgs.push({ name: event.toolName, args: JSON.stringify(event.args).slice(0, 2000) });
        }
        if (event.type === "tool_call" || event.type === "tool_use" || event.type === "tool_execution_start") {
          const name = event.toolName || event.name;
          if (name) {
            toolCallNames.add(name);
            if (name === "create_goal") createGoalCalled = true;
            if (isSleepOnlyCall(name, event.input || event.parameters)) sleepOnlyUsed = true;
            if (name === "read" && typeof event.input?.path === "string") {
              const p = event.input.path;
              if (p.includes("ulw-research/references/")) {
                referencesRead.add(p);
              }
            }
            if (
              (name === "bash" && String(event.input?.command || "").includes("report-tools.mjs")) ||
              (name === "eval" && String(event.input?.code || "").includes("report-tools.mjs"))
            ) {
              reportToolsRan = true;
            }
          }
        }

        if (event.type === "message" && event.message?.role === "toolResult") {
          const name = event.message.toolName;
          if (name) toolCallNames.add(name);
        }
      } catch (e) {
        // Line wasn't json, ignore
      }
    });

    child.on("close", (exitCode) => {
      clearTimeout(wallTimer);
      const testResult = checkFixtureTest(fixtureDir);
      
      // Determine if scenario met expected outcome
      let outcomeMet = true;
      if (scenario.expectedOutcome) {
        // Only expectations the scenario defines are checked here; the fixture test
        // result matters only when the scenario sets fixtureTestPass.
        outcomeMet = exitCode === 0;
        if (scenario.expectedOutcome.fixtureTestPass) {
          outcomeMet = outcomeMet && testResult.pass === true;
        }
      }

      const record = {
        scenarioId: scenario.id,
        scenarioName: scenario.name,
        runIndex,
        exitCode: exitCode ?? -1,
        inputTokens,
        outputTokens,
        toolCallNames: Array.from(toolCallNames),
        createGoalCalled,
        sleepOnlyUsed,
        ...tracker.summary(),
        finalText,
        toolArgs,
        referencesRead: Array.from(referencesRead),
        reportToolsRan,
        fixtureTestResult: testResult,
        outcomeMet,
        status: overBudget ? "over-budget" : timedOut ? "timed-out" : (outcomeMet ? "completed" : "failed"),
      };

      const outPath = path.join(outDir, `${scenario.id}-run${runIndex}.json`);
      fs.mkdirSync(outDir, { recursive: true });
      fs.writeFileSync(outPath, JSON.stringify(record, null, 2), "utf8");

      resolve(record);
    });
  });
}

/**
 * Main scenario runner.
 */
export async function runScenarios(options) {
  const selectedScenarios = options.scenarios
    ? SCENARIOS.filter((s) => options.scenarios.includes(s.id))
    : SCENARIOS;

  if (options.dryRun) {
    console.log(`DRY RUN: planned ${selectedScenarios.length} scenarios, ${options.runs} runs each.`);
    for (const sc of selectedScenarios) {
      console.log(`  [${sc.id}] ${sc.name} - prompt: "${sc.prompt}"`);
    }
    return { dryRun: true, plannedRuns: selectedScenarios.length * options.runs };
  }

  if (!options.bin || !fs.existsSync(options.bin)) {
    throw new Error(`mhc binary not found at: ${options.bin}`);
  }

  fs.mkdirSync(options.out, { recursive: true });

  const allResults = [];
  let totalInputTokens = 0;

  for (const scenario of selectedScenarios) {
    for (let r = 1; r <= options.runs; r++) {
      if (totalInputTokens >= options.maxTotalInputTokens) {
        console.warn(`Total input token limit (${options.maxTotalInputTokens}) reached. Aborting further runs.`);
        const record = {
          scenarioId: scenario.id,
          scenarioName: scenario.name,
          runIndex: r,
          status: "over-budget",
          inputTokens: 0,
          outputTokens: 0,
        };
        allResults.push(record);
        continue;
      }

      const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), `ulw-bench-${scenario.id}-r${r}-`));
      try {
        createFixtureRepo(tempDir);
        const result = await executeScenarioRun({
          bin: options.bin,
          model: options.model,
          scenario,
          runIndex: r,
          fixtureDir: tempDir,
          maxInputTokens: options.maxInputTokens,
          maxWallMs: options.maxWallMs,
          outDir: options.out,
        });

        totalInputTokens += result.inputTokens;
        allResults.push(result);
      } finally {
        try {
          fs.rmSync(tempDir, { recursive: true, force: true });
        } catch (e) {
          // Cleanup best effort
        }
      }
    }
  }

  const summary = {
    totalRuns: allResults.length,
    completed: allResults.filter((r) => r.status === "completed").length,
    overBudget: allResults.filter((r) => r.status === "over-budget").length,
    timedOut: allResults.filter((r) => r.status === "timed-out").length,
    failed: allResults.filter((r) => r.status === "failed").length,
    totalInputTokens,
    results: allResults,
  };

  fs.writeFileSync(path.join(options.out, "summary.json"), JSON.stringify(summary, null, 2), "utf8");
  return summary;
}

async function main() {
  const parsed = parseArgs(process.argv.slice(2));
  if (parsed.help) {
    printHelp();
    process.exit(0);
  }

  try {
    const summary = await runScenarios(parsed);
    if (!parsed.dryRun) {
      console.log(`Scenario run complete: ${summary.completed}/${summary.totalRuns} completed.`);
      if (summary.failed > 0) {
        console.error(`Error: ${summary.failed} scenario runs failed expectations.`);
        process.exit(1);
      }
    }
  } catch (err) {
    console.error(`Error: ${err.message}`);
    process.exit(1);
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === path.resolve(import.meta.filename || "")) {
  main();
}
