#!/usr/bin/env bun
// Verify the assembled session2-residual gate evidence against the bound requirements manifest.
//
//   bun tools/verify-session2-residual.mjs --evidence <E> --sha <full-assembled-sha>
//   bun tools/verify-session2-residual.mjs --self-test
//
// The verifier NEVER trusts a claim: it re-reads the frozen package manifests, the per-command exit
// codes and log hashes, the nextest summary AND per-test status lines, the parity fragments
// (declared-vs-parsed row coverage), and the real QA artifacts. It rejects: a null/stale/mismatched
// SHA, an incomplete command set, any nonzero child exit, a skipped or non-executed required test,
// an onboarding-only TUI reply, a parser-hidden ledger row, and any unresolved/not-started scope
// that has no recorded disposition. Exit 0 = pass, 1 = audit failure, 2 = usage error.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { deflateSync } from "node:zlib";

import { parseFragment } from "./parity-audit.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "..");
const FIXTURES = join(HERE, "fixtures", "session2-residual-negative");

export const SCHEMA = "session2-residual-requirements-manifest/v1";
export const QA_SCHEMA = "session2-residual-qa/v1";
export const PNG_SCHEMA = "session2-residual-png/v1";
export const PACKAGE_SCHEMA = "session2-residual-package-manifest/v1";
export const COMMAND_SCHEMA = "session2-residual-command-manifest/v1";

const FULL_SHA = /^[0-9a-f]{40}$/;
const SHA256_HEX = /^[0-9a-f]{64}$/;
const RUN_IDENTITY = "run-identity.json";
const SOURCE_IDENTITY = "source-identity.json";
const SOURCE_IDENTITY_END = "source-identity-end.json";
// The causal TUI fields every case must prove (own prompt, gated stream, steer, abort, resize).
const TUI_REQUIRED_FIELDS = [
	"onboarding_pre_completed",
	"startup_predicate",
	"prompt_echo_observed",
	"gated_stream_visible_while_held",
	"working_indicator_present_while_held",
	"steer_echoed_while_working",
	"abort_cleared_busy",
	"reply_after_prompt",
	"working_indicator_absent_at_reply",
	"resize_reflowed",
];
// A target id is a bare integration-test file stem or the literal `lib` (the crate unit-test target).
const EXACT_TARGET_ID = /^[A-Za-z0-9_]+$/;
// A test identifier is a bare fn name (integration) or a `module::tests::fn` path (lib unit tests).
// Anything with spaces, slashes, parentheses or dots is prose, not an identifier.
const EXACT_TEST_ID = /^[A-Za-z0-9_]+(?:::[A-Za-z0-9_]+)*$/;
const PACKAGE_ID = /^[A-Za-z0-9_-]+$/;
// A machine-consumable template that Task 17 maintains; the verifier binds a COPY, never mutates it.
const TEMPLATE_PATH = ".omo/evidence/residual-source/task-17/requirements-manifest.json";
const ALLOWED_DISPOSITIONS = new Set(["resolved", "accepted-exclusion", "accepted-unresolved"]);
// Post-gate scopes (reviews + integration) run AFTER this gate, so the verifier must not
// circularly require their pre-gate disposition.
const POST_GATE_SCOPES = new Set(["F1", "F2", "F3", "F4", "task-19", "task-20"]);
// The fixed approved requirement IDs (G1-G14, IS-1..6, the SDK ids, the parser negative).
const REQUIRED_REQUIREMENT_IDS = [
	"G1", "G2", "G3", "G4", "G5", "G6", "G7", "G8", "G9", "G10", "G11", "G12", "G13", "G14",
	"IS-1", "IS-2", "IS-3", "IS-4", "IS-5", "IS-6",
	"SDK-NATIVE-SESSION", "SDK-ROW10-14", "PARSE-HIDDEN-ROW",
];
// The accepted task-12 server rows (53-row manifest rows 1-53 + row 710). The server map is only
// complete when EVERY accepted row is present, so a dropped/renamed row cannot silently escape.
const SERVER_ROW_IDS = [
	1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27,
	28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53,
	710,
];
// The mandatory TUI matrix (independent of the manifest's own arrays).
const TUI_GEOMETRIES = ["80x24", "120x36", "200x50"];
const TUI_MODES = ["regular", "fullscreen"];
const TASK8_LEDGER = "crates/maho-cli/parity.d/38.md";
const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const VERIFY_COMMAND_MARKER = "verify-session2-residual.mjs --evidence";
const REQUIRED_SCENARIOS = ["tui", "theme", "install", "mini", "server", "registry"];

export class Report {
	constructor() {
		this.problems = [];
		this.notes = [];
	}
	fail(message) {
		this.problems.push(message);
	}
	note(message) {
		this.notes.push(message);
	}
}

export function sha256(data) {
	return createHash("sha256").update(data).digest("hex");
}

export function readJson(path) {
	return JSON.parse(readFileSync(path, "utf8"));
}

// ---------------------------------------------------------------------------------------------
// Workspace / package discovery (no cargo invocation).
// ---------------------------------------------------------------------------------------------

function parseTomlString(text, key) {
	const match = text.match(new RegExp(`^\\s*${key}\\s*=\\s*"([^"]*)"`, "m"));
	return match ? match[1] : null;
}

