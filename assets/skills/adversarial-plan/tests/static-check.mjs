// Static acceptance checks for adversarial-plan/SKILL.md (spec §9). Run: node tests/static-check.mjs
// Behavioural checks (live team runs) need an mhc session; this verifies the skill text encodes them.
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const md = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "SKILL.md"), "utf8");
const checks = [
	["frontmatter name", /^---\nname: adversarial-plan\n/],
	["frontmatter description", /\ndescription: .+\n/],
	["1 child-session stop before team_create", /top-level session[\s\S]*child[\s\S]*Do not call `team_create`/],
	["2 tools-absent stop message", /`team_create`, `task_send` and `team_delete`[\s\S]*If any is missing, stop/],
	["3 exactly one team_create + single deep-low retry", /Exactly one call[\s\S]*retry \*\*once\*\*[\s\S]*minus `evidence`/],
	["4 gated rounds", /a round starts only when every expected reply/i],
	["4 round-3 only own attacks", /\*\*only the attacks aimed at its own findings\*\*/],
	["5 bundle four groups", /### Constraints[\s\S]*### Agreed decisions[\s\S]*### Risks and mitigations[\s\S]*### Open questions/],
	["5 provenance counts", /### Provenance[\s\S]*Survivors per member[\s\S]*Dropped \(conceded\)/],
	["5 drop conceded", /Drop every \*\*conceded\*\* finding/],
	["6 foreground task w/ plan skill", /"run_in_background": false[\s\S]*"load_skills": \[|"load_skills": \[[\s\S]*"run_in_background": false/],
	["6 four obligations", /constraint holds[\s\S]*mitigation is attached[\s\S]*decision gate[\s\S]*success criteria/],
	["6 full bundle in prompt", /INSIGHTS:\n<full insight bundle>/],
	["7 plan shown unchanged + provenance line", /planner's output \*\*unchanged\*\*, preceded by one line/],
	["8 team_delete force after planner", /Only after the planner has returned[\s\S]*"force": true/],
	["roster ids", /`minimalist`[\s\S]*`breakage`[\s\S]*`evidence`[\s\S]*`structure`[\s\S]*`alternatives`/],
	["no softening", /\*\*Do not soften it\*\*/],
];
const forbidden = /hyperplan|\bomo\b|oh-my|opencode|senpi|sisyphus|prometheus|momus|metis|hephaestus|\boracle\b/i;
let fail = 0;
for (const [label, re] of checks) {
	const ok = re.test(md);
	if (!ok) fail++;
	console.log(`${ok ? "ok  " : "FAIL"} ${label}`);
}
const bad = md.match(forbidden);
console.log(`${bad ? "FAIL" : "ok  "} 9 no upstream names${bad ? `: ${bad[0]}` : ""}`);
if (bad) fail++;
process.exitCode = fail ? 1 : 0;
