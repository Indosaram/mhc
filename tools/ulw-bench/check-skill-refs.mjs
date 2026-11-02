// SPDX-License-Identifier: MIT
// check-skill-refs.mjs - Stale-reference gate for ulw skills and contracts.

import { existsSync, readdirSync, readFileSync, statSync } from "node:fs"
import { dirname, join, relative, resolve } from "node:path"
import { fileURLToPath } from "node:url"

export const DEFAULT_SKILL_NAMES = [
	"ultrawork",
	"ulw-loop",
	"ulw-execute",
	"ulw-plan",
	"ulw-research",
	"visual-qa",
]

export const REGISTERED_REPORT_TOOLS_COMMANDS = new Set([
	"check",
	"layout-probe",
	"repair decide",
	"outcome init",
	"outcome set",
	"outcome gate",
	"outcome render",
	"outcome state",
	"outcome verify",
	"outcome finish",
	"outcome briefing",
	"format-extract",
	"fact add",
	"fact quote",
	"fact retract",
	"fact export",
	"fact fill",
	"fact verify",
	"fact derive",
	"fact pairs",
	"fact meaning-check",
	"fact polish-apply",
	"fact polish-diff",
	"source snapshot",
	"source refresh",
	"source probe-conversion",
	"claims index",
	"repair",
	"outcome",
	"fact",
	"source",
	"claims",
	"help",
	"--help",
])

