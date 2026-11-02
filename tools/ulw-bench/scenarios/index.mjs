// SPDX-License-Identifier: MIT

/**
 * 7 ulw-bench scenario definitions for evaluating ultrawork and ulw-research workflows.
 */
export const SCENARIOS = [
  {
    id: "S1",
    name: "read-only ask",
    prompt: "ulw where is fn add defined?",
    kind: "read-only",
    description: "Read-only query: model should answer with file and line without creating goals or todo items",
    expectedOutcome: {
      noGoal: true,
      noTodo: true,
      correctFile: "src/math.rs",
      correctLine: 1,
    },
  },
  {
    id: "S2",
    name: "LIGHT fix",
    prompt: "ulw fix the failing test in math.rs",
    kind: "light-fix",
    description: "Lightweight fix: fix the failing assertion in math.rs without heavy ceremony",
    expectedOutcome: {
      fixtureTestPass: true,
      usesTodoOnly: true,
      noGoal: true,
    },
  },
  {
    id: "S3",
    name: "HEAVY multi-file",
    prompt: "ulw add a --verbose flag across cli.rs and lib.rs with tests",
    kind: "heavy-multi-file",
    description: "Heavy multi-file task: add --verbose across cli.rs and lib.rs with tests, verifying before commit",
    expectedOutcome: {
      multiFileChanged: ["src/cli.rs", "src/lib.rs"],
      fixtureTestPass: true,
      singleMergedBuild: true,
      noCommitWithoutPermission: true,
    },
  },
  {
    id: "S4",
    name: "delegated audit",
    prompt: "ulw audit this repo for unused functions using parallel subagents",
    kind: "delegated-audit",
    description: "Delegated audit using subagents: compact reports (<=3500 chars), bounded wakes (<=24000 B), falsification wave",
    expectedOutcome: {
      childReportSizeMax: 3500,
      wakeBatchMax: 24000,
      falsificationWaveObserved: true,
      identifiesUnusedFunctions: ["dead_code_helper_alpha", "dead_code_helper_beta"],
    },
  },
  {
    id: "S5",
    name: "long wait",
    prompt: "ulw run ./slow.sh (sleeps 90s) and report its output",
    kind: "long-wait",
    description: "Long-running process: subscription/event-based wait without sleep-only polling loops",
    expectedOutcome: {
      noSleepPolling: true,
      subscriptionWait: true,
    },
  },
  {
    id: "S6",
    name: "research chat answer",
    prompt: "ulw-research which sorting algorithm does the fixture's sort.rs implement and what is its worst case?",
    kind: "research-chat",
    description: "Chat-shaped research query: answer directly without loading heavy report/fidelity machinery",
    expectedOutcome: {
      noHeavyReferences: true,
      noReportToolsRan: true,
      identifiesAlgorithm: "quicksort",
      identifiesWorstCase: "O(n^2)",
    },
  },
  {
    id: "S7",
    name: "research document",
    prompt: "ulw-research write a short markdown report comparing the two parsers in the fixture",
    kind: "research-doc",
    description: "Document deliverable: full research machinery loaded, produces markdown comparison report",
    expectedOutcome: {
      markdownReportProduced: true,
      comparesParsers: ["RecursiveDescentParser", "ShiftReduceParser"],
    },
  },
];
