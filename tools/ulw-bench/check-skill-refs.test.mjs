// SPDX-License-Identifier: MIT
// check-skill-refs.test.mjs - Unit and integration tests for check-skill-refs.

import assert from "node:assert/strict"
import { execFileSync } from "node:child_process"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { afterEach, beforeEach, describe, it } from "node:test"

import { checkLine, scanSkillRefs } from "./check-skill-refs.mjs"

describe("check-skill-refs rules", () => {
	const repoRoot = process.cwd()
	const dummyContext = {
		filePath: join(repoRoot, "assets/omo-latest/skills/ulw-plan/SKILL.md"),
		repoRoot,
		skillDir: join(repoRoot, "assets/omo-latest/skills/ulw-plan"),
	}

	it("rule (a): flags missing repo-relative path in backticks", () => {
		const findings = checkLine("Run `crates/fake_nonexistent_crate/lib.rs` here", 1, dummyContext)
		assert.equal(findings.length, 1)
		assert.equal(findings[0].rule, "a")
		assert.match(findings[0].message, /missing repo-relative path/)
	})

	it("rule (a): ignores placeholder paths in backticks", () => {
		const findings = checkLine("Load `assets/<name>.mmd` or `tools/<script>`", 1, dummyContext)
		assert.equal(findings.length, 0)
	})

	it("rule (a): accepts existing repo-relative path in backticks", () => {
		const findings = checkLine("See `tools/ulw-bench/check-skill-refs.mjs` for gate", 1, dummyContext)
		assert.equal(findings.length, 0)
	})

	it("rule (b): flags missing skill-relative reference and script", () => {
		const findingsRef = checkLine("Read references/nonexistent_guide.md for info", 1, dummyContext)
		assert.equal(findingsRef.length, 1)
		assert.equal(findingsRef[0].rule, "b")

		const findingsScript = checkLine("Run scripts/nonexistent_tool.mjs now", 2, dummyContext)
		assert.equal(findingsScript.length, 1)
		assert.equal(findingsScript[0].rule, "b")
	})

	it("rule (b): accepts existing skill-relative reference", () => {
		const findings = checkLine("Read references/full-workflow.md for detail", 1, dummyContext)
		assert.equal(findings.length, 0)
	})

	const visualQaContext = {
		filePath: join(repoRoot, "assets/omo-latest/skills/visual-qa/SKILL.md"),
		repoRoot,
		skillDir: join(repoRoot, "assets/omo-latest/skills/visual-qa"),
	}

	it("rule (b): accepts sibling-qualified skill path", () => {
		const f1 = checkLine("documented in `frontend/references/perfection/detect.md`", 1, visualQaContext)
		assert.deepEqual(f1, [])
		const f2 = checkLine("bun <frontend skill dir>/scripts/detect/cli.ts --json <url>", 2, visualQaContext)
		assert.deepEqual(f2, [])
	})

	it("rule (b): accepts path that exists in a sibling skill named on the line", () => {
		const findings = checkLine("the `browser` skill's `references/owned-engine/README.md` covers it", 1, visualQaContext)
		assert.deepEqual(findings, [])
	})

	it("rule (b): accepts sibling path when the sibling is named in the enclosing paragraph", () => {
		const ctx = { ...visualQaContext, paragraph: "(staged in the `browser` skill; load it through\nthat skill's `scripts/omowright.mjs` from js eval)" }
		assert.deepEqual(checkLine("that skill's `scripts/omowright.mjs` from js eval)", 2, ctx), [])
		const missing = checkLine("that skill's `scripts/nonexistent_sibling_tool.mjs`", 2, ctx)
		assert.equal(missing.length, 2)
	})

	it("rule (b): still flags a path missing in both current and named sibling skill", () => {
		const f1 = checkLine("the `browser` skill's `references/nonexistent_sibling_doc.md`", 1, visualQaContext)
		assert.equal(f1.length, 1)
		assert.equal(f1[0].rule, "b")
		const f2 = checkLine("see `frontend/references/perfection/nonexistent.md`", 2, visualQaContext)
		assert.equal(f2.length, 1)
		assert.equal(f2[0].rule, "b")
	})

	const researchContext = {
		filePath: join(repoRoot, "assets/omo-latest/skills/ulw-research/SKILL.md"),
		repoRoot,
		skillDir: join(repoRoot, "assets/omo-latest/skills/ulw-research"),
	}

	it("rule (c): flags unregistered report-tools subcommand", () => {
		const findings = checkLine('node "$SKILL_DIR/scripts/report-tools.mjs" bad-command --foo', 1, researchContext)
		assert.equal(findings.length, 1)
		assert.equal(findings[0].rule, "c")
		assert.match(findings[0].message, /unregistered report-tools\.mjs subcommand/)
	})

	it("rule (c): accepts registered report-tools subcommands", () => {
		const f1 = checkLine('node "$SKILL_DIR/scripts/report-tools.mjs" check report.html', 1, researchContext)
		assert.equal(f1.length, 0)
		const f2 = checkLine('node "$SKILL_DIR/scripts/report-tools.mjs" outcome gate static pass', 2, researchContext)
		assert.equal(f2.length, 0)
		const f3 = checkLine('node "$SKILL_DIR/scripts/report-tools.mjs" fact add --source S1', 3, researchContext)
		assert.equal(f3.length, 0)
	})

	it("rule (d): flags denylisted binary omo-agent-toolkit", () => {
		const findings = checkLine("call omo-agent-toolkit ulw-loop status", 1, dummyContext)
		assert.equal(findings.length, 1)
		assert.equal(findings[0].rule, "d")
		assert.match(findings[0].message, /denylisted binary: omo-agent-toolkit/)
	})

	it("rule (d): flags python3 -c in eval guidance", () => {
		const findings = checkLine('inside eval guidance: run python3 -c "import sys"', 1, dummyContext)
		assert.equal(findings.length, 1)
		assert.equal(findings[0].rule, "d")
		assert.match(findings[0].message, /python3 -c/)
	})

	it("rule (d): flags eval language other than js", () => {
		const f1 = checkLine("Hop to py for quick calculation", 1, dummyContext)
		assert.equal(f1.length, 1)
		assert.equal(f1[0].rule, "d")

		const f2 = checkLine('eval with { language: "python" } is invalid', 2, dummyContext)
		assert.equal(f2.length, 1)
		assert.equal(f2[0].rule, "d")
	})
})