function stripPathDecorations(raw) {
	return raw
		.replace(/[,.;:?!)\]'"]+$/, "")
		.replace(/:\d+(?::\d+)?$/, "")
		.replace(/#.*$/, "")
}

function findSkillDir(filePath, repoRoot) {
	let current = dirname(filePath)
	while (current.length >= repoRoot.length) {
		const skillMd = join(current, "SKILL.md")
		if (existsSync(skillMd)) return current
		const parent = dirname(current)
		if (parent === current) break
		current = parent
	}
	return dirname(filePath)
}

export function checkLine(line, lineNum, context) {
	const { filePath, repoRoot, skillDir } = context
	const findings = []

	// (a) repo-relative paths in backticks matching ^(scripts?|tools|crates|assets|docs)/
	const backtickRegex = /`([^`\r\n]+)`/g
	let btMatch
	while ((btMatch = backtickRegex.exec(line)) !== null) {
		const raw = btMatch[1].trim()
		if (raw.includes("<") || raw.includes(">") || raw.startsWith("--")) continue
		const match = raw.match(/^(?:scripts?|tools|crates|assets|docs)\/[^\s`'"]+/)
		if (match) {
			const clean = stripPathDecorations(match[0])
			if (clean.includes("*")) {
				const baseDir = clean.split("/*")[0]
				if (!existsSync(join(repoRoot, baseDir)) && !existsSync(join(skillDir, baseDir))) {
					findings.push({ line: lineNum, rule: "a", message: `missing directory for wildcard: ${clean}`, match: raw })
				}
				continue
			}
			if (clean.startsWith("scripts/")) {
				const existsSkill = existsSync(join(skillDir, clean))
				const existsRepo = existsSync(join(repoRoot, clean))
				if (!existsSkill && !existsRepo) {
					findings.push({ line: lineNum, rule: "a", message: `missing path in backticks: ${clean}`, match: raw })
				}
			} else {
				if (!existsSync(join(repoRoot, clean))) {
					findings.push({ line: lineNum, rule: "a", message: `missing repo-relative path: ${clean}`, match: raw })
				}
			}
		}
	}

	// (b) skill-relative references/*.md and scripts/*.mjs
	const skillRelRegex = /\b(?:references\/[A-Za-z0-9_\-\./]+\.md|scripts\/[A-Za-z0-9_\-\./]+\.(?:mjs|ts))\b/g
	let srMatch
	while ((srMatch = skillRelRegex.exec(line)) !== null) {
		const raw = srMatch[0]
		if (raw.includes("<") || raw.includes(">")) continue
		const clean = stripPathDecorations(raw)
		const existsSkill = existsSync(join(skillDir, clean))
		const existsFileDir = existsSync(join(dirname(filePath), clean))
		const existsRepo = existsSync(join(repoRoot, clean))
		if (!existsSkill && !existsFileDir && !existsRepo) {
			findings.push({ line: lineNum, rule: "b", message: `missing skill-relative path: ${clean}`, match: raw })
		}
	}

	// (c) report-tools.mjs <cmd> subcommands -> must be registered in report-tools-commands.mjs
	const reportToolsRegex = /(?:node\s+[^`\n]*|`|\$)?report-tools\.mjs["']?\s+([A-Za-z0-9_\-]+(?:\s+[A-Za-z0-9_\-]+)?)/g
	let rtMatch
	while ((rtMatch = reportToolsRegex.exec(line)) !== null) {
		const rawCmd = rtMatch[1].trim()
		if (rawCmd.startsWith("<") || rawCmd.startsWith("-") || rawCmd === "dispatches") continue
		const words = rawCmd.split(/\s+/)
		const oneWord = words[0]
		const secondWord = words[1]
		const twoWord = secondWord && !secondWord.startsWith("-") && !secondWord.startsWith("<")
			? `${oneWord} ${secondWord}`
			: null
		const isRegistered = (twoWord && REGISTERED_REPORT_TOOLS_COMMANDS.has(twoWord)) ||
			REGISTERED_REPORT_TOOLS_COMMANDS.has(oneWord)
		if (!isRegistered) {
			const reportedCmd = twoWord || oneWord
			findings.push({ line: lineNum, rule: "c", message: `unregistered report-tools.mjs subcommand: ${reportedCmd}`, match: rtMatch[0] })
		}
	}

	// (d) binaries in denylist (omo-agent-toolkit, python3 -c inside eval guidance) and eval languages other than js
	if (line.includes("omo-agent-toolkit")) {
		findings.push({ line: lineNum, rule: "d", message: "denylisted binary: omo-agent-toolkit", match: "omo-agent-toolkit" })
	}
	if (line.includes("python3 -c") && /eval/i.test(line)) {
		findings.push({ line: lineNum, rule: "d", message: "denylisted binary in eval guidance: python3 -c", match: "python3 -c" })
	}
	if (/HOP to py/i.test(line)) {
		findings.push({ line: lineNum, rule: "d", message: "eval language other than js: HOP to py", match: "HOP to py" })
	}
	const evalLangMatch = line.match(/(?:eval\b[^`\n]*language:\s*|["']language["']:\s*)["'](?!js\b)([a-zA-Z0-9_\-]+)["']/i)
	if (evalLangMatch && /eval/i.test(line)) {
		findings.push({ line: lineNum, rule: "d", message: `eval language other than js: ${evalLangMatch[1]}`, match: evalLangMatch[0] })
	}

	return findings
}

export function checkFile(filePath, repoRoot) {
	const content = readFileSync(filePath, "utf8")
	const skillDir = findSkillDir(filePath, repoRoot)
	const lines = content.split("\n")
	const findings = []

	for (let i = 0; i < lines.length; i++) {
		const lineFindings = checkLine(lines[i], i + 1, { filePath, repoRoot, skillDir })
		for (const f of lineFindings) {
			findings.push({ file: relative(repoRoot, filePath), ...f })
		}
	}
	return findings
}

export function collectFiles(paths, repoRoot) {
	const files = []
	function walk(dir) {
		if (!existsSync(dir)) return
		for (const entry of readdirSync(dir, { withFileTypes: true })) {
			const full = join(dir, entry.name)
			if (entry.isDirectory()) {
				walk(full)
			} else if (entry.name.endsWith(".md") || entry.name.endsWith(".mjs")) {
				files.push(full)
			}
		}
	}

	for (const p of paths) {
		const full = resolve(repoRoot, p)
		if (!existsSync(full)) continue
		if (statSync(full).isDirectory()) {
			walk(full)
		} else if (full.endsWith(".md") || full.endsWith(".mjs")) {
			files.push(full)
		}
	}
	return files
}

export function scanSkillRefs({ repoRoot = process.cwd(), customPaths = [] } = {}) {
	const targets = []
	if (customPaths.length > 0) {
		for (const p of customPaths) {
			const skillPath = join(repoRoot, "assets/omo-latest/skills", p)
			if (existsSync(skillPath)) targets.push(skillPath)
			else targets.push(resolve(repoRoot, p))
		}
	} else {
		for (const name of DEFAULT_SKILL_NAMES) {
			const dir = join(repoRoot, "assets/omo-latest/skills", name)
			if (existsSync(dir)) targets.push(dir)
		}
		const contractDir = join(repoRoot, "crates/maho-ultrawork/contract")
		if (existsSync(contractDir)) targets.push(contractDir)
	}

	const files = collectFiles(targets, repoRoot)
	const allFindings = []
	for (const file of files) {
		const findings = checkFile(file, repoRoot)
		allFindings.push(...findings)
	}
	return allFindings
}

export async function main(argv = process.argv.slice(2)) {
	let repoRoot = process.cwd()
	const customPaths = []

	for (let i = 0; i < argv.length; i++) {
		if (argv[i] === "--repo-root" && i + 1 < argv.length) {
			repoRoot = resolve(argv[++i])
		} else if (!argv[i].startsWith("--")) {
			customPaths.push(argv[i])
		}
	}

	const findings = scanSkillRefs({ repoRoot, customPaths })

	if (findings.length === 0) {
		console.log("All skill references and commands verified clean.")
		process.exit(0)
	}

	const fileSet = new Set(findings.map((f) => f.file))
	for (const f of findings) {
		console.log(`${f.file}:${f.line}: [rule-${f.rule}] ${f.message}`)
	}
	console.error(`\nFound ${findings.length} offender(s) across ${fileSet.size} file(s).`)
	process.exit(1)
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
	main().catch((err) => {
		console.error("check-skill-refs fatal error:", err)
		process.exit(2)
	})
}
