#!/usr/bin/env bun
// The assembled session2-residual gate runner.
//
//   bun tools/run-session2-residual-gate.mjs --workspace <PWD> --evidence <E> --sha <SHA> \
//       --cargo <path/to/cargo4> --binary <path/to/mhc>
//
// It runs EVERY command the bound requirements manifest lists, in order, KEEPING GOING after a
// failure, and captures each command's stdout+stderr and its ACTUAL exit code. It freezes the
// workspace package manifest, binds a COPY of the committed task-17 manifest into the evidence root
// (never mutating the committed template), writes a command manifest with log hashes, and returns
// nonzero if any command failed. It runs no check of its own beyond launching the declared commands.
//
// Task 19 invokes this once, inside a monitor, and reads the resulting evidence with
// `tools/verify-session2-residual.mjs`.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const TEMPLATE_PATH = ".omo/evidence/residual-source/task-17/requirements-manifest.json";
const REPO_TOOLS = dirname(fileURLToPath(import.meta.url));
const PACKAGE_SCHEMA = "session2-residual-package-manifest/v1";
const COMMAND_SCHEMA = "session2-residual-command-manifest/v1";
const BUILD_PROVENANCE_SCHEMA = "session2-residual-build-provenance/v1";
const BUILD_PROVENANCE = "build-provenance.json";
const RUN_IDENTITY = "run-identity.json";
const SOURCE_IDENTITY = "source-identity.json";
const HARNESS_IDENTITY = "harness-identity.json";
const HARNESS_IDENTITY_SCHEMA = "session2-residual-harness-identity/v1";
// The workspace build command whose success gates every binary-dependent command, and the commands
// that consume the built `$BIN`/staged artifact (they must never run against a stale binary).
const BUILD_COMMAND_RE = /build --workspace/;
const BINARY_DEPENDENT_RES = [/package-native\.mjs/, /run-qa\.sh/, /residual-qa\.mjs/];

function usage(message) {
	console.error(`run-session2-residual-gate: ${message}`);
	console.error(
		"usage: bun tools/run-session2-residual-gate.mjs --workspace <dir> --evidence <dir> --sha <sha> --cargo <cargo4> --binary <mhc>",
	);
	process.exit(2);
}

function sha256(data) {
	return createHash("sha256").update(data).digest("hex");
}

/** The pre-run state of `$BIN`. Recorded as INFORMATIONAL provenance only — a byte-identical
 *  artifact is NOT by itself a stale-binary signal (cargo legitimately reuses an up-to-date
 *  artifact for an unchanged product with harness/evidence-only edits). */
function binBaseline(binary) {
	const abs = resolve(binary);
	if (!existsSync(abs)) return { existed: false, sha256: null, mtime_ms: null };
	return { existed: true, sha256: sha256(readFileSync(abs)), mtime_ms: statSync(abs).mtimeMs };
}

/** Bind the SUCCESSFUL candidate build to its output artifact WITHOUT requiring the artifact to be
 *  new or changed. The proof is the exact source/build INPUTS (candidate sha + Cargo.lock +
 *  Cargo.toml + rust-toolchain), the captured cargo RESULT (exit + build-log hash), the artifact
 *  fingerprint, and — verified by the verifier — staging/QA hash identity. Hash inequality and
 *  mtime are NOT proof and are recorded as informational provenance only; requiring them would
 *  falsely reject a legitimate cargo reuse of an up-to-date artifact. */
function buildProvenance(repo, binary, sha, buildExit, buildLogPath, cargoTargetDir, baseline) {
	const abs = resolve(binary);
	let binarySha = null;
	let binarySize = null;
	let mtimeMs = null;
	if (existsSync(abs)) {
		const bytes = readFileSync(abs);
		binarySha = sha256(bytes);
		binarySize = bytes.length;
		mtimeMs = statSync(abs).mtimeMs;
	}
	const readHash = (rel) => {
		const p = join(repo, rel);
		return existsSync(p) ? sha256(readFileSync(p)) : null;
	};
	const buildLogSha = buildLogPath && existsSync(buildLogPath) ? sha256(readFileSync(buildLogPath)) : null;
	return {
		schema: BUILD_PROVENANCE_SCHEMA,
		sha,
		binary: abs,
		produced: buildExit === 0 && binarySha !== null,
		build_command: "cargo4 build --workspace --bins",
		build_exit_code: buildExit,
		build_log_sha256: buildLogSha,
		cargo_target_dir: cargoTargetDir,
		inputs: {
			source_sha: sha,
			cargo_lock_sha256: readHash("Cargo.lock"),
			cargo_toml_sha256: readHash("Cargo.toml"),
			rust_toolchain_sha256: readHash("rust-toolchain.toml"),
		},
		binary_sha256: binarySha,
		binary_size: binarySize,
		baseline: { existed: baseline.existed, sha256: baseline.sha256, mtime_ms: baseline.mtime_ms },
		artifact_mtime_ms: mtimeMs,
		captured_at: new Date().toISOString(),
	};
}