describe("integration scan", () => {
	const repoRoot = process.cwd()

	it("ulw-plan and ulw-execute are clean after fixes", () => {
		const findings = scanSkillRefs({
			repoRoot,
			customPaths: ["ulw-plan", "ulw-execute"],
		})
		assert.deepEqual(findings, [])
	})
})

describe("cli execution", () => {
	const repoRoot = process.cwd()
	let tempSkillDir

	beforeEach(() => {
		tempSkillDir = mkdtempSync(join(tmpdir(), "check-refs-scratch-"))
		writeFileSync(join(tempSkillDir, "SKILL.md"), "# Scratch Skill\nRun `scripts/nope.mjs` to test.\n")
	})

	afterEach(() => {
		if (tempSkillDir) rmSync(tempSkillDir, { recursive: true, force: true })
	})

	it("exits 1 and reports offender on scratch skill naming scripts/nope.mjs", () => {
		const scriptPath = join(repoRoot, "tools/ulw-bench/check-skill-refs.mjs")
		let stdout = ""
		let exitCode = 0
		try {
			stdout = execFileSync("node", [scriptPath, tempSkillDir], {
				cwd: repoRoot,
				encoding: "utf8",
				stdio: "pipe",
			})
		} catch (error) {
			exitCode = error.status
			stdout = (error.stdout || "") + "\n" + (error.stderr || "")
		}
		assert.equal(exitCode, 1)
		assert.match(stdout, /scripts\/nope\.mjs/)
	})
})
