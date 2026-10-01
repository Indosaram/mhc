#!/usr/bin/env bun
// Golden harness for todo 19: the dynamic system prompt and the skill diagnostics.
//
// It builds one fixture cwd (AGENTS.md + two skills), renders the dynamic system prompt with the
// pinned senpi sources and with the Rust port, and asserts the two are byte-identical. It then
// repeats the comparison for a skill whose frontmatter is invalid: both sides must skip the skill
// and report a warning diagnostic for the same path.
//
// Usage: bun tools/golden/system-prompt.mjs [--repo <checkout>]
//
// Evidence: <repo>/.omo/evidence/task-19-prompt.txt and <repo>/.omo/evidence/task-19-badskill.txt

import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const SENPI_SRC = process.env.SENPI_SRC ?? "/Users/indo/code/senpi";
const CARGO_WRAPPER = process.env.CARGO_WRAPPER ?? "/Volumes/T9-Mac/maho-code-lanes/cargo4";
const here = dirname(fileURLToPath(import.meta.url));

const repoArgument = process.argv.indexOf("--repo");
const repo = repoArgument >= 0 ? resolve(process.argv[repoArgument + 1]) : resolve(here, "..", "..");
const targetDir = process.env.CARGO_TARGET_DIR ?? "/Volumes/T9-Mac/.cargo-target/maho-code/lane-19";

const TOOLS = ["read", "bash", "edit", "write", "grep", "glob", "skill", "session_list"];

const senpiDriver = (fixture, agentDir) => `
import { join } from "node:path";
import { readFileSync } from "node:fs";
import { buildDynamicSystemPrompt } from ${JSON.stringify(join(SENPI_SRC, "packages/coding-agent/src/core/dynamic-prompt/index.ts"))};
import { loadSkills } from ${JSON.stringify(join(SENPI_SRC, "packages/coding-agent/src/core/skills.ts"))};
import { loadProjectContextFiles } from ${JSON.stringify(join(SENPI_SRC, "packages/coding-agent/src/core/resource-loader.ts"))};

const cwd = ${JSON.stringify(fixture)};
const agentDir = ${JSON.stringify(agentDir)};
const skills = loadSkills({ cwd, agentDir, skillPaths: [], includeDefaults: false }).skills;
const toolSnippets = {
  read: "Read file contents",
  bash: "Execute bash commands",
  edit: "Make precise file edits",
  write: "Create or overwrite files",
  grep: "Search file contents",
  glob: "Find files by pattern",
  skill: "Load a skill's instructions",
  session_list: "List sessions",
};
const prompt = buildDynamicSystemPrompt({
  cwd,
  selectedTools: ${JSON.stringify(TOOLS)},
  toolSnippets,
  promptGuidelines: [],
  contextFiles: loadProjectContextFiles({ cwd, agentDir }),
  skills,
});
process.stdout.write(prompt);
`;

function runSenpiSkillDiagnostics(dir) {
  const script = `
import { loadSkillsFromDir } from ${JSON.stringify(join(SENPI_SRC, "packages/coding-agent/src/core/skills.ts"))};
const result = loadSkillsFromDir({ dir: ${JSON.stringify(dir)}, source: "path" });
console.log(JSON.stringify({
  skills: result.skills.map((skill) => skill.name),
  diagnostics: result.diagnostics.map((diagnostic) => ({ type: diagnostic.type, path: diagnostic.path, message: diagnostic.message })),
}, null, 2));
`;
  return runBun(script);
}

function runBun(source) {
  const file = join(tmpdir(), `maho-golden-${process.pid}-${Math.random().toString(36).slice(2)}.ts`);
  writeFileSync(file, source);
  try {
    return execFileSync("bun", [file], {
      encoding: "utf8",
      cwd: repo,
      env: { ...process.env, SENPI_BRAND: JSON.stringify({ name: "maho" }) },
    });
  } finally {
    rmSync(file, { force: true });
  }
}

function runRustExample(example, args) {
  return execFileSync(CARGO_WRAPPER, ["run", "--quiet", "-p", "maho-core", "--example", example, "--", ...args], {
    encoding: "utf8",
    cwd: repo,
    env: { ...process.env, CARGO_TARGET_DIR: targetDir },
    stdio: ["ignore", "pipe", "inherit"],
  });
}

function writeEvidence(name, text) {
  const dir = join(repo, ".omo", "evidence");
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, name), text);
  return join(dir, name);
}