function parseArgs(argv) {
	const args = { workspace: process.cwd() };
	for (let i = 0; i < argv.length; i++) {
		const key = argv[i];
		if (key === "--workspace") args.workspace = argv[++i];
		else if (key === "--evidence") args.evidence = argv[++i];
		else if (key === "--sha") args.sha = argv[++i];
		else if (key === "--cargo") args.cargo = argv[++i];
		else if (key === "--binary") args.binary = argv[++i];
		else if (key === "--compile-blocked") args.compileBlocked = argv[++i];
		else usage(`unknown argument ${key}`);
	}
	if (!args.evidence) usage("--evidence is required");
	if (!args.sha) usage("--sha is required");
	if (!args.cargo) usage("--cargo is required");
	if (!args.binary) usage("--binary is required");
	return args;
}

// ---------------------------------------------------------------------------------------------
// Workspace package manifest (ALL members — expanded coverage, not merely canonical 25).
// Enumerated with real `cargo metadata` (authoritative), with a Cargo.toml parse as a fallback.
// ---------------------------------------------------------------------------------------------

function parseTomlString(text, key) {
	const match = text.match(new RegExp(`^\\s*${key}\\s*=\\s*"([^"]*)"`, "m"));
	return match ? match[1] : null;
}

async function cargoMetadataPackages(repo, cargo) {
	try {
		const proc = Bun.spawn([cargo, "metadata", "--no-deps", "--format-version", "1"], {
			cwd: repo,
			stdout: "pipe",
			stderr: "pipe",
			env: { ...process.env, CARGO_BUILD_JOBS: process.env.CARGO_BUILD_JOBS ?? "1" },
		});
		// Drain BOTH pipes concurrently to avoid a stderr pipe-buffer deadlock on a chatty cargo.
		const [out, err, code] = await Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text(), proc.exited]);
		if (code !== 0) {
			if (err.trim()) console.error(`cargo metadata stderr: ${err.trim().split("\n").slice(-3).join(" | ")}`);
			return null;
		}
		const meta = JSON.parse(out);
		const members = new Set(meta.workspace_members ?? []);
		const packages = (meta.packages ?? []).map((entry) => ({
			name: entry.name,
			// Store RELATIVE to the repo so the verifier never join()s an absolute path onto the repo.
			manifest_path: relative(repo, entry.manifest_path),
			workspace_member: members.has(entry.id),
		}));
		return packages.length > 0 ? packages : null;
	} catch (error) {
		console.error(`cargo metadata failed: ${error.message}`);
		return null;
	}
}