export function workspaceMembers(repo) {
	const text = readFileSync(join(repo, "Cargo.toml"), "utf8");
	const block = text.match(/\[workspace\][\s\S]*?members\s*=\s*\[([\s\S]*?)\]/);
	if (!block) throw new Error("Cargo.toml has no [workspace] members list");
	return block[1]
		.split("\n")
		.map((line) => line.trim().replace(/[",]/g, ""))
		.filter(Boolean)
		.map((member) => member.replace(/\/$/, ""));
}

/** Package name -> manifest path, over the AUTHORITATIVE workspace membership (the root
 *  `[workspace] members` list), never every `crates/**\/Cargo.toml` (which would include excluded
 *  crates and misreport them as missing members). */
export function packageManifests(repo) {
	const map = new Map();
	for (const member of workspaceMembers(repo)) {
		if (member.includes("*")) throw new Error("Cargo.toml uses glob members; cargo metadata is required");
		const manifest = join(repo, member, "Cargo.toml");
		if (!existsSync(manifest)) continue;
		const name = parseTomlString(readFileSync(manifest, "utf8"), "name");
		if (name) map.set(name, manifest);
	}
	return map;
}

/** Locates the source file behind a (package, target) pair.
 *  `target: "lib"` is the crate unit-test target (`src/lib.rs`); anything else is an
 *  auto-discovered integration test (`tests/<target>.rs`). */
export function findTargetFile(repo, pkg, target) {
	const manifests = packageManifests(repo);
	const manifest = manifests.get(pkg);
	if (!manifest) return null;
	const dir = dirname(manifest);
	if (target === "lib") {
		for (const name of ["src/lib.rs", "lib.rs", "src/main.rs"]) {
			const candidate = join(dir, name);
			if (existsSync(candidate)) return candidate;
		}
		return null;
	}
	const candidate = join(dir, "tests", `${target}.rs`);
	return existsSync(candidate) ? candidate : null;
}

/** The nextest binary id for a (package, target): a lib target reports under the bare package
 *  name, an integration target under `<package>::<target>`. */
export function binaryIdFor(pkg, target) {
	return target === "lib" ? pkg : `${pkg}::${target}`;
}

// ---------------------------------------------------------------------------------------------
// Bound manifest + SHA binding.
// ---------------------------------------------------------------------------------------------

export function validateBoundManifest(evidenceDir, sha, report, repo) {
	const path = join(evidenceDir, "requirements-manifest.json");
	if (!existsSync(path)) {
		report.fail(`bound manifest missing: ${path}`);
		return null;
	}
	let manifest;
	try {
		manifest = readJson(path);
	} catch (error) {
		report.fail(`bound manifest is not valid JSON: ${error.message}`);
		return null;
	}
	if (manifest.schema !== SCHEMA) report.fail(`bound manifest schema "${manifest.schema}" != "${SCHEMA}"`);
	const bound = manifest.binding?.assembled_sha;
	if (bound === null || bound === undefined) {
		report.fail("bound manifest assembled_sha is null (unbound template; bind a copy to the gate SHA)");
	} else if (!FULL_SHA.test(String(bound))) {
		report.fail(`bound manifest assembled_sha "${bound}" is not a full 40-hex SHA`);
	} else if (String(bound) !== sha) {
		report.fail(`bound manifest assembled_sha ${bound} is STALE (gate SHA ${sha})`);
	}
	const from = manifest.bound_from;
	if (!from || typeof from.source_path !== "string" || !SHA256_HEX.test(String(from.source_sha256))) {
		report.fail("bound manifest lacks bound_from provenance (source_path + 64-hex sha256)");
	}
	if (repo && from && typeof from.source_path === "string") {
		const templatePath = join(repo, TEMPLATE_PATH);
		if (!existsSync(templatePath)) {
			report.fail(`committed manifest template missing: ${TEMPLATE_PATH}`);
		} else {
			const liveHash = sha256(readFileSync(templatePath));
			if (from.source_sha256 !== liveHash) {
				report.fail(
					`bound manifest was derived from a STALE/foreign template: bound_from.source_sha256=${from.source_sha256} ` +
						`but ${TEMPLATE_PATH} is ${liveHash}`,
				);
			}
			if (resolve(from.source_path) !== resolve(templatePath) && from.source_path !== TEMPLATE_PATH) {
				report.fail(`bound manifest bound_from.source_path ${from.source_path} != ${TEMPLATE_PATH}`);
			}
		}
	}
	if (!Array.isArray(manifest.gate_commands) || manifest.gate_commands.length === 0) {
		report.fail("bound manifest has no gate_commands");
	}
	return manifest;
}

// ---------------------------------------------------------------------------------------------
// Package manifest (frozen, all workspace members).
// ---------------------------------------------------------------------------------------------

export function validatePackageManifest(evidenceDir, repo, sha, manifest, report) {
	const path = join(evidenceDir, "package-manifest.json");
	if (!existsSync(path)) {
		report.fail(`package manifest missing: ${path}`);
		return;
	}
	let frozen;
	try {
		frozen = readJson(path);
	} catch (error) {
		report.fail(`package manifest is not valid JSON: ${error.message}`);
		return;
	}
	if (frozen.schema !== PACKAGE_SCHEMA) report.fail(`package manifest schema "${frozen.schema}" != "${PACKAGE_SCHEMA}"`);
	if (frozen.sha !== sha) report.fail(`package manifest sha ${frozen.sha} != gate SHA ${sha}`);
	const byName = new Map((frozen.packages ?? []).map((entry) => [entry.name, entry]));
	// Coverage is asserted against the AUTHORITATIVE workspace membership (the root members list),
	// not every `crates/**/Cargo.toml` (excluded/vendored crates are not members).
	const current = packageManifests(repo);
	for (const [name] of current) {
		if (!byName.has(name)) report.fail(`package manifest omits workspace member ${name} (incomplete coverage)`);
	}
	for (const entry of frozen.packages ?? []) {
		if (entry.workspace_member === false) continue;
		if (typeof entry.manifest_path !== "string" || entry.manifest_path.length === 0) {
			report.fail(`package manifest entry ${entry.name} has no manifest_path`);
			continue;
		}
		const manifestPath = resolve(repo, entry.manifest_path);
		if (!existsSync(manifestPath)) {
			report.fail(`package manifest entry ${entry.name} points at a missing manifest (${entry.manifest_path})`);
			continue;
		}
		const live = sha256(readFileSync(manifestPath));
		if (live !== entry.sha256) report.fail(`package manifest stale: ${entry.name} Cargo.toml changed since freeze`);
	}
	// Every real package named by a requirement must be frozen.
	const pseudo = new Set(["packaging", "tools", "workspace"]);
	for (const requirement of manifest?.requirements ?? []) {
		for (const pkg of requirement.packages ?? []) {
			if (pseudo.has(pkg)) continue;
			if (!byName.has(pkg)) report.fail(`requirement ${requirement.id} names package ${pkg} absent from the frozen manifest`);
		}
	}
}

// ---------------------------------------------------------------------------------------------
// Command manifest (all gate commands, keep-going, real exit codes).
// ---------------------------------------------------------------------------------------------

export function validateCommandManifest(evidenceDir, sha, manifest, report, final = false) {
	const path = join(evidenceDir, "command-manifest.json");
	if (!existsSync(path)) {
		report.fail(`command manifest missing: ${path}`);
		return;
	}
	let frozen;
	try {
		frozen = readJson(path);
	} catch (error) {
		report.fail(`command manifest is not valid JSON: ${error.message}`);
		return;
	}
	if (frozen.schema !== COMMAND_SCHEMA) report.fail(`command manifest schema "${frozen.schema}" != "${COMMAND_SCHEMA}"`);
	if (frozen.sha !== sha) report.fail(`command manifest sha ${frozen.sha} != gate SHA ${sha}`);
	const entries = frozen.commands ?? [];
	const byTemplate = new Map(entries.map((entry) => [entry.template, entry]));
	for (const template of manifest?.gate_commands ?? []) {
		const entry = byTemplate.get(template);
		if (!entry) {
			report.fail(`command manifest is missing gate command: ${template}`);
			continue;
		}
		const self = String(template).includes(VERIFY_COMMAND_MARKER);
		const missing = entry.exit_code === null || entry.exit_code === undefined;
		if (missing) {
			// Only the still-running verifier's own entry may be null, and only in a PRE-FINAL audit.
			// In final mode every command (including this verifier) has a completed exit code.
			if (!self || final) report.fail(`gate command has no exit code${final ? " in the final audit" : ""}: ${template}`);
		} else if (entry.exit_code !== 0) {
			report.fail(`gate command exited ${entry.exit_code} (expected 0): ${template}`);
		} else {
			// A COMPLETED command must carry an immutable raw log with a 64-hex hash that matches the
			// actual bytes; a missing/malformed hash is not accepted.
			if (!entry.log) {
				report.fail(`completed command has no log: ${template}`);
			} else if (!existsSync(join(evidenceDir, entry.log))) {
				report.fail(`command log missing for a completed command: ${entry.log}`);
			} else if (!SHA256_HEX.test(String(entry.log_sha256 ?? ""))) {
				report.fail(`completed command log has no 64-hex log_sha256: ${template}`);
			} else if (sha256(readFileSync(join(evidenceDir, entry.log))) !== entry.log_sha256) {
				report.fail(`command log is STALE (hash changed since capture): ${entry.log}`);
			}
		}
	}
	if (final) {
		for (const entry of entries) {
			if (entry.exit_code === null || entry.exit_code === undefined) report.fail(`final audit: command never completed: ${entry.template}`);
			if (!entry.log) report.fail(`final audit: command has no log: ${entry.template}`);
			if (!SHA256_HEX.test(String(entry.log_sha256 ?? ""))) report.fail(`final audit: command has no 64-hex log_sha256: ${entry.template}`);
		}
	}
}

export function validateSourceIdentity(evidenceDir, repo, sha, report, final = false) {
	const runPath = join(evidenceDir, RUN_IDENTITY);
	if (!existsSync(runPath)) report.fail(`run identity missing: ${RUN_IDENTITY}`);
	else {
		const run = readJson(runPath);
		if (run.sha !== sha) report.fail(`run identity sha ${run.sha} != gate SHA ${sha}`);
	}
	const srcPath = join(evidenceDir, SOURCE_IDENTITY);
	if (!existsSync(srcPath)) {
		report.fail(`source identity missing: ${SOURCE_IDENTITY} (the supplied SHA is not proven to be the worktree HEAD)`);
	} else {
		const src = readJson(srcPath);
		if (src.sha !== sha) report.fail(`source identity sha ${src.sha} != gate SHA ${sha}`);
		if (src.head !== sha) report.fail(`source identity HEAD ${src.head} != gate SHA ${sha} (SHA is not the worktree HEAD)`);
		if (src.clean !== true || (Array.isArray(src.dirty_sources) && src.dirty_sources.length > 0)) {
			report.fail(`begin source identity is not clean: dirty=${JSON.stringify(src.dirty_sources ?? null)}`);
		}
	}
	if (final) {
		// The runner records an end identity after every command; the FINAL audit requires it and
		// fails on a dirty tree or HEAD drift. A pre-final audit must not require it (it does not
		// exist yet), so this check is gated on `final`.
		const endPath = join(evidenceDir, SOURCE_IDENTITY_END);
		if (!existsSync(endPath)) {
			report.fail(`end source identity missing: ${SOURCE_IDENTITY_END}`);
		} else {
			const end = readJson(endPath);
			if (end.head !== sha) report.fail(`end source identity HEAD ${end.head} != gate SHA ${sha} (worktree HEAD drifted during the gate)`);
			if (end.clean !== true || (Array.isArray(end.dirty_sources) && end.dirty_sources.length > 0)) {
				report.fail(`end source identity is not clean (the gate mutated the worktree): dirty=${JSON.stringify(end.dirty_sources ?? null)}`);
			}
		}
	}
}

// ---------------------------------------------------------------------------------------------
// nextest: summary + per-test execution.
// ---------------------------------------------------------------------------------------------

// Real nextest line (workspace run):
//   `        PASS [   0.009s] ( 201/8773) maho-ai api::openai_prompt_cache::tests::affinity_headers_are_skipped_without_a_session_id`
// The `(N/TOTAL)` progress counter is optional; the binary id is the bare package name for a lib
// target or `<package>::<target>` for an integration target.
const NEXTEST_STATUS =
	/^\s*(PASS|FAIL|SKIP|SLOW|LEAK|ABORT|TIMEOUT|SIGSEGV|SIGABRT)\s+\[\s*[^\]]*\]\s+(?:\(\s*\d+\/\d+\)\s+)?(\S+)\s+(\S+)\s*$/;

export function parseNextestLog(text) {
	const summary = text.match(/(\d+)\s+tests run:\s*(\d+)\s+passed,\s*(\d+)\s+failed,\s*(\d+)\s+skipped/);
	const statuses = new Map();
	for (const line of text.split("\n")) {
		const match = line.match(NEXTEST_STATUS);
		if (match) statuses.set(`${match[2]}\u0000${match[3]}`, match[1]);
	}
	return {
		run: summary ? Number(summary[1]) : null,
		passed: summary ? Number(summary[2]) : null,
		failed: summary ? Number(summary[3]) : null,
		skipped: summary ? Number(summary[4]) : null,
		statuses,
	};
}

export function validateNextest(evidenceDir, repo, manifest, report) {
	const path = join(evidenceDir, "nextest.log");
	if (!existsSync(path)) {
		report.fail(`nextest log missing: ${path}`);
		return;
	}
	const parsed = parseNextestLog(readFileSync(path, "utf8"));
	if (parsed.run === null) {
		report.fail("nextest log has no summary line (tests did not run)");
		return;
	}
	if (parsed.run <= 0) report.fail("nextest summary reports zero tests run");
	if (parsed.failed !== 0) report.fail(`nextest summary reports ${parsed.failed} failed test(s) (expected 0)`);
	if (parsed.skipped !== 0) report.fail(`nextest summary reports ${parsed.skipped} skipped test(s) (expected 0)`);
	if (parsed.statuses.size === 0) {
		report.fail("nextest log has no per-test status lines; execution cannot be proven (run nextest with NEXTEST_STATUS_LEVEL=all)");
	}
	const required = manifest?.execution_coverage?.required_tests ?? [];
	if (required.length === 0) report.fail("manifest declares no execution_coverage.required_tests");
	for (const entry of required) {
		const label = `${entry.package}::${entry.target}::${entry.test}`;
		if (!PACKAGE_ID.test(String(entry.package ?? ""))) report.fail(`invalid package id: ${entry.package}`);
		if (!EXACT_TARGET_ID.test(String(entry.target ?? ""))) report.fail(`invalid exact target id: ${entry.target} (${label})`);
		if (!EXACT_TEST_ID.test(String(entry.test ?? ""))) {
			report.fail(`invalid exact test identifier (placeholder, not an identifier): ${JSON.stringify(entry.test)} (${label})`);
			continue;
		}
		if (!findTargetFile(repo, entry.package, entry.target)) {
			report.fail(`nonexistent target id: ${entry.package}::${entry.target} has no tests/${entry.target}.rs (or src/lib.rs for \`lib\`)`);
			continue;
		}
		const key = `${binaryIdFor(entry.package, entry.target)}\u0000${entry.test}`;
		const status = parsed.statuses.get(key);
		if (status === undefined) {
			report.fail(`required test was not executed on the assembled SHA: ${label}`);
		} else if (status !== "PASS") {
			report.fail(`required test did not pass (${status}): ${label}`);
		}
	}
}

// ---------------------------------------------------------------------------------------------
// clippy / build log sanity (exit codes come from the command manifest).
// ---------------------------------------------------------------------------------------------

function logHasCompilerError(path) {
	const text = readFileSync(path, "utf8");
	return /^\s*error(\[[A-Z0-9]+\])?:/m.test(text);
}

export function validateClippy(evidenceDir, report) {
	const path = join(evidenceDir, "clippy.log");
	if (!existsSync(path)) report.fail(`clippy log missing: ${path}`);
	else if (logHasCompilerError(path)) report.fail("clippy log contains compiler errors despite a zero exit code");
}

export function validateBuild(evidenceDir, report) {
	const path = join(evidenceDir, "build.log");
	if (!existsSync(path)) report.fail(`build log missing: ${path}`);
	else if (logHasCompilerError(path)) report.fail("build log contains compiler errors despite a zero exit code");
}

// ---------------------------------------------------------------------------------------------
// Parity: exit status + declared-vs-parsed row coverage (the task-8 hidden-row guard).
// ---------------------------------------------------------------------------------------------

function splitRow(line) {
	const t = line.trim();
	if (!t.startsWith("|") || !t.endsWith("|")) return null;
	return t.slice(1, -1).split("|").map((cell) => cell.trim());
}

/** Counts every row that belongs to a `TS path` table, INCLUDING rows after an intra-table blank. */
export function countDeclaredRows(text) {
	const lines = text.split("\n");
	let declared = 0;
	let inTable = false;
	let headerSeen = false;
	for (let i = 0; i < lines.length; i++) {
		const cells = splitRow(lines[i]);
		if (cells) {
			const first = cells[0].toLowerCase();
			if (first === "ts path") {
				inTable = true;
				headerSeen = true;
				continue;
			}
			if (inTable) {
				if (/^-+$/.test(cells[0].replace(/[\s:]/g, ""))) continue; // separator row
				declared += 1;
			}
			continue;
		}
		// A blank line inside a table does NOT end the table (that is the defect this guards).
		if (lines[i].trim() === "") continue;
		inTable = false;
	}
	return headerSeen ? declared : 0;
}

export function parityFragments(repo) {
	const fragments = [];
	const visit = (dir, depth) => {
		if (depth > 4) return;
		for (const name of readdirSync(dir)) {
			if (name === "target" || name === ".git") continue;
			const abs = join(dir, name);
			let isDir = false;
			try {
				isDir = statSync(abs).isDirectory();
			} catch {
				continue;
			}
			if (isDir) visit(abs, depth + 1);
			else if (name === "parity.d") fragments.push(abs);
		}
	};
	const crates = join(repo, "crates");
	if (!existsSync(crates)) return fragments;
	const collect = (dir, depth) => {
		if (depth > 5) return;
		for (const name of readdirSync(dir)) {
			if (name === "target" || name === ".git") continue;
			const abs = join(dir, name);
			let isDir = false;
			try {
				isDir = statSync(abs).isDirectory();
			} catch {
				continue;
			}
			if (!isDir) continue;
			if (name === "parity.d") fragments.push(abs);
			else collect(abs, depth + 1);
		}
	};
	collect(crates, 0);
	return fragments;
}

export function validateParity(evidenceDir, repo, report) {
	for (const log of ["parity-self-test.log", "parity-all.log"]) {
		if (!existsSync(join(evidenceDir, log))) report.fail(`parity log missing: ${log}`);
	}
	for (const dir of parityFragments(repo)) {
		for (const file of readdirSync(dir).filter((name) => /^\d+\.md$/.test(name))) {
			const abs = join(dir, file);
			const text = readFileSync(abs, "utf8");
			let parsed;
			try {
				parsed = parseFragment(text, relative(repo, abs)).length;
			} catch (error) {
				report.fail(`parity fragment fails to parse: ${relative(repo, abs)}: ${error.message}`);
				continue;
			}
			const declared = countDeclaredRows(text);
			if (declared > parsed) {
				report.fail(
					`PARSE-HIDDEN-ROW: ${relative(repo, abs)} declares ${declared} row(s) but the parser reads ${parsed} ` +
						`(${declared - parsed} hidden behind an intra-table blank line)`,
				);
			}
		}
	}
	const task8 = join(repo, TASK8_LEDGER);
	if (!existsSync(task8)) {
		report.fail(`task-8 ledger missing: ${TASK8_LEDGER} (its rows are required coverage, not optional)`);
	} else {
		const text = readFileSync(task8, "utf8");
		const declared = countDeclaredRows(text);
		const parsed = parseFragment(text, TASK8_LEDGER).length;
		if (declared !== parsed) report.fail(`task-8 ledger 38.md row coverage mismatch (declared ${declared}, parsed ${parsed})`);
		if (declared === 0) report.fail(`task-8 ledger ${TASK8_LEDGER} declares no rows`);
	}
}

// ---------------------------------------------------------------------------------------------
// QA artifacts.
// ---------------------------------------------------------------------------------------------

export function validateQa(evidenceDir, sha, manifest, report) {
	const root = join(evidenceDir, "qa");
	const summaryPath = join(root, "residual-qa.json");
	if (!existsSync(summaryPath)) {
		report.fail(`QA summary missing: ${relative(evidenceDir, summaryPath)}`);
		return;
	}
	let summary;
	try {
		summary = readJson(summaryPath);
	} catch (error) {
		report.fail(`QA summary is not valid JSON: ${error.message}`);
		return;
	}
	if (summary.schema !== QA_SCHEMA) report.fail(`QA summary schema "${summary.schema}" != "${QA_SCHEMA}"`);
	if (summary.sha !== sha) report.fail(`QA summary sha ${summary.sha} != gate SHA ${sha}`);
	// Bind the QA to the ACTUAL staged binary artifact, not just a self-reported hash.
	const staged = join(evidenceDir, "install", "mhc");
	if (!SHA256_HEX.test(String(summary.binary_sha256 ?? ""))) {
		report.fail("QA summary binary_sha256 is not a 64-hex hash");
	} else if (existsSync(staged) && summary.binary_sha256 !== sha256(readFileSync(staged))) {
		report.fail("QA summary binary_sha256 != the staged $E/install/mhc artifact hash");
	} else if (!existsSync(staged)) {
		report.fail("QA cannot bind binary_sha256: $E/install/mhc is missing");
	}
	for (const name of REQUIRED_SCENARIOS) {
		const scenario = summary.scenarios?.[name];
		if (!scenario) {
			report.fail(`QA scenario missing from summary: ${name}`);
			continue;
		}
		if (scenario.status !== "pass") {
			report.fail(`QA scenario ${name} is not pass (status=${scenario.status}${scenario.blocker ? `; blocker=${scenario.blocker}` : ""})`);
		}
		if (scenario.cleanup_ok !== true) report.fail(`QA scenario ${name} has no clean cleanup receipt`);
		if (!Array.isArray(scenario.artifacts) || scenario.artifacts.length === 0) {
			report.fail(`QA scenario ${name} records no artifacts (proof semantics missing)`);
		} else {
			for (const artifact of scenario.artifacts) {
				if (!existsSync(join(root, artifact)) && !existsSync(join(evidenceDir, artifact))) {
					report.fail(`QA scenario ${name} artifact missing: ${artifact}`);
				}
			}
		}
	}
	// The mandatory TUI matrix is asserted independently of the manifest's own arrays.
	for (const geometry of TUI_GEOMETRIES) {
		for (const mode of TUI_MODES) {
			validateTuiCase(join(root, `tui-${geometry}-${mode}`), geometry, mode, report);
		}
	}
	validateThemeCases(root, report);
	// The frozen run-qa harness must have shown its own RPC-id and help-flag-set sentinels.
	const runQa = join(evidenceDir, "run-qa.log");
	if (!existsSync(runQa)) report.fail("run-qa.log missing (RPC ids + help-49 equality not proven)");
	else {
		const text = readFileSync(runQa, "utf8");
		if (!text.includes("RPC_IDS_PASS")) report.fail("run-qa.log does not contain RPC_IDS_PASS");
		if (!text.includes("HELP_FLAG_SET_MATCH")) report.fail("run-qa.log does not contain HELP_FLAG_SET_MATCH (help flag-set equality)");
	}
}

/** The theme scenario must prove real display evidence: the custom accent rendered AND the malformed
 *  theme fell back to a working TUI with a diagnostic. */
function validateThemeCases(root, report) {
	const custom = join(root, "theme-custom", "evidence.json");
	const fallback = join(root, "theme-fallback", "evidence.json");
	if (!existsSync(custom)) report.fail("theme: theme-custom/evidence.json missing");
	else {
		const evidence = readJson(custom);
		if (evidence.theme_accent_rendered !== true) report.fail("theme: the custom theme accent was not rendered (no display evidence)");
		if (evidence.startup_predicate !== true) report.fail("theme: the custom-theme TUI did not reach the idle editor prompt");
	}
	if (!existsSync(fallback)) report.fail("theme: theme-fallback/evidence.json missing");
	else {
		const evidence = readJson(fallback);
		if (evidence.theme_fallback_diagnostic !== true) report.fail("theme: a malformed theme produced no fallback diagnostic");
		if (evidence.startup_predicate !== true) report.fail("theme: the malformed-theme TUI did not fall back to a working screen");
	}
}

function validateTuiCase(dir, geometry, mode, report) {
	const label = `tui-${geometry}-${mode}`;
	const evidencePath = join(dir, "evidence.json");
	if (!existsSync(evidencePath)) {
		report.fail(`TUI evidence missing: ${label}/evidence.json`);
		return;
	}
	let evidence;
	try {
		evidence = readJson(evidencePath);
	} catch (error) {
		report.fail(`${label}/evidence.json is not valid JSON: ${error.message}`);
		return;
	}
	for (const field of TUI_REQUIRED_FIELDS) {
		if (evidence[field] !== true) report.fail(`${label}: ${field} is not true (evidence=${JSON.stringify(evidence[field])})`);
	}
	if (evidence.reply_after_prompt !== true) {
		report.fail(`${label}: reply did not follow a real typed prompt (onboarding-only reply is not accepted)`);
	}
	if (evidence.exit_code !== 0) report.fail(`${label}: process exit ${evidence.exit_code}`);
	if (evidence.geometry !== geometry) report.fail(`${label}: geometry mismatch (${evidence.geometry})`);
	if (evidence.mode !== mode) report.fail(`${label}: mode mismatch (${evidence.mode})`);
	const [cols, rows] = geometry.split("x").map(Number);
	const pngPath = join(dir, "terminal.png");
	if (!existsSync(pngPath)) {
		report.fail(`${label}: terminal.png missing`);
		return;
	}
	const png = readFileSync(pngPath);
	if (png.length < 24) {
		report.fail(`${label}: terminal.png is only ${png.length} bytes (truncated)`);
		return;
	}
	if (!png.subarray(0, 8).equals(PNG_SIGNATURE)) report.fail(`${label}: terminal.png has no PNG signature`);
	const width = png.readUInt32BE(16);
	const height = png.readUInt32BE(20);
	if (width !== cols * 8 || height !== rows * 16) {
		report.fail(`${label}: terminal.png is ${width}x${height}, expected ${cols * 8}x${rows * 16}`);
	}
	const sidecar = join(dir, "terminal.png.json");
	if (existsSync(sidecar)) {
		const meta = readJson(sidecar);
		if (meta.schema !== PNG_SCHEMA) report.fail(`${label}: png sidecar schema mismatch`);
		if (meta.derived !== true) report.fail(`${label}: png sidecar is not marked derived (must not claim a literal screenshot)`);
		if (meta.colored !== true) report.fail(`${label}: png sidecar is not color-faithful (per-cell fg/bg missing)`);
		if (meta.cols !== cols || meta.rows !== rows) report.fail(`${label}: png sidecar geometry mismatch`);
		if (!SHA256_HEX.test(String(meta.pngSha256 ?? ""))) report.fail(`${label}: png sidecar pngSha256 is not a 64-hex hash`);
		else if (meta.pngSha256 !== sha256(png)) report.fail(`${label}: terminal.png changed since the sidecar hash`);
	} else {
		report.fail(`${label}: terminal.png.json sidecar missing`);
	}
	if (!existsSync(join(dir, "reply-cells.json"))) report.fail(`${label}: reply-cells.json missing (no per-cell color/position provenance)`);
}

// ---------------------------------------------------------------------------------------------
// Unresolved / not-started scopes must block green unless explicitly dispositioned.
// ---------------------------------------------------------------------------------------------

export function validateUnresolved(manifest, report) {
	const reasonOf = (entry) => (typeof entry.reason === "string" ? entry.reason : typeof entry.note === "string" ? entry.note : "");
	for (const entry of manifest?.unresolved ?? []) {
		if (!ALLOWED_DISPOSITIONS.has(entry.disposition)) {
			report.fail(`unresolved scope has no accepted disposition (silently skipped): ${entry.id} (${entry.kind ?? "?"})`);
		} else if (reasonOf(entry).trim() === "") {
			report.fail(`unresolved scope ${entry.id} disposition "${entry.disposition}" has no reason`);
		}
	}
	for (const entry of manifest?.not_started ?? []) {
		// Post-gate scopes are recorded but not required pre-gate (no circular requirement).
		if (POST_GATE_SCOPES.has(entry.id)) continue;
		if (!ALLOWED_DISPOSITIONS.has(entry.disposition)) {
			report.fail(`not-started scope still blocks completion: ${entry.id} (${entry.kind ?? "?"})`);
		} else if (reasonOf(entry).trim() === "") {
			report.fail(`not-started scope ${entry.id} disposition "${entry.disposition}" has no reason`);
		}
	}
	// Awaiting categories are pending exact executed test names: they must not silently satisfy a
	// requirement. Each entry must carry an accepted disposition (a landed receipt resolves it).
	for (const entry of manifest?.awaited_executed_tests?.items ?? []) {
		if (!ALLOWED_DISPOSITIONS.has(entry.disposition)) {
			report.fail(`awaited-executed category not resolved (cannot silently satisfy a requirement): ${entry.category} (${entry.requirement ?? "?"})`);
		} else if (reasonOf(entry).trim() === "") {
			report.fail(`awaited-executed category ${entry.category} disposition "${entry.disposition}" has no reason`);
		}
	}
	// Required-but-not-authored entries must link an exact authored test that is ALSO present as an
	// exact (package, target, test) triple in execution_coverage.required_tests, so an orphan fn
	// identifier cannot satisfy the requirement; validateNextest then proves that triple executed PASS.
	const requiredTriples = new Set(
		(manifest?.execution_coverage?.required_tests ?? []).map((entry) => `${entry.package}\u0000${entry.target}\u0000${entry.test}`),
	);
	for (const entry of manifest?.awaited_executed_tests?.required_but_not_authored ?? []) {
		const label = JSON.stringify(entry.id ?? entry.category ?? entry.test ?? entry);
		const packageId = String(entry.package ?? "");
		const targetId = String(entry.target ?? "");
		const testId = String(entry.test ?? "");
		if (entry.authored !== true || !PACKAGE_ID.test(packageId) || !EXACT_TARGET_ID.test(targetId) || !EXACT_TEST_ID.test(testId)) {
			report.fail(`required-but-not-authored entry has no exact authored (package, target, test): ${label}`);
			continue;
		}
		if (!requiredTriples.has(`${packageId}\u0000${targetId}\u0000${testId}`)) {
			report.fail(`required-but-not-authored entry is an orphan test not linked into execution_coverage.required_tests: ${label}`);
		}
	}
	// Completeness against the fixed approved requirement IDs.
	const present = new Set((manifest?.requirements ?? []).map((requirement) => requirement.id));
	for (const id of REQUIRED_REQUIREMENT_IDS) {
		if (!present.has(id)) report.fail(`manifest omits approved requirement id ${id} (incomplete coverage)`);
	}
	// A claimed pass must be backed by the evidence, never self-derived.
	const knownScenarios = new Set(REQUIRED_SCENARIOS);
	for (const requirement of manifest?.requirements ?? []) {
		if (requirement.proof === "pass" && Array.isArray(requirement.scenarios)) {
			for (const scenario of requirement.scenarios) {
				if (scenario === "all") continue;
				if (!knownScenarios.has(scenario)) {
					report.fail(`requirement ${requirement.id} claims proof=pass for unknown scenario ${scenario}`);
				}
			}
		}
	}
}

/** An empty `src/lib.rs` is a silent gap unless the manifest records an approved exclusion with
 *  evidence (e.g. the quiet-model-profile bootstrap crate: no pinned source, no consumer). */
/** The server row map (task-12 accepted partials) must be machine-enforced, not decorative: every
 *  accepted row is present; every non-excluded row cites exact (package, target, test) triples that
 *  resolve to a real target file AND are linked into execution_coverage.required_tests (so the same
 *  nextest execution machinery proves them PASS - no parallel list); approved-exclusion rows cite no
 *  tests; row 25's cross-crate MCP test is typed maho-ext-mcp, never a wrong maho-server assertion;
 *  and any declared uncovered_behavior blocks success until resolved. */
export function validateServerRows(manifest, repo, report) {
	const server = manifest?.server_row_manifest;
	if (!server || typeof server !== "object") {
		report.fail("manifest has no server_row_manifest (accepted server rows are unenforced)");
		return;
	}
	const rows = Array.isArray(server.rows) ? server.rows : [];
	if (rows.length === 0) {
		report.fail("server_row_manifest has no rows");
		return;
	}
	const requiredTriples = new Set(
		(manifest?.execution_coverage?.required_tests ?? []).map((entry) => `${entry.package}\u0000${entry.target}\u0000${entry.test}`),
	);
	const present = new Set();
	for (const row of rows) {
		const label = `server row ${row.id} (${row.ts ?? "?"})`;
		if (present.has(row.id)) report.fail(`duplicate server row id: ${row.id}`);
		present.add(row.id);
		const trips = Array.isArray(row.required_tests) ? row.required_tests : [];
		if (row.status === "approved-exclusion") {
			if (trips.length !== 0) report.fail(`${label} is approved-exclusion but cites ${trips.length} test(s)`);
			continue;
		}
		if (trips.length === 0) {
			report.fail(`${label} is not an approved-exclusion but cites no exact required_tests`);
			continue;
		}
		for (const entry of trips) {
			const pkg = String(entry.package ?? "");
			const tgt = String(entry.target ?? "");
			const test = String(entry.test ?? "");
			const triple = `${pkg}::${tgt}::${test}`;
			if (!PACKAGE_ID.test(pkg) || !EXACT_TARGET_ID.test(tgt) || !EXACT_TEST_ID.test(test)) {
				report.fail(`invalid server row triple: ${label} -> ${triple}`);
				continue;
			}
			if (!findTargetFile(repo, pkg, tgt)) {
				report.fail(`server row triple names a nonexistent target: ${label} -> ${triple}`);
				continue;
			}
			if (!requiredTriples.has(`${pkg}\u0000${tgt}\u0000${test}`)) {
				report.fail(`server row triple is not linked into execution_coverage.required_tests: ${label} -> ${triple}`);
			}
		}
	}
	for (const id of SERVER_ROW_IDS) {
		if (!present.has(id)) report.fail(`server_row_manifest omits accepted task-12 row ${id} (incomplete coverage)`);
	}
	// Row 25 is cross-crate: the MCP lifecycle_registration test must be typed maho-ext-mcp, never a
	// wrong maho-server assertion.
	const row25 = rows.find((row) => row.id === 25);
	if (row25) {
		const mcp = (row25.required_tests ?? []).filter((entry) => String(entry.test ?? "").includes("session_start_attach_publishes_live_mcp_status"));
		if (mcp.length === 0) {
			report.fail("server row 25 does not type the cross-crate MCP lifecycle_registration test");
		}
		for (const entry of mcp) {
			if (entry.package !== "maho-ext-mcp" || entry.target !== "lifecycle_registration") {
				report.fail(`server row 25 MCP test must be typed maho-ext-mcp::lifecycle_registration (got ${entry.package}::${entry.target})`);
			}
		}
	}
	// Declared uncovered behavior blocks success until it is resolved (no silent gap).
	for (const entry of Array.isArray(server.uncovered_behavior) ? server.uncovered_behavior : []) {
		report.fail(`server_row_manifest.uncovered_behavior unresolved (blocks completion): ${entry?.id ?? entry?.detail ?? JSON.stringify(entry)}`);
	}
}

export function validateEmptyLibExports(manifest, repo, report) {
	// Discover the ACTUAL empty (<=1 byte) `src/lib.rs` files across workspace members, so an omitted
	// entry cannot escape: every empty lib must be recorded with an approved exclusion + evidence.
	const recorded = new Map();
	for (const entry of manifest?.empty_lib_exports?.remaining ?? []) {
		if (entry.path) recorded.set(entry.path.replace(/^crates\//, "").replace(/^\/+/, ""), entry);
	}
	const discovered = new Set();
	for (const [name, manifestPath] of packageManifests(repo)) {
		const lib = join(dirname(manifestPath), "src", "lib.rs");
		if (!existsSync(lib)) continue;
		if (statSync(lib).size <= 1) discovered.add(name);
	}
	for (const [name, manifestPath] of packageManifests(repo)) {
		const lib = join(dirname(manifestPath), "src", "lib.rs");
		if (!existsSync(lib) || statSync(lib).size > 1) continue;
		const rel = relative(repo, lib);
		const entry = recorded.get(rel) ?? recorded.get(rel.replace(/^crates\//, ""));
		if (!entry) {
			report.fail(`empty lib ${rel} is not recorded in empty_lib_exports (unexplained silent gap)`);
			continue;
		}
		if (entry.verdict !== "approved-exclusion") report.fail(`empty lib ${rel} has no approved exclusion verdict`);
		else if (typeof entry.evidence !== "string" || entry.evidence.trim() === "") report.fail(`empty lib ${rel} approved-exclusion has no evidence`);
	}
	for (const [path, entry] of recorded) {
		const abs = join(repo, path.startsWith("crates/") ? path : `crates/${path}`);
		if (existsSync(abs) && statSync(abs).size > 2) report.fail(`empty_lib_exports entry ${path} is no longer empty; remove the exclusion`);
		if (!entry.evidence || String(entry.evidence).trim() === "") report.fail(`empty lib ${path} approved-exclusion has no evidence`);
	}
	if (discovered.size === 0) report.note("no empty src/lib.rs found among workspace members");
}

// ---------------------------------------------------------------------------------------------
// Top-level verification.
// ---------------------------------------------------------------------------------------------

export function verify({ repo, evidenceDir, sha, final = false }) {
	const report = new Report();
	if (!FULL_SHA.test(sha)) {
		report.fail(`--sha "${sha}" is not a full 40-hex SHA`);
		return report;
	}
	const manifest = validateBoundManifest(evidenceDir, sha, report, repo);
	if (manifest) {
		validateSourceIdentity(evidenceDir, repo, sha, report, final);
		validatePackageManifest(evidenceDir, repo, sha, manifest, report);
		validateCommandManifest(evidenceDir, sha, manifest, report, final);
		validateNextest(evidenceDir, repo, manifest, report);
		validateClippy(evidenceDir, report);
		validateBuild(evidenceDir, report);
		validateParity(evidenceDir, repo, report);
		validateQa(evidenceDir, sha, manifest, report);
		validateUnresolved(manifest, report);
		validateEmptyLibExports(manifest, repo, report);
		validateServerRows(manifest, repo, report);
	}
	return report;
}

// ---------------------------------------------------------------------------------------------
// Self-test: a valid fixture must pass; every negative fixture must fail.
// ---------------------------------------------------------------------------------------------

function writePng(path, width, height) {
	// Minimal, deterministic RGBA PNG (all-zero pixels) with a correct IHDR — enough for the
	// signature + IHDR-dimension checks. Uses node:zlib, no third-party code.
	const crcTable = (() => {
		const table = new Uint32Array(256);
		for (let n = 0; n < 256; n++) {
			let c = n;
			for (let k = 0; k < 8; k++) c = (c & 1) === 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
			table[n] = c >>> 0;
		}
		return table;
	})();
	const crc32 = (buf) => {
		let crc = 0xffffffff;
		for (let i = 0; i < buf.length; i++) crc = (crcTable[(crc ^ buf[i]) & 0xff] ?? 0) ^ (crc >>> 8);
		return (crc ^ 0xffffffff) >>> 0;
	};
	const chunk = (type, data) => {
		const typeBuf = Buffer.from(type, "ascii");
		const length = Buffer.alloc(4);
		length.writeUInt32BE(data.length, 0);
		const crc = Buffer.alloc(4);
		crc.writeUInt32BE(crc32(Buffer.concat([typeBuf, data])), 0);
		return Buffer.concat([length, typeBuf, data, crc]);
	};
	const rowBytes = width * 4;
	const raw = Buffer.alloc(height * (rowBytes + 1));
	const header = Buffer.alloc(13);
	header.writeUInt32BE(width, 0);
	header.writeUInt32BE(height, 4);
	header[8] = 8;
	header[9] = 6;
	writeFileSync(
		path,
		Buffer.concat([
			PNG_SIGNATURE,
			chunk("IHDR", header),
			chunk("IDAT", deflateSync(raw)),
			chunk("IEND", Buffer.alloc(0)),
		]),
	);
}

function buildValidFixture(root, sha) {
	const repo = join(root, "repo");
	const evidence = join(root, "evidence");
	const put = (path, text) => {
		mkdirSync(dirname(path), { recursive: true });
		writeFileSync(path, text);
	};
	put(join(repo, "Cargo.toml"), '[workspace]\nmembers = [\n  "crates/maho-cli",\n  "crates/maho-core",\n  "crates/maho-test-support",\n  "crates/extensions/maho-ext-quiet-model-profile",\n  "crates/builtins/maho-ext-mcp",\n]\n');
	put(join(repo, "crates/maho-cli/Cargo.toml"), '[package]\nname = "maho-cli"\n');
	put(join(repo, "crates/maho-cli/tests/mini_worker.rs"), "// fixture\n");
	put(
		join(repo, "crates/maho-cli/parity.d/38.md"),
		"| TS path | Rust path | TS tests | Rust tests | status |\n|---|---|---:|---:|---|\n| a.ts | src/a.rs | 1 | 1 | done |\n",
	);
	put(join(repo, "crates/maho-test-support/Cargo.toml"), '[package]\nname = "maho-test-support"\n');
	put(join(repo, "crates/maho-test-support/tests/faux_native_session.rs"), "// fixture\n");
	// A lib-target package (unit tests live under `src/lib.rs` with `module::tests::fn` ids).
	put(join(repo, "crates/maho-core/Cargo.toml"), '[package]\nname = "maho-core"\n');
	put(join(repo, "crates/maho-core/src/lib.rs"), "pub mod provider_account_events {\n    #[cfg(test)]\n    pub mod tests {\n        #[test]\n        fn the_global_registry_delivers_then_stops_after_unsubscribe() {}\n    }\n}\n");
	// A one-byte lib with an approved exclusion (must be recorded, not silently skipped).
	put(join(repo, "crates/extensions/maho-ext-quiet-model-profile/Cargo.toml"), '[package]\nname = "maho-ext-quiet-model-profile"\n');
	put(join(repo, "crates/extensions/maho-ext-quiet-model-profile/src/lib.rs"), "\n");
	put(join(repo, "crates/builtins/maho-ext-mcp/Cargo.toml"), '[package]\nname = "maho-ext-mcp"\n');
	put(join(repo, "crates/builtins/maho-ext-mcp/tests/lifecycle_registration.rs"), "// fixture\n");
	// The committed machine-consumable template the bound copy must be derived from.
	const templateText = JSON.stringify({ schema: SCHEMA, produced_by: "task-17", gate_commands: [] }, null, 2) + "\n";
	put(join(repo, TEMPLATE_PATH), templateText);

	const cliManifest = join(repo, "crates/maho-cli/Cargo.toml");
	const supportManifest = join(repo, "crates/maho-test-support/Cargo.toml");
	const coreManifest = join(repo, "crates/maho-core/Cargo.toml");
	const quietManifest = join(repo, "crates/extensions/maho-ext-quiet-model-profile/Cargo.toml");
	const mcpManifest = join(repo, "crates/builtins/maho-ext-mcp/Cargo.toml");
	const gateCommands = [
		"cargo4 nextest run --workspace --no-fail-fast",
		"cargo4 clippy --workspace --all-targets --keep-going -- -D warnings",
		"cargo4 build --workspace --bins",
		"SENPI_SRC=/home/indo/code/senpi OMO_SRC=/home/indo/code/oh-my-openagent bun tools/parity-audit.mjs --self-test",
		"SENPI_SRC=/home/indo/code/senpi OMO_SRC=/home/indo/code/oh-my-openagent bun tools/parity-audit.mjs --all",
		'bun tools/package-native.mjs --binary "$BIN" --output "$E/install"',
		'bash .omo/evidence/session2-final/run-qa.sh "$BIN" "$SHA"',
		'bun tools/residual-qa.mjs --binary "$BIN" --installed "$E/install/mhc" --scenario all --evidence "$E"',
		"bun tools/verify-session2-residual.mjs --self-test",
		'bun tools/verify-session2-residual.mjs --evidence "$E" --sha "$SHA"',
	];
	const manifest = {
		schema: SCHEMA,
		binding: { assembled_sha: sha, candidate_base: "e57e1bd26760340d2b7640b917af557b2eb86e83" },
		bound_from: { source_path: TEMPLATE_PATH, source_sha256: sha256(templateText) },
		gate_commands: gateCommands,
		scenarios: { tui: { geometries: ["80x24"], modes: ["regular"] } },
		execution_coverage: {
			required_tests: [
				{ package: "maho-test-support", target: "faux_native_session", test: "native_handle_drives_a_prompt_and_closes_cleanly", requirement: "SDK-NATIVE-SESSION" },
				{ package: "maho-cli", target: "mini_worker", test: "unwatch_and_close_release_watch_subscriptions", requirement: "G7" },
				{ package: "maho-core", target: "lib", test: "provider_account_events::tests::the_global_registry_delivers_then_stops_after_unsubscribe", requirement: "G12" },
				{ package: "maho-ext-mcp", target: "lifecycle_registration", test: "session_start_attach_publishes_live_mcp_status_to_the_bound_subscriber_and_stops_after_unsubscribe", requirement: "G9" },
			],
		},
		requirements: REQUIRED_REQUIREMENT_IDS.map((id) => ({ id, proof: "unrun", scenarios: [], packages: id === "G7" ? ["maho-cli"] : [] })),
		awaited_executed_tests: { items: [] },
		unresolved: [{ id: "IsInContentFullscreen", kind: "unresolved-literal", disposition: "accepted-exclusion", reason: "no located symbol in pinned senpi or Rust tree" }],
		not_started: [{ id: "task-20", kind: "task", disposition: "downstream", reason: "post-gate integration" }],
		empty_lib_exports: {
			remaining: [
				{
					path: "crates/extensions/maho-ext-quiet-model-profile/src/lib.rs",
					size_bytes: 1,
					verdict: "approved-exclusion",
					evidence: "parity.d/40.md records n/a: no pinned source and no consumer",
				},
			],
		},
		server_row_manifest: {
			rows: Array.from({ length: 54 }, (_, index) => {
				const id = index === 53 ? 710 : index + 1;
				return {
					id,
					ts: `row-${id}.ts`,
					module: "m",
					status: "done",
					required_tests:
						id === 25
							? [{ package: "maho-ext-mcp", target: "lifecycle_registration", test: "session_start_attach_publishes_live_mcp_status_to_the_bound_subscriber_and_stops_after_unsubscribe" }]
							: [{ package: "maho-cli", target: "mini_worker", test: "unwatch_and_close_release_watch_subscriptions" }],
				};
			}),
			uncovered_behavior: [],
		},
	};
	put(join(evidence, "requirements-manifest.json"), JSON.stringify(manifest, null, 2));
	put(join(evidence, "run-identity.json"), JSON.stringify({ schema: "session2-residual-run-identity/v1", sha }, null, 2));
	put(join(evidence, "source-identity.json"), JSON.stringify({ schema: "session2-residual-source-identity/v1", sha, head: sha, head_matches_sha: true, status_porcelain: "", dirty_sources: [], clean: true }, null, 2));
	put(join(evidence, "source-identity-end.json"), JSON.stringify({ schema: "session2-residual-source-identity/v1", sha, head: sha, head_matches_sha: true, status_porcelain: "", dirty_sources: [], clean: true }, null, 2));
	put(
		join(evidence, "package-manifest.json"),
		JSON.stringify(
			{
				schema: PACKAGE_SCHEMA,
				sha,
				packages: [
					{ name: "maho-cli", manifest_path: "crates/maho-cli/Cargo.toml", sha256: sha256(readFileSync(cliManifest)) },
					{ name: "maho-test-support", manifest_path: "crates/maho-test-support/Cargo.toml", sha256: sha256(readFileSync(supportManifest)) },
					{ name: "maho-core", manifest_path: "crates/maho-core/Cargo.toml", sha256: sha256(readFileSync(coreManifest)) },
					{ name: "maho-ext-quiet-model-profile", manifest_path: "crates/extensions/maho-ext-quiet-model-profile/Cargo.toml", sha256: sha256(readFileSync(quietManifest)) },
					{ name: "maho-ext-mcp", manifest_path: "crates/builtins/maho-ext-mcp/Cargo.toml", sha256: sha256(readFileSync(mcpManifest)) },
				],
			},
			null,
			2,
		),
	);
	const commands = gateCommands.map((template, index) => {
		const self = template.includes(VERIFY_COMMAND_MARKER);
		if (self) return { template, command: template.replace("cargo4", "/cargo4"), exit_code: null, log: null };
		const log = `command-${String(index + 1).padStart(2, "0")}.log`;
		const bytes = Buffer.from(`${template}\nexit 0\n`);
		put(join(evidence, log), bytes);
		return { template, command: template.replace("cargo4", "/cargo4"), exit_code: 0, log, log_sha256: sha256(bytes) };
	});
	put(join(evidence, "command-manifest.json"), JSON.stringify({ schema: COMMAND_SCHEMA, sha, commands }, null, 2));
	put(
		join(evidence, "nextest.log"),
		[
			"   Starting 4 tests across 4 binaries",
			"        PASS [   0.001s] (   1/4) maho-test-support::faux_native_session native_handle_drives_a_prompt_and_closes_cleanly",
			"        PASS [   0.002s] (   2/4) maho-cli::mini_worker unwatch_and_close_release_watch_subscriptions",
			"        PASS [   0.003s] (   3/4) maho-core provider_account_events::tests::the_global_registry_delivers_then_stops_after_unsubscribe",
			"        PASS [   0.004s] (   4/4) maho-ext-mcp::lifecycle_registration session_start_attach_publishes_live_mcp_status_to_the_bound_subscriber_and_stops_after_unsubscribe",
			"   Summary [   0.010s] 4 tests run: 4 passed, 0 failed, 0 skipped",
			"",
		].join("\n"),
	);
	put(join(evidence, "clippy.log"), "    Finished `clippy` profile\n");
	put(join(evidence, "build.log"), "    Finished `dev` profile\n");
	put(join(evidence, "parity-self-test.log"), "self-test: 42/42 passed\n");
	put(join(evidence, "parity-all.log"), "audit: 0 problems\n");
	put(join(evidence, "run-qa.log"), "RPC_IDS_PASS\nHELP_FLAG_SET_MATCH\nQA_ALL_EXIT=0\n");
	// The staged binary the QA summary must bind by hash.
	put(join(evidence, "install/mhc"), "#!/bin/sh\nfixture binary\n");
	const stagedHash = sha256(readFileSync(join(evidence, "install/mhc")));
	const qaScenarios = {};
	for (const name of REQUIRED_SCENARIOS) qaScenarios[name] = { status: "pass", cleanup_ok: true, artifacts: [`scenario-${name}.log`] };
	for (const name of REQUIRED_SCENARIOS) put(join(evidence, `qa/scenario-${name}.log`), `${name} pass\n`);
	put(join(evidence, "qa/residual-qa.json"), JSON.stringify({ schema: QA_SCHEMA, sha, binary_sha256: stagedHash, scenarios: qaScenarios }, null, 2));
	// The mandatory TUI matrix: every geometry x mode.
	for (const geometry of TUI_GEOMETRIES) {
		for (const mode of TUI_MODES) {
			const [cols, rows] = geometry.split("x").map(Number);
			const dir = join(evidence, `qa/tui-${geometry}-${mode}`);
			mkdirSync(dir, { recursive: true });
			const ev = { geometry, mode, exit_code: 0 };
			for (const field of TUI_REQUIRED_FIELDS) ev[field] = true;
			put(join(dir, "evidence.json"), JSON.stringify(ev, null, 2));
			writePng(join(dir, "terminal.png"), cols * 8, rows * 16);
			put(join(dir, "terminal.png.json"), JSON.stringify({ schema: PNG_SCHEMA, cols, rows, derived: true, colored: true, pngSha256: sha256(readFileSync(join(dir, "terminal.png"))) }, null, 2));
			put(join(dir, "reply-cells.json"), JSON.stringify({ cols, rows, cells: [{ ch: "x", fg: null, bg: null, w: 1, col: 0, row: 0 }] }));
		}
	}
	put(join(evidence, "qa/theme-custom/evidence.json"), JSON.stringify({ theme_accent_rendered: true, startup_predicate: true }, null, 2));
	put(join(evidence, "qa/theme-fallback/evidence.json"), JSON.stringify({ theme_fallback_diagnostic: true, startup_predicate: true }, null, 2));
	return { repo, evidence };
}

function expectValid() {
	const root = mkdtempSync(join(tmpdir(), "verify-session2-valid-"));
	try {
		const sha = "a".repeat(40);
		const { repo, evidence } = buildValidFixture(root, sha);
		const report = verify({ repo, evidenceDir: evidence, sha });
		return { ok: report.problems.length === 0, detail: report.problems.join("; ") };
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
}

function mutate(fn) {
	const root = mkdtempSync(join(tmpdir(), "verify-session2-self-"));
	try {
		const sha = "a".repeat(40);
		const { repo, evidence } = buildValidFixture(root, sha);
		const base = verify({ repo, evidenceDir: evidence, sha });
		if (base.problems.length !== 0) {
			return { ok: false, detail: `valid fixture unexpectedly failed: ${base.problems.join("; ")}` };
		}
		const target = fn({ repo, evidence, sha, read: readFileSync, write: writeFileSync, join, readJson });
		const mutated = verify({ repo, evidenceDir: evidence, sha: target.sha ?? sha });
		return { ok: mutated.problems.length > 0, detail: mutated.problems.join("; ") };
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
}

export function selfTest() {
	const results = [];
	const check = (name, outcome) => {
		results.push({ name, ok: outcome.ok });
		console.log(`${outcome.ok ? "ok  " : "FAIL"} ${name}${outcome.ok ? "" : ` -> ${outcome.detail}`}`);
	};
	check("valid fixture passes", expectValid());
	check("null SHA rejected", mutate(({ evidence, read, write, join }) => {
		const m = JSON.parse(read(join(evidence, "requirements-manifest.json"), "utf8"));
		m.binding.assembled_sha = null;
		write(join(evidence, "requirements-manifest.json"), JSON.stringify(m));
	}));
	check("stale SHA rejected", mutate(({ evidence, read, write, join }) => {
		const m = JSON.parse(read(join(evidence, "requirements-manifest.json"), "utf8"));
		m.binding.assembled_sha = "b".repeat(40);
		write(join(evidence, "requirements-manifest.json"), JSON.stringify(m));
	}));
	check("nonzero child exit rejected", mutate(({ evidence, read, write, join }) => {
		const c = JSON.parse(read(join(evidence, "command-manifest.json"), "utf8"));
		c.commands[0].exit_code = 101;
		write(join(evidence, "command-manifest.json"), JSON.stringify(c));
	}));
	check("skipped test rejected", mutate(({ evidence, read, write, join }) => {
		const text = read(join(evidence, "nextest.log"), "utf8").replace(
			"4 tests run: 4 passed, 0 failed, 0 skipped",
			"4 tests run: 3 passed, 0 failed, 1 skipped",
		).replace("PASS [   0.002s] (   2/4)", "SKIP [   0.002s] (   2/4)");
		write(join(evidence, "nextest.log"), text);
	}));
	check("discovered-but-not-executed required test rejected", mutate(({ evidence, read, write, join }) => {
		const text = read(join(evidence, "nextest.log"), "utf8").replace(
			/^\s*PASS \[[^\]]*\] \(\s*\d+\/\d+\) maho-cli::mini_worker unwatch_and_close_release_watch_subscriptions.*\n/m,
			"",
		);
		write(join(evidence, "nextest.log"), text);
	}));
	check("lib-target test not executed rejected", mutate(({ evidence, read, write, join }) => {
		const text = read(join(evidence, "nextest.log"), "utf8").replace(
			/^\s*PASS \[[^\]]*\] \(\s*\d+\/\d+\) maho-core provider_account_events::tests::the_global_registry_delivers_then_stops_after_unsubscribe.*\n/m,
			"",
		);
		write(join(evidence, "nextest.log"), text);
	}));
	check("empty lib without approved exclusion rejected", mutate(({ evidence, read, write, join }) => {
		const m = JSON.parse(read(join(evidence, "requirements-manifest.json"), "utf8"));
		m.empty_lib_exports.remaining[0].verdict = "";
		write(join(evidence, "requirements-manifest.json"), JSON.stringify(m));
	}));
	check("stale template binding rejected", mutate(({ evidence, read, write, join }) => {
		const m = JSON.parse(read(join(evidence, "requirements-manifest.json"), "utf8"));
		m.bound_from.source_sha256 = "c".repeat(64);
		write(join(evidence, "requirements-manifest.json"), JSON.stringify(m));
	}));
	check("placeholder exact test identifier rejected", mutate(({ evidence, read, write, join }) => {
		const m = JSON.parse(read(join(evidence, "requirements-manifest.json"), "utf8"));
		m.execution_coverage.required_tests.push({
			package: "maho-cli",
			target: "startup",
			test: "tests/startup.rs theme matrix (custom/missing/invalid + pinned fallback)",
			requirement: "G2",
		});
		write(join(evidence, "requirements-manifest.json"), JSON.stringify(m));
	}));
	check("nonexistent target id rejected", mutate(({ evidence, read, write, join }) => {
		const m = JSON.parse(read(join(evidence, "requirements-manifest.json"), "utf8"));
		m.execution_coverage.required_tests.push({ package: "maho-cli", target: "does_not_exist", test: "some_case", requirement: "G2" });
		write(join(evidence, "requirements-manifest.json"), JSON.stringify(m));
	}));
	check("onboarding-only TUI reply rejected", mutate(({ evidence, read, write, join }) => {
		const p = join(evidence, "qa/tui-80x24-regular/evidence.json");
		const e = JSON.parse(read(p, "utf8"));
		e.reply_after_prompt = false;
		write(p, JSON.stringify(e));
	}));
	check("missing gated-stream evidence rejected", mutate(({ evidence, read, write, join }) => {
		const p = join(evidence, "qa/tui-80x24-regular/evidence.json");
		const e = JSON.parse(read(p, "utf8"));
		e.gated_stream_visible_while_held = false;
		write(p, JSON.stringify(e));
	}));
	check("theme without display evidence rejected", mutate(({ evidence, read, write, join }) => {
		const p = join(evidence, "qa/theme-custom/evidence.json");
		const e = JSON.parse(read(p, "utf8"));
		e.theme_accent_rendered = false;
		write(p, JSON.stringify(e));
	}));
	check("unresolved scope without disposition rejected", mutate(({ evidence, read, write, join }) => {
		const m = JSON.parse(read(join(evidence, "requirements-manifest.json"), "utf8"));
		m.unresolved.push({ id: "IsInContentFullscreen", kind: "unresolved-literal" });
		write(join(evidence, "requirements-manifest.json"), JSON.stringify(m));
	}));
	check("not-started scope blocks completion", mutate(({ evidence, read, write, join }) => {
		const m = JSON.parse(read(join(evidence, "requirements-manifest.json"), "utf8"));
		m.not_started.push({ id: "task-19", kind: "task" });
		write(join(evidence, "requirements-manifest.json"), JSON.stringify(m));
	}));
	check("awaited-executed category blocks completion", mutate(({ evidence, read, write, join }) => {
		const m = JSON.parse(read(join(evidence, "requirements-manifest.json"), "utf8"));
		m.awaited_executed_tests.items.push({ category: "Task9 mini/client", requirement: "G7", detail: "exact test names pending receipt" });
		write(join(evidence, "requirements-manifest.json"), JSON.stringify(m));
	}));
	check("PARSE-HIDDEN-ROW detected in a real fragment", mutate(({ repo, write, join }) => {
		write(
			join(repo, "crates/maho-cli/parity.d/38.md"),
			[
				"| TS path | Rust path | TS tests | Rust tests | status |",
				"|---|---|---:|---:|---|",
				"| a.ts | src/a.rs | 1 | 1 | done |",
				"",
				"| b.ts | src/b.rs | 1 | 1 | done |",
				"| c.ts | src/c.rs | 1 | 1 | done |",
				"",
			].join("\n"),
		);
	}));
	check("source identity mismatch rejected", mutate(({ evidence, write, join, sha }) => {
		write(join(evidence, "source-identity.json"), JSON.stringify({ schema: "session2-residual-source-identity/v1", sha: "b".repeat(40), head: "b".repeat(40), head_matches_sha: false }));
	}));
	check("dirty begin source identity rejected", mutate(({ evidence, read, write, join }) => {
		const p = join(evidence, "source-identity.json");
		const s = JSON.parse(read(p, "utf8"));
		s.clean = false;
		s.dirty_sources = ["untracked source: crates/x.rs"];
		write(p, JSON.stringify(s));
	}));
	check("final mode rejects a dirty end identity", (() => {
		const root = mkdtempSync(join(tmpdir(), "verify-session2-endid-"));
		try {
			const sha = "a".repeat(40);
			const { repo, evidence } = buildValidFixture(root, sha);
			const endPath = join(evidence, "source-identity-end.json");
			const end = JSON.parse(readFileSync(endPath, "utf8"));
			end.clean = false;
			end.dirty_sources = ["tracked change:  M crates/x.rs"];
			writeFileSync(endPath, JSON.stringify(end));
			const report = verify({ repo, evidenceDir: evidence, sha, final: true });
			return { ok: report.problems.some((p) => p.includes("end source identity is not clean")), detail: report.problems.join("; ") };
		} finally {
			rmSync(root, { recursive: true, force: true });
		}
	})());
	check("completed command missing log hash rejected", mutate(({ evidence, read, write, join }) => {
		const p = join(evidence, "command-manifest.json");
		const c = JSON.parse(read(p, "utf8"));
		delete c.commands[0].log_sha256;
		write(p, JSON.stringify(c));
	}));
	check("completed command malformed log hash rejected", mutate(({ evidence, read, write, join }) => {
		const p = join(evidence, "command-manifest.json");
		const c = JSON.parse(read(p, "utf8"));
		c.commands[0].log_sha256 = "not-a-64-hex-hash";
		write(p, JSON.stringify(c));
	}));
	check("stale command log hash rejected", mutate(({ evidence, read, write, join }) => {
		const p = join(evidence, "command-manifest.json");
		const c = JSON.parse(read(p, "utf8"));
		write(join(evidence, c.commands[0].log), "tampered log bytes\n");
		write(p, JSON.stringify(c));
	}));
	check("missing task-8 ledger rejected", mutate(({ repo, read, write, join }) => {
		const p = join(repo, TASK8_LEDGER);
		write(p, "");
	}));
	check("required-but-not-authored rejected", mutate(({ evidence, read, write, join }) => {
		const m = JSON.parse(read(join(evidence, "requirements-manifest.json"), "utf8"));
		m.awaited_executed_tests.required_but_not_authored = [{ id: "G12-TOOLCTX-INVOCATION", authored: false, detail: "no test drives a real typed tool" }];
		write(join(evidence, "requirements-manifest.json"), JSON.stringify(m));
	}));
	check("orphan required-but-not-authored test rejected", mutate(({ evidence, read, write, join }) => {
		const m = JSON.parse(read(join(evidence, "requirements-manifest.json"), "utf8"));
		m.awaited_executed_tests.required_but_not_authored = [
			{ id: "G12-TOOLCTX-INVOCATION", authored: true, package: "maho-agent", target: "harness_drive_lane", test: "some_orphan_fn_not_in_required_tests" },
		];
		write(join(evidence, "requirements-manifest.json"), JSON.stringify(m));
	}));
	check("linked required-but-not-authored test accepted", (() => {
		const root = mkdtempSync(join(tmpdir(), "verify-session2-rbn-"));
		try {
			const sha = "a".repeat(40);
			const { repo, evidence } = buildValidFixture(root, sha);
			const p = join(evidence, "requirements-manifest.json");
			const m = JSON.parse(readFileSync(p, "utf8"));
			m.awaited_executed_tests.required_but_not_authored = [
				{ id: "SDK-NATIVE-SESSION", authored: true, package: "maho-test-support", target: "faux_native_session", test: "native_handle_drives_a_prompt_and_closes_cleanly" },
			];
			writeFileSync(p, JSON.stringify(m));
			const report = verify({ repo, evidenceDir: evidence, sha });
			return { ok: report.problems.length === 0, detail: report.problems.join("; ") };
		} finally {
			rmSync(root, { recursive: true, force: true });
		}
	})());
	check("omitted requirement id rejected", mutate(({ evidence, read, write, join }) => {
		const m = JSON.parse(read(join(evidence, "requirements-manifest.json"), "utf8"));
		m.requirements = m.requirements.filter((r) => r.id !== "IS-6");
		write(join(evidence, "requirements-manifest.json"), JSON.stringify(m));
	}));
	check("empty lib without recorded exclusion rejected", mutate(({ evidence, read, write, join }) => {
		const m = JSON.parse(read(join(evidence, "requirements-manifest.json"), "utf8"));
		m.empty_lib_exports = { remaining: [] };
		write(join(evidence, "requirements-manifest.json"), JSON.stringify(m));
	}));
	check("final mode rejects a null exit", (() => {
		const root = mkdtempSync(join(tmpdir(), "verify-session2-final-"));
		try {
			const sha = "a".repeat(40);
			const { repo, evidence } = buildValidFixture(root, sha);
			const report = verify({ repo, evidenceDir: evidence, sha, final: true });
			return { ok: report.problems.some((p) => p.includes("no exit code") || p.includes("never completed")), detail: report.problems.join("; ") };
		} finally {
			rmSync(root, { recursive: true, force: true });
		}
	})());
	check("server row without required_tests rejected", mutate(({ evidence, read, write, join }) => {
		const m = JSON.parse(read(join(evidence, "requirements-manifest.json"), "utf8"));
		m.server_row_manifest.rows.find((row) => row.id === 7).required_tests = [];
		write(join(evidence, "requirements-manifest.json"), JSON.stringify(m));
	}));
	check("server row unlinked triple rejected", mutate(({ evidence, read, write, join }) => {
		const m = JSON.parse(read(join(evidence, "requirements-manifest.json"), "utf8"));
		m.server_row_manifest.rows.find((row) => row.id === 7).required_tests = [{ package: "maho-cli", target: "mini_worker", test: "some_fn_not_in_required_tests" }];
		write(join(evidence, "requirements-manifest.json"), JSON.stringify(m));
	}));
	check("server row 25 MCP wrong package rejected", mutate(({ evidence, read, write, join }) => {
		const m = JSON.parse(read(join(evidence, "requirements-manifest.json"), "utf8"));
		m.server_row_manifest.rows.find((row) => row.id === 25).required_tests = [{ package: "maho-server", target: "lifecycle_registration", test: "session_start_attach_publishes_live_mcp_status_to_the_bound_subscriber_and_stops_after_unsubscribe" }];
		write(join(evidence, "requirements-manifest.json"), JSON.stringify(m));
	}));
	check("server row approved-exclusion with tests rejected", mutate(({ evidence, read, write, join }) => {
		const m = JSON.parse(read(join(evidence, "requirements-manifest.json"), "utf8"));
		m.server_row_manifest.rows[0].status = "approved-exclusion";
		m.server_row_manifest.rows[0].required_tests = [{ package: "maho-cli", target: "mini_worker", test: "unwatch_and_close_release_watch_subscriptions" }];
		write(join(evidence, "requirements-manifest.json"), JSON.stringify(m));
	}));
	check("server row completeness gap rejected", mutate(({ evidence, read, write, join }) => {
		const m = JSON.parse(read(join(evidence, "requirements-manifest.json"), "utf8"));
		m.server_row_manifest.rows = m.server_row_manifest.rows.filter((row) => row.id !== 25);
		write(join(evidence, "requirements-manifest.json"), JSON.stringify(m));
	}));
	check("server row uncovered_behavior rejects success", mutate(({ evidence, read, write, join }) => {
		const m = JSON.parse(read(join(evidence, "requirements-manifest.json"), "utf8"));
		m.server_row_manifest.uncovered_behavior = [{ id: "UB-TEST", rows: [10], detail: "unresolved behavior" }];
		write(join(evidence, "requirements-manifest.json"), JSON.stringify(m));
	}));
	check("on-disk fixture: hidden row", (() => {
		const path = join(FIXTURES, "parity-hidden-row.md");
		if (!existsSync(path)) return { ok: false, detail: "fixture missing" };
		const text = readFileSync(path, "utf8");
		const declared = countDeclaredRows(text);
		const parsed = parseFragment(text, "fixture").length;
		return { ok: declared > parsed, detail: `declared=${declared} parsed=${parsed}` };
	})());
	check("on-disk fixture: skipped nextest log", (() => {
		const path = join(FIXTURES, "nextest-skipped.log");
		if (!existsSync(path)) return { ok: false, detail: "fixture missing" };
		const parsed = parseNextestLog(readFileSync(path, "utf8"));
		return { ok: parsed.skipped > 0, detail: `skipped=${parsed.skipped}` };
	})());
	check("on-disk fixture: placeholder identifiers rejected", (() => {
		const path = join(FIXTURES, "placeholder-required-tests.json");
		if (!existsSync(path)) return { ok: false, detail: "fixture missing" };
		const required = readJson(path).execution_coverage?.required_tests ?? [];
		const bad = required.filter((entry) => !EXACT_TEST_ID.test(String(entry.test)));
		return { ok: bad.length === required.length && bad.length > 0, detail: `${bad.length}/${required.length} rejected as placeholders` };
	})());
	check("on-disk fixture: discovered-but-not-executed rejected", (() => {
		const path = join(FIXTURES, "nextest-missing-required.log");
		if (!existsSync(path)) return { ok: false, detail: "fixture missing" };
		const parsed = parseNextestLog(readFileSync(path, "utf8"));
		const key = `${binaryIdFor("maho-cli", "mini_worker")}\u0000models_login_rejects_an_unknown_provider`;
		return { ok: parsed.statuses.get(key) === undefined, detail: `status=${parsed.statuses.get(key)}` };
	})());
	check("on-disk fixture: command missing log hash rejected", (() => {
		const path = join(FIXTURES, "command-manifest-missing-hash.json");
		if (!existsSync(path)) return { ok: false, detail: "fixture missing" };
		const commands = readJson(path).commands ?? [];
		const bad = commands.filter((entry) => entry.exit_code !== null && !SHA256_HEX.test(String(entry.log_sha256 ?? "")));
		return { ok: bad.length === commands.length && bad.length > 0, detail: `${bad.length}/${commands.length} rejected` };
	})());
	check("on-disk fixture: command malformed log hash rejected", (() => {
		const path = join(FIXTURES, "command-manifest-malformed-hash.json");
		if (!existsSync(path)) return { ok: false, detail: "fixture missing" };
		const commands = readJson(path).commands ?? [];
		const bad = commands.filter((entry) => !SHA256_HEX.test(String(entry.log_sha256 ?? "")));
		return { ok: bad.length === commands.length && bad.length > 0, detail: `${bad.length}/${commands.length} rejected` };
	})());
	check("on-disk fixture: dirty source identity rejected", (() => {
		const path = join(FIXTURES, "source-identity-dirty.json");
		if (!existsSync(path)) return { ok: false, detail: "fixture missing" };
		const src = readJson(path);
		return { ok: src.clean !== true && (src.dirty_sources ?? []).length > 0, detail: `clean=${src.clean} dirty=${(src.dirty_sources ?? []).length}` };
	})());
	check("on-disk fixture: orphan required-but-not-authored rejected", (() => {
		const path = join(FIXTURES, "required-but-not-authored-orphan.json");
		if (!existsSync(path)) return { ok: false, detail: "fixture missing" };
		const parsed = readJson(path);
		const triples = new Set((parsed.execution_coverage?.required_tests ?? []).map((e) => `${e.package}\u0000${e.target}\u0000${e.test}`));
		const orphans = (parsed.awaited_executed_tests?.required_but_not_authored ?? []).filter((e) => !triples.has(`${e.package}\u0000${e.target}\u0000${e.test}`));
		return { ok: orphans.length > 0, detail: `orphans=${orphans.length}` };
	})());

	check("on-disk fixture: server row without required tests rejected", (() => {
		const path = join(FIXTURES, "server-row-missing-required-tests.json");
		if (!existsSync(path)) return { ok: false, detail: "fixture missing" };
		const parsed = readJson(path);
		const bad = (parsed.server_row_manifest?.rows ?? []).filter((row) => row.status !== "approved-exclusion" && !(row.required_tests ?? []).length);
		return { ok: bad.length > 0, detail: `rows-without-required_tests=${bad.length}` };
	})());
	check("on-disk fixture: server row unlinked triple rejected", (() => {
		const path = join(FIXTURES, "server-row-unlinked-triple.json");
		if (!existsSync(path)) return { ok: false, detail: "fixture missing" };
		const parsed = readJson(path);
		const triples = new Set((parsed.execution_coverage?.required_tests ?? []).map((e) => `${e.package}\u0000${e.target}\u0000${e.test}`));
		const unlinked = (parsed.server_row_manifest?.rows ?? []).flatMap((row) => row.required_tests ?? []).filter((e) => !triples.has(`${e.package}\u0000${e.target}\u0000${e.test}`));
		return { ok: unlinked.length > 0, detail: `unlinked=${unlinked.length}` };
	})());
	check("on-disk fixture: server row uncovered behavior rejected", (() => {
		const path = join(FIXTURES, "server-row-uncovered-behavior.json");
		if (!existsSync(path)) return { ok: false, detail: "fixture missing" };
		const parsed = readJson(path);
		return { ok: (parsed.server_row_manifest?.uncovered_behavior ?? []).length > 0, detail: `uncovered=${(parsed.server_row_manifest?.uncovered_behavior ?? []).length}` };
	})());
	check("on-disk fixture: server row 25 MCP wrong package rejected", (() => {
		const path = join(FIXTURES, "server-row-mcp-wrong-package.json");
		if (!existsSync(path)) return { ok: false, detail: "fixture missing" };
		const parsed = readJson(path);
		const row25 = (parsed.server_row_manifest?.rows ?? []).find((row) => row.id === 25);
		const mcp = (row25?.required_tests ?? []).filter((e) => String(e.test ?? "").includes("session_start_attach_publishes_live_mcp_status"));
		return { ok: mcp.length > 0 && mcp.some((e) => e.package !== "maho-ext-mcp"), detail: `mcp=${JSON.stringify(mcp)}` };
	})());

	const failed = results.filter((r) => !r.ok);
	console.log(`self-test: ${results.length - failed.length}/${results.length} passed`);
	return failed.length === 0;
}

// ---------------------------------------------------------------------------------------------
// CLI.
// ---------------------------------------------------------------------------------------------

function usage(message) {
	console.error(`verify-session2-residual: ${message}`);
	console.error("usage: bun tools/verify-session2-residual.mjs (--evidence <E> --sha <sha> | --self-test)");
	process.exit(2);
}

if (import.meta.main) {
	const argv = process.argv.slice(2);
	let evidenceDir;
	let sha;
	let self = false;
	let final = false;
	for (let i = 0; i < argv.length; i++) {
		if (argv[i] === "--evidence") evidenceDir = argv[++i];
		else if (argv[i] === "--sha") sha = argv[++i];
		else if (argv[i] === "--self-test") self = true;
		else if (argv[i] === "--final") final = true;
		else usage(`unknown argument ${argv[i]}`);
	}
	if (self) process.exit(selfTest() ? 0 : 1);
	if (!evidenceDir || !sha) usage("--evidence and --sha are both required");
	const report = verify({ repo: REPO, evidenceDir: resolve(evidenceDir), sha, final });
	for (const note of report.notes) console.log(`note: ${note}`);
	for (const problem of report.problems) console.error(`FAIL: ${problem}`);
	if (report.problems.length > 0) {
		console.error(`verify-session2-residual: ${report.problems.length} problem(s)`);
		process.exit(1);
	}
	console.log("verify-session2-residual: evidence complete and consistent");
	process.exit(0);
}