function buildFixture() {
  const root = mkdtempSync(join(tmpdir(), "maho-golden-19-"));
  const cwd = join(root, "project");
  const agentDir = join(root, "agent");
  mkdirSync(join(cwd, ".maho"), { recursive: true });
  mkdirSync(join(agentDir), { recursive: true });
  writeFileSync(join(cwd, "AGENTS.md"), "# Project rules\n\nKeep changes small.\n");
  mkdirSync(join(cwd, ".maho", "skills", "alpha"), { recursive: true });
  writeFileSync(
    join(cwd, ".maho", "skills", "alpha", "SKILL.md"),
    "---\nname: alpha\ndescription: First fixture skill\n---\n\nAlpha body.\n",
  );
  mkdirSync(join(cwd, ".maho", "skills", "beta"), { recursive: true });
  writeFileSync(
    join(cwd, ".maho", "skills", "beta", "SKILL.md"),
    "---\nname: beta\ndescription: Second fixture skill\n---\n\nBeta body.\n",
  );
  return { root, cwd, agentDir };
}

function buildBadSkillFixture() {
  const root = mkdtempSync(join(tmpdir(), "maho-golden-19-bad-"));
  mkdirSync(join(root, "broken"), { recursive: true });
  writeFileSync(join(root, "broken", "SKILL.md"), "---\nname: broken\ndescription: [unclosed\n---\n\nBody.\n");
  return root;
}

let failures = 0;

const fixture = buildFixture();
try {
  const senpiPrompt = runBun(senpiDriver(fixture.cwd, fixture.agentDir));
  const rustPrompt = runRustExample("system_prompt_fixture", [fixture.cwd, fixture.agentDir, TOOLS.join(",")]);
  const equal = senpiPrompt === rustPrompt;
  const report = [
    `# todo 19 QA - dynamic system prompt for a fixture cwd (AGENTS.md + 2 skills)`,
    ``,
    `fixture cwd: ${fixture.cwd}`,
    `senpi: ${SENPI_SRC} (pinned fe8c564b), run under SENPI_BRAND={"name":"maho"} so both sides carry the engine identity maho`,
    `rust: ${repo}/crates/maho-core (example system_prompt_fixture)`,
    `result: ${equal ? "BYTE-EQUAL" : "MISMATCH"}`,
    ``,
    `## senpi output (${senpiPrompt.length} bytes)`,
    senpiPrompt,
  ];
  if (!equal) {
    failures += 1;
    let index = 0;
    while (index < Math.min(senpiPrompt.length, rustPrompt.length) && senpiPrompt[index] === rustPrompt[index]) index += 1;
    report.push(
      ``,
      `first difference at byte ${index}`,
      `senpi: ${JSON.stringify(senpiPrompt.slice(Math.max(0, index - 60), index + 60))}`,
      `rust:  ${JSON.stringify(rustPrompt.slice(Math.max(0, index - 60), index + 60))}`,
      ``,
      `## rust output (${rustPrompt.length} bytes)`,
      rustPrompt,
    );
  }
  const evidencePath = writeEvidence("task-19-prompt.txt", report.join("\n"));
  console.log(`${equal ? "PASS" : "FAIL"} system prompt: ${senpiPrompt.length} bytes vs ${rustPrompt.length} bytes (${evidencePath})`);
} finally {
  rmSync(fixture.root, { recursive: true, force: true });
}

const badRoot = buildBadSkillFixture();
try {
  const senpiDiagnostics = JSON.parse(runSenpiSkillDiagnostics(badRoot));
  const rustDiagnostics = JSON.parse(runRustExample("skill_diagnostics_fixture", [badRoot]));
  const senpiSkipped = senpiDiagnostics.skills.length === 0;
  const rustSkipped = rustDiagnostics.skills.length === 0;
  const senpiWarning = senpiDiagnostics.diagnostics[0];
  const rustWarning = rustDiagnostics.diagnostics[0];
  const sameShape =
    senpiSkipped &&
    rustSkipped &&
    senpiWarning?.type === "warning" &&
    rustWarning?.type === "warning" &&
    senpiWarning?.path === rustWarning?.path;
  const report = [
    `# todo 19 QA - a skill with invalid frontmatter is skipped with a warning`,
    ``,
    `fixture: ${badRoot}/broken/SKILL.md`,
    `result: ${sameShape ? "SAME SKIP DECISION, TYPE AND PATH" : "MISMATCH"}`,
    ``,
    `## senpi (yaml npm package)`,
    JSON.stringify(senpiDiagnostics, null, 2),
    ``,
    `## rust (serde_yaml)`,
    JSON.stringify(rustDiagnostics, null, 2),
    ``,
    `Both sides skip the skill and report one warning for the same path. The message text differs:`,
    `senpi reports the yaml package's parse error, the Rust port reports serde_yaml's (see the ledger notes).`,
  ];
  if (!sameShape) {
    failures += 1;
  }
  const evidencePath = writeEvidence("task-19-badskill.txt", report.join("\n"));
  console.log(`${sameShape ? "PASS" : "FAIL"} invalid-frontmatter skill: ${evidencePath}`);
} finally {
  rmSync(badRoot, { recursive: true, force: true });
}

process.exit(failures === 0 ? 0 : 1);