function parseTomlMembers(repo) {
	const text = readFileSync(join(repo, "Cargo.toml"), "utf8");
	const block = text.match(/\[workspace\][\s\S]*?members\s*=\s*\[([\s\S]*?)\]/);
	if (!block) throw new Error("Cargo.toml has no [workspace] members list");
	const members = block[1]
		.split("\n")
		.map((line) => line.trim().replace(/[",]/g, ""))
		.filter(Boolean)
		.map((member) => member.replace(/\/$/, ""));
	if (members.some((member) => member.includes("*"))) {
		throw new Error("Cargo.toml uses glob members; the parse fallback cannot expand them (cargo metadata is required)");
	}
	return members.map((member) => {
		const manifestPath = join(repo, member, "Cargo.toml");
		if (!existsSync(manifestPath)) throw new Error(`workspace member ${member} has no Cargo.toml`);
		return { name: parseTomlString(readFileSync(manifestPath, "utf8"), "name") ?? member.split("/").pop(), manifest_path: `${member}/Cargo.toml`, workspace_member: true };
	});
}

async function freezePackageManifest(repo, sha, cargo) {
	let packages = await cargoMetadataPackages(repo, cargo);
	let method = "cargo-metadata";
	if (!packages) {
		packages = parseTomlMembers(repo);
		method = "toml-parse";
	}
	const entries = packages.map((entry) => ({
		name: entry.name,
		manifest_path: entry.manifest_path,
		workspace_member: entry.workspace_member,
		sha256: sha256(readFileSync(resolve(repo, entry.manifest_path))),
	}));
	entries.sort((a, b) => a.name.localeCompare(b.name));
	return { schema: PACKAGE_SCHEMA, sha, method, package_count: entries.length, packages: entries };
}

// ---------------------------------------------------------------------------------------------
// Bound manifest: a COPY of the committed task-17 template with assembled_sha set.
// ---------------------------------------------------------------------------------------------

function bindManifest(repo, sha) {
	const templatePath = join(repo, TEMPLATE_PATH);
	if (!existsSync(templatePath)) throw new Error(`committed manifest template missing: ${TEMPLATE_PATH}`);
	const raw = readFileSync(templatePath, "utf8");
	const bound = JSON.parse(raw);
	if (!Array.isArray(bound.gate_commands) || bound.gate_commands.length === 0) {
		throw new Error("committed manifest template has no gate_commands");
	}
	bound.binding = bound.binding ?? {};
	bound.binding.assembled_sha = sha;
	bound.bound_from = { source_path: TEMPLATE_PATH, source_sha256: sha256(raw), bound_at_sha: sha };
	bound.gate_runner = "tools/run-session2-residual-gate.mjs";
	return bound;
}

// ---------------------------------------------------------------------------------------------
// Command execution (keep-going, real exit codes, per-command logs).
// ---------------------------------------------------------------------------------------------

const LOG_NAMES = [
	[/nextest run/, "nextest.log"],
	[/clippy --workspace/, "clippy.log"],
	[/build --workspace/, "build.log"],
	[/parity-audit\.mjs --self-test/, "parity-self-test.log"],
	[/parity-audit\.mjs --all/, "parity-all.log"],
	[/package-native\.mjs/, "package-native.log"],
	[/run-qa\.sh/, "run-qa.log"],
	[/residual-qa\.mjs/, "residual-qa.log"],
	[/verify-session2-residual\.mjs --self-test/, "verify-self-test.log"],
	[/verify-session2-residual\.mjs --evidence/, "verify.log"],
];

function logNameFor(template, index) {
	for (const [pattern, name] of LOG_NAMES) if (pattern.test(template)) return name;
	return `command-${String(index + 1).padStart(2, "0")}.log`;
}

/** Expands the manifest template into a shell command: `cargo4` -> the real cargo path, and the
 *  `$BIN`/`$E`/`$SHA`/`$PWD` variables via the exported environment. */
function materialize(template, args) {
	return template.replace(/^cargo4\b/, args.cargo);
}

async function runCommand(command, env, logPath, cwd) {
	const startedAt = new Date().toISOString();
	const startedMs = Date.now();
	const proc = Bun.spawn(["bash", "-c", command], { cwd, env, stdout: "pipe", stderr: "pipe" });
	const [stdout, stderr] = await Promise.all([new Response(proc.stdout).arrayBuffer(), new Response(proc.stderr).arrayBuffer()]);
	const exitCode = await proc.exited;
	const endedMs = Date.now();
	const outBuf = Buffer.from(stdout);
	const errBuf = Buffer.from(stderr);
	const combined = Buffer.concat([
		outBuf,
		outBuf.length > 0 && !outBuf.subarray(-1).equals(Buffer.from("\n")) ? Buffer.from("\n") : Buffer.alloc(0),
		errBuf,
	]);
	writeFileSync(logPath, combined);
	return {
		started_at: startedAt,
		ended_at: new Date().toISOString(),
		duration_ms: endedMs - startedMs,
		exit_code: exitCode,
		stdout_bytes: outBuf.length,
		stderr_bytes: errBuf.length,
		log_sha256: sha256(combined),
	};
}

// ---------------------------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------------------------

async function git(repo, args) {
	const proc = Bun.spawn(["git", ...args], { cwd: repo, stdout: "pipe", stderr: "pipe" });
	const [out, , code] = await Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text(), proc.exited]);
	return code === 0 ? out.trim() : null;
}

/** Splits `git status --porcelain=v1` into entries, rejecting tracked source changes and untracked
 *  files outside `.omo/` (which holds this run's evidence and other sessions' scratch). */
function dirtySources(porcelain) {
	const bad = [];
	for (const line of (porcelain ?? "").split("\n")) {
		if (line.length < 4) continue;
		const code = line.slice(0, 2);
		const path = line.slice(3).trim();
		if (code === "??") {
			if (!path.startsWith(".omo/")) bad.push(`untracked source: ${path}`);
		} else {
			bad.push(`tracked change: ${line.trim()}`);
		}
	}
	return bad;
}

async function sourceIdentity(repo, sha, evidenceRel) {
	const head = await git(repo, ["rev-parse", "HEAD"]);
	const porcelain = await git(repo, ["status", "--porcelain=v1"]);
	const dirty = dirtySources(porcelain).filter((entry) => !entry.includes(evidenceRel));
	return {
		schema: "session2-residual-source-identity/v1",
		sha,
		head,
		head_matches_sha: head === sha,
		status_porcelain: porcelain,
		dirty_sources: dirty,
		clean: dirty.length === 0,
	};
}

// The active harness/tooling set the QA evidence is bound to: the harness-v2 scripts plus the
// invoked tracked tools (verifier, residual-qa, parity, package). Each entry is hashed at run start
// and re-verified at run end; a change during the run means a QA leg may have executed a different
// harness revision than the one recorded, so the run fails rather than certify.
const HARNESS_TOOLING = [
	"tools/run-session2-residual-gate.mjs",
	"tools/verify-session2-residual.mjs",
	"tools/residual-qa.mjs",
	"tools/parity-audit.mjs",
	"tools/package-native.mjs",
	".omo/evidence/session2-residual/harness-v2/run-qa.sh",
	".omo/evidence/session2-residual/harness-v2/qa-tui-pty.mjs",
	".omo/evidence/session2-residual/harness-v2/tui-driver.mjs",
	".omo/evidence/session2-residual/harness-v2/qa-loopback-lib.mjs",
	".omo/evidence/session2-residual/harness-v2/qa-help-flags.mjs",
	".omo/evidence/session2-residual/harness-v2/qa-import-omo.mjs",
	".omo/evidence/session2-residual/harness-v2/qa-loopback-print.mjs",
	".omo/evidence/session2-residual/harness-v2/qa-rpc-ids.mjs",
	".omo/evidence/session2-residual/harness-v2/png-render.mjs",
];

function harnessIdentity(repo) {
	return HARNESS_TOOLING.map((rel) => {
		const path = join(repo, rel);
		return { path: rel, sha256: existsSync(path) ? sha256(readFileSync(path)) : null };
	});
}

async function main() {
	const args = parseArgs(process.argv.slice(2));
	const repo = resolve(args.workspace);
	const evidence = resolve(args.evidence);
	const sha = args.sha;
	if (!/^[0-9a-f]{40}$/.test(sha)) usage(`--sha "${sha}" is not a full 40-hex SHA`);

	// Preflight: another session's compile marker is never deleted or bypassed.
	const marker = args.compileBlocked ?? join(repo, ".COMPILE_BLOCKED");
	if (existsSync(marker)) {
		console.error(`run-session2-residual-gate: ${marker} exists — owner coordination required; refusing to run. Do not delete it.`);
		process.exit(3);
	}

	// Refuse to reuse an evidence root that already holds a gate run: raw logs are immutable.
	if (existsSync(join(evidence, RUN_IDENTITY))) {
		console.error(`run-session2-residual-gate: ${evidence} already contains a gate run (${RUN_IDENTITY}); refusing to overwrite immutable logs. Use a fresh SHA-scoped evidence root.`);
		process.exit(4);
	}

	// Source identity: the supplied SHA must be the actual git HEAD and the worktree must be clean
	// of tracked/untracked source changes (this run's own evidence root is allowed).
	const evidenceRel = relative(repo, evidence);
	const identity = await sourceIdentity(repo, sha, evidenceRel);
	if (!identity.head_matches_sha) {
		console.error(`run-session2-residual-gate: --sha ${sha} != worktree HEAD ${identity.head}; refusing to run (env SHA is not evidence).`);
		process.exit(5);
	}
	if (!identity.clean) {
		console.error(`run-session2-residual-gate: worktree is not clean; refusing to run. ${identity.dirty_sources.join("; ")}`);
		process.exit(6);
	}

	const runStartedAtMs = Date.now();
	mkdirSync(evidence, { recursive: true });
	writeFileSync(join(evidence, RUN_IDENTITY), JSON.stringify({ schema: "session2-residual-run-identity/v1", sha, started_at: new Date(runStartedAtMs).toISOString(), started_at_ms: runStartedAtMs, runner: "tools/run-session2-residual-gate.mjs" }, null, 2) + "\n");
	writeFileSync(join(evidence, SOURCE_IDENTITY), JSON.stringify(identity, null, 2) + "\n");

	// Bind the QA evidence to the exact harness/tooling revision: record the start hashes BEFORE any
	// command runs; the end block is re-verified after the last command (a mid-run change fails).
	const harnessStart = harnessIdentity(repo);
	const writeHarnessIdentity = (end, changed, match) =>
		writeFileSync(join(evidence, HARNESS_IDENTITY), JSON.stringify({ schema: HARNESS_IDENTITY_SCHEMA, sha, start: harnessStart, end, changed, match }, null, 2) + "\n");
	writeHarnessIdentity(null, [], null);

	// Freeze + bind BEFORE running anything, so the verifier's inputs exist even if a command fails.
	writeFileSync(join(evidence, "package-manifest.json"), JSON.stringify(await freezePackageManifest(repo, sha, resolve(args.cargo)), null, 2) + "\n");
	const bound = bindManifest(repo, sha);
	writeFileSync(join(evidence, "requirements-manifest.json"), JSON.stringify(bound, null, 2) + "\n");

	const env = {
		...process.env,
		PWD: repo,
		SHA: sha,
		E: evidence,
		BIN: resolve(args.binary),
		CARGO: resolve(args.cargo),
	};

	// The full command list is known up front; the command manifest is persisted AFTER EACH command
	// (with every entry present, later exit codes still null) so the final verifier — itself a listed
	// command — finds the manifest it depends on without a self-dependency.
	const commands = bound.gate_commands.map((template, i) => ({ template, command: materialize(template, { cargo: env.CARGO }), log: logNameFor(template, i), exit_code: null }));
	const writeCommands = () => writeFileSync(join(evidence, "command-manifest.json"), JSON.stringify({ schema: COMMAND_SCHEMA, sha, cargo: env.CARGO, binary: env.BIN, commands }, null, 2) + "\n");
	writeCommands();

	// Failure accounting is split so the reported aggregate is honest: per-command failures (a nonzero
	// exit, including a BLOCKED binary-dependent command) are counted separately from run-level
	// failures (a build that produced no artifact, a dirty end source identity, and a failing final
	// audit). `failed` remains their sum, so the exit code and verdict are unchanged.
	let commandFailures = 0;
	let runFailures = 0;
	// The pre-run `$BIN` state; a stale binary (unchanged sha256, mtime before the run) must never
	// satisfy a binary-dependent command. The build command's success gates those commands.
	const baseline = binBaseline(args.binary);
	const buildIndex = commands.findIndex((entry) => BUILD_COMMAND_RE.test(entry.template));
	const isBinaryDependent = (template) => BINARY_DEPENDENT_RES.some((re) => re.test(template));
	let buildExit = null;
	let buildFailed = false;
	let provenance = null;
	for (let i = 0; i < commands.length; i++) {
		const entry = commands[i];
		// A binary-dependent command must not run against a stale artifact after a failed build:
		// record it BLOCKED (a FAILURE, never a skip-green) and keep going for the full inventory.
		if (buildFailed && isBinaryDependent(entry.template)) {
			const message = `BLOCKED: not executed — the workspace build (command ${buildIndex + 1}) failed (exit ${buildExit}); running this against a pre-existing binary would accept a STALE artifact.\n`;
			const bytes = Buffer.from(message);
			writeFileSync(join(evidence, entry.log), bytes);
			Object.assign(entry, {
				started_at: new Date().toISOString(),
				ended_at: new Date().toISOString(),
				duration_ms: 0,
				exit_code: 125,
				blocked: true,
				stdout_bytes: 0,
				stderr_bytes: bytes.length,
				log_sha256: sha256(bytes),
			});
			commandFailures += 1;
			console.log(`[gate ${i + 1}/${commands.length}] BLOCKED (build failed): ${entry.command}`);
			writeCommands();
			continue;
		}
		console.log(`[gate ${i + 1}/${commands.length}] ${entry.command}`);
		const result = await runCommand(entry.command, env, join(evidence, entry.log), repo);
		Object.assign(entry, result);
		console.log(`[gate ${i + 1}/${commands.length}] exit=${result.exit_code} log=${entry.log}`);
		if (i === buildIndex) {
			buildExit = result.exit_code;
			buildFailed = result.exit_code !== 0;
			// Bind the (successful or failed) build output to the candidate BEFORE any QA consumes it.
			provenance = buildProvenance(repo, args.binary, sha, buildExit, join(evidence, entry.log), process.env.CARGO_TARGET_DIR ?? null, baseline);
			writeFileSync(join(evidence, BUILD_PROVENANCE), JSON.stringify(provenance, null, 2) + "\n");
		}
		if (result.exit_code !== 0) commandFailures += 1;
		writeCommands();
	}

	// Fail the gate if the build reported success but the binary was NOT produced by this run.
	if (buildExit === 0 && provenance && !provenance.produced) {
		console.error("run-session2-residual-gate: build reported success but $BIN was not produced by this run (stale/absent binary); failing.");
		runFailures += 1;
	}

	// End-of-run source identity: prove the gate did not mutate the worktree.
	const endIdentity = await sourceIdentity(repo, sha, evidenceRel);
	writeFileSync(join(evidence, "source-identity-end.json"), JSON.stringify(endIdentity, null, 2) + "\n");
	if (!endIdentity.clean || !endIdentity.head_matches_sha) {
		console.error(`run-session2-residual-gate: worktree changed during the gate (clean=${endIdentity.clean} head=${endIdentity.head}); failing.`);
		runFailures += 1;
	}

	// End-of-run harness identity: a harness/tooling file that changed during the gate means a QA leg
	// may have executed a different revision than the one recorded at start — fail rather than certify.
	const harnessEnd = harnessIdentity(repo);
	const harnessChanged = harnessStart.filter((entry) => harnessEnd.find((e) => e.path === entry.path)?.sha256 !== entry.sha256).map((entry) => entry.path);
	const harnessMatch = harnessChanged.length === 0;
	writeHarnessIdentity(harnessEnd, harnessChanged, harnessMatch);
	if (!harnessMatch) {
		console.error(`run-session2-residual-gate: harness/tooling changed during the gate (${harnessChanged.join(", ")}); failing — QA evidence is not bound to the recorded harness revision.`);
		runFailures += 1;
	}

	// Non-circular final audit: the listed verifier ran mid-list (pre-final, excluding exactly its
	// own still-null entry). Here, every command — including the verifier — has a completed exit, so
	// the parent/runner re-runs the verifier in final mode over the complete manifest.
	const finalAudit = await runCommand(
		`bun ${join(REPO_TOOLS, "verify-session2-residual.mjs")} --evidence "$E" --sha "$SHA" --final`,
		env,
		join(evidence, "verify-final.log"),
		repo,
	);
	console.log(`run-session2-residual-gate: final verifier audit exit=${finalAudit.exit_code}`);
	if (finalAudit.exit_code !== 0) runFailures += 1;

	const failed = commandFailures + runFailures;
	console.log(`run-session2-residual-gate: ${commands.length} commands, ${commandFailures} command failure(s), ${runFailures} run-level failure(s), ${failed} total`);
	writeFileSync(join(evidence, "gate-complete.json"), JSON.stringify({ schema: "session2-residual-gate-complete/v1", sha, failed, command_failures: commandFailures, run_failures: runFailures, commands: commands.length, final_audit_exit: finalAudit.exit_code, build_exit_code: buildExit, build_produced: provenance ? provenance.produced : null }, null, 2) + "\n");
	process.exit(failed === 0 ? 0 : 1);
}

if (import.meta.main) {
	await main();
}
