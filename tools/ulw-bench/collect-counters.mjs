// SPDX-License-Identifier: MIT

import fs from "node:fs";
import path from "node:path";

/**
 * Parses cargo nextest log output or counter test results.
 * @param {string} logText
 * @returns {{ contextTransformInvocations: number|null, messageClones: number|null, statusCallsConstant: boolean|null, rawMatches: string[] }}
 */
export function parseCounterLog(logText) {
  const rawMatches = [];
  let contextTransformInvocations = null;
  let messageClones = null;
  let statusCallsConstant = null;

  for (const line of logText.split("\n")) {
    // Check for context-transform zero assertions
    const ctMatch = line.match(/context[-_]transform[-_]invocations?\s*[:=]\s*(\d+)/i);
    if (ctMatch) {
      contextTransformInvocations = parseInt(ctMatch[1], 10);
      rawMatches.push(line);
    }
    // Check for message clones
    const cloneMatch = line.match(/message[-_]clones?\s*[:=]\s*(\d+)/i);
    if (cloneMatch) {
      messageClones = parseInt(cloneMatch[1], 10);
      rawMatches.push(line);
    }
    // Check for status calls constant
    if (line.includes("status_calls_constant_over_300_unchanged_results") || line.includes("status_calls_constant")) {
      statusCallsConstant = line.includes("PASS") || line.includes("ok") || !line.includes("FAILED");
      rawMatches.push(line);
    }
  }

  return {
    contextTransformInvocations,
    messageClones,
    statusCallsConstant,
    rawMatches,
  };
}

/**
 * Collects counter results from one or more log files and outputs evidence JSON.
 * @param {string[]} logPaths
 * @param {string} [outPath]
 */
export function collectCounterFiles(logPaths, outPath) {
  const results = {};
  for (const logPath of logPaths) {
    if (!fs.existsSync(logPath)) {
      continue;
    }
    const content = fs.readFileSync(logPath, "utf8");
    const parsed = parseCounterLog(content);
    results[path.basename(logPath)] = parsed;
  }

  if (outPath) {
    fs.mkdirSync(path.dirname(outPath), { recursive: true });
    fs.writeFileSync(outPath, JSON.stringify(results, null, 2), "utf8");
  }

  return results;
}

function main() {
  const args = process.argv.slice(2);
  if (args.length === 0 || args.includes("--help") || args.includes("-h")) {
    console.log("Usage: node collect-counters.mjs <log-files...> [--out <evidence.json>]");
    process.exit(0);
  }

  let outPath = null;
  const logPaths = [];
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--out" && i + 1 < args.length) {
      outPath = args[++i];
    } else {
      logPaths.push(args[i]);
    }
  }

  const collected = collectCounterFiles(logPaths, outPath);
  console.log(JSON.stringify(collected, null, 2));
}

if (process.argv[1] && path.resolve(process.argv[1]) === path.resolve(import.meta.filename || "")) {
  main();
}
