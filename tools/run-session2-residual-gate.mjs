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
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

const TEMPLATE_PATH = ".omo/evidence/residual-source/task-17/requirements-manifest.json";
const PACKAGE_SCHEMA = "session2-residual-package-manifest/v1";
const COMMAND_SCHEMA = "session2-residual-command-manifest/v1";
const RUN_IDENTITY = "run-identity.json";
const SOURCE_IDENTITY = "source-identity.json";

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
		const [out, code] = await Promise.all([new Response(proc.stdout).text(), proc.exited]);
		if (code !== 0) return null;
		const meta = JSON.parse(out);
		const packages = (meta.packages ?? []).map((entry) => ({
			name: entry.name,
			manifest_path: entry.manifest_path,
			workspace_member: (meta.workspace_members ?? []).includes(entry.id),
		}));
		return packages.length > 0 ? packages : null;
	} catch {
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
		return { name: parseTomlString(readFileSync(manifestPath, "utf8"), "name") ?? member.split("/").pop(), manifest_path: manifestPath, workspace_member: true };
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
		sha256: sha256(readFileSync(entry.manifest_path)),
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

function git(repo, args) {
	const proc = Bun.spawnSync(["git", ...args], { cwd: repo, stdout: "pipe", stderr: "pipe" });
	return proc.exitCode === 0 ? proc.stdout.toString().trim() : null;
}

function sourceIdentity(repo, sha) {
	const head = git(repo, ["rev-parse", "HEAD"]);
	return {
		schema: "session2-residual-source-identity/v1",
		sha,
		head,
		head_matches_sha: head === sha,
		status_porcelain: git(repo, ["status", "--porcelain=v1"]),
	};
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

	// Source identity: the supplied SHA must be the actual git HEAD of the worktree.
	const identity = sourceIdentity(repo, sha);
	if (!identity.head_matches_sha) {
		console.error(`run-session2-residual-gate: --sha ${sha} != worktree HEAD ${identity.head}; refusing to run (env SHA is not evidence).`);
		process.exit(5);
	}

	mkdirSync(evidence, { recursive: true });
	writeFileSync(join(evidence, RUN_IDENTITY), JSON.stringify({ schema: "session2-residual-run-identity/v1", sha, started_at: new Date().toISOString(), runner: "tools/run-session2-residual-gate.mjs" }, null, 2) + "\n");
	writeFileSync(join(evidence, SOURCE_IDENTITY), JSON.stringify(identity, null, 2) + "\n");

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

	let failed = 0;
	for (let i = 0; i < commands.length; i++) {
		const entry = commands[i];
		console.log(`[gate ${i + 1}/${commands.length}] ${entry.command}`);
		const result = await runCommand(entry.command, env, join(evidence, entry.log), repo);
		Object.assign(entry, result);
		console.log(`[gate ${i + 1}/${commands.length}] exit=${result.exit_code} log=${entry.log}`);
		if (result.exit_code !== 0) failed += 1;
		writeCommands();
	}

	console.log(`run-session2-residual-gate: ${commands.length} commands, ${failed} failed`);
	process.exit(failed === 0 ? 0 : 1);
}

if (import.meta.main) {
	await main();
}
