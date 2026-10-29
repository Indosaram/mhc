#!/usr/bin/env node
// Static check for the give-me-tips skill (spec section 5). No dependencies.
// Usage: node tests/static-check.mjs   (exit 0 = pass, 1 = failures listed)
import { readFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const failures = [];
const passes = [];
const check = (ok, label) => (ok ? passes : failures).push(label);

const skillPath = join(root, "SKILL.md");
if (!existsSync(skillPath)) {
  console.error("FAIL: SKILL.md not found");
  process.exit(1);
}
const skill = readFileSync(skillPath, "utf8");

// --- frontmatter ---
const fm = skill.match(/^---\n([\s\S]*?)\n---\n/);
check(!!fm, "SKILL.md starts with a YAML frontmatter block");
const front = fm ? fm[1] : "";
const field = (key) => {
  const m = front.match(new RegExp(`^${key}:\\s*(.*)$`, "m"));
  return m ? m[1].trim().replace(/^["']|["']$/g, "") : null;
};
const name = field("name");
check(name === "give-me-tips", `frontmatter name is exactly "give-me-tips" (got ${JSON.stringify(name)})`);
check(name !== null && /^[a-z0-9-]{1,64}$/.test(name), "name uses lowercase/digits/hyphens, <= 64 chars");
const description = field("description");
check(!!description && description.length > 0, "description is non-empty");
check(!!description && description.length <= 1024, `description <= 1024 chars (got ${description ? description.length : 0})`);
check(!/^disable-model-invocation:/m.test(front), "disable-model-invocation is not set");

// --- required mentions ---
for (const term of ["--list-tips", "settings.json", "tipsHistory", "requiresCommand"]) {
  check(skill.includes(term), `SKILL.md mentions ${term}`);
}
check(skill.includes("https://github.com/Indosaram/mhc"), "SKILL.md names the public mhc repo for the fallback");

// --- banned names (spec section 7) ---
// Exact runtime identifiers that legitimately contain a banned fragment are removed first.
const allowList = [
  "omo-fallback-architect:tip",
  "omo-fallback-architect:notice",
  "omo-kibitzer:nudged",
  "~/.omo/agent/",
  "crates/omo/components/maho-omo-memory/",
  "crates/omo/components/",
];
let scrubbed = skill;
for (const id of allowList) scrubbed = scrubbed.split(id).join(" ");
const banned = [
  "Sisyphus", "Prometheus", "oh-my-openagent", "OmO", "omo-senpi", "senpi --list-tips",
  "senpi", "ultrawork", "ulw", "mass-ulw", "hyperplan", "Fable", "Kibitzer",
];
const esc = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
for (const term of banned) {
  const re = new RegExp(`(^|[^a-z0-9])${esc(term)}([^a-z0-9]|$)`, "i");
  const hit = scrubbed.match(re);
  check(!hit, `banned name "${term}" absent${hit ? ` (found near: ${JSON.stringify(scrubbed.slice(Math.max(0, hit.index - 20), hit.index + 30))})` : ""}`);
}
// Upstream launcher env vars and package paths (spec section 7 drops).
for (const term of ["OMO_NATIVE", "OMO_BIN", "packages/coding-agent", "packages/omo-senpi"]) {
  check(!skill.includes(term), `dropped upstream reference "${term}" absent`);
}

// --- companion files ---
const license = existsSync(join(root, "LICENSE")) ? readFileSync(join(root, "LICENSE"), "utf8") : "";
check(/MIT License/.test(license) && /Copyright \(c\) 2026 Indo Yoon/.test(license), "LICENSE is MIT, (c) 2026 Indo Yoon");
check(existsSync(join(root, "CLEANROOM.md")), "CLEANROOM.md present");

for (const p of passes) console.log(`PASS: ${p}`);
for (const f of failures) console.error(`FAIL: ${f}`);
console.log(`\n${passes.length}/${passes.length + failures.length} checks passed`);
process.exit(failures.length ? 1 : 0);
