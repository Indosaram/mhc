// SPDX-License-Identifier: MIT

export const DEFAULT_MODEL = "mahoquot/claude-sonnet-5-5";
export const DEFAULT_RUNS = 3;
export const DEFAULT_MAX_INPUT_TOKENS = 400_000;
export const DEFAULT_MAX_WALL_MS = 20 * 60 * 1000;
export const DEFAULT_MAX_TOTAL_INPUT_TOKENS = 12_000_000;

export function printHelp() {
  console.log(`Usage: node run-scenarios.mjs [options]

Options:
  --bin <path>                    Path to mhc executable (required for live runs)
  --model <model>                 Model ID (default: ${DEFAULT_MODEL})
  --runs <n>                      Number of runs per scenario (default: ${DEFAULT_RUNS})
  --out <dir>                     Directory to save run results (default: ./scenarios-output)
  --max-input-tokens <n>          Per-run input token ceiling (default: ${DEFAULT_MAX_INPUT_TOKENS})
  --max-total-input-tokens <n>    Total invocation input token ceiling (default: ${DEFAULT_MAX_TOTAL_INPUT_TOKENS})
  --max-wall-ms <n>               Per-run wall-clock cap in ms (default: ${DEFAULT_MAX_WALL_MS}); exceeded runs are "timed-out"
  --scenarios <ids>               Comma-separated scenario IDs to run (e.g. S1,S2; default: all)
  --dry-run                       Validate arguments and plan execution without spawning runs
  --help, -h                      Print this help text and exit 0
`);
}

/**
 * Checks if a bash or eval command is a sleep-only call without real work.
 * @param {string} toolName
 * @param {Record<string, unknown>|undefined} input
 * @returns {boolean}
 */
export function isSleepOnlyCall(toolName, input) {
  if (!input) return false;
  if (toolName === "bash") {
    const cmd = String(input.command || "").trim();
    return /^sleep\s+\d+(\.\d+)?\s*(;|&&)?\s*$/.test(cmd);
  }
  if (toolName === "eval") {
    const code = String(input.code || "").trim();
    return /^(await\s+)?(sleep|delay)\(\d+\)\s*;?\s*$/.test(code);
  }
  return false;
}

/**
 * Parses command line arguments.
 * @param {string[]} argv
 * @returns {Record<string, any>}
 */
export function parseArgs(argv) {
  const args = {
    bin: null,
    model: DEFAULT_MODEL,
    runs: DEFAULT_RUNS,
    out: "./scenarios-output",
    maxInputTokens: DEFAULT_MAX_INPUT_TOKENS,
    maxTotalInputTokens: DEFAULT_MAX_TOTAL_INPUT_TOKENS,
    maxWallMs: DEFAULT_MAX_WALL_MS,
    scenarios: null,
    dryRun: false,
    help: false,
  };

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--help" || arg === "-h") {
      args.help = true;
    } else if (arg === "--dry-run") {
      args.dryRun = true;
    } else if (arg === "--bin" && i + 1 < argv.length) {
      args.bin = argv[++i];
    } else if (arg === "--model" && i + 1 < argv.length) {
      args.model = argv[++i];
    } else if (arg === "--runs" && i + 1 < argv.length) {
      args.runs = parseInt(argv[++i], 10);
    } else if (arg === "--out" && i + 1 < argv.length) {
      args.out = argv[++i];
    } else if (arg === "--max-input-tokens" && i + 1 < argv.length) {
      args.maxInputTokens = parseInt(argv[++i], 10);
    } else if (arg === "--max-total-input-tokens" && i + 1 < argv.length) {
      args.maxTotalInputTokens = parseInt(argv[++i], 10);
    } else if (arg === "--max-wall-ms" && i + 1 < argv.length) {
      args.maxWallMs = parseInt(argv[++i], 10);
    } else if (arg === "--scenarios" && i + 1 < argv.length) {
      args.scenarios = argv[++i].split(",").map((s) => s.trim());
    } else {
      throw new Error(`Unknown option: ${arg}`);
    }
  }

  return args;
}
