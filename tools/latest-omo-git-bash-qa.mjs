#!/usr/bin/env bun
// Real-surface QA driver for the registered `omo-git-bash` MCP stdio subprocess
// (`crates/omo/git-bash-mcp`, package `maho-git-bash-mcp`, bin `omo-git-bash`).
//
//   bun tools/latest-omo-git-bash-qa.mjs --binary <built omo-git-bash> --evidence <isolated E> \
//        [--scenario disabled|usage|protocol|all]
//
// The scenarios, their exact invocation, observable and cleanup:
//
//   disabled  printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize"}' | <bin> mcp
//     observable  EMPTY stdout and exit 0. Off Windows (and on Windows without a resolvable Git
//                 Bash) the reference `runMcpStdioServer` returns without serving, so
//                 `GitBashServerOutcome::Disabled` writes nothing (`src/mcp.rs`).
//     cleanup     one-shot; assert the process exited and no child survived.
//
//   usage     <bin> bogus
//     observable  stderr `Usage: omo-git-bash [mcp]`, exit 2 (`src/cli.rs`).
//     cleanup     none (one-shot).
//
//   protocol  printf an `initialize` + `tools/list` handshake | <bin> mcp
//     observable  a SERVED host answers `serverInfo.name == "git_bash"` and lists the `run` tool.
//                 On non-win32 the server is disabled by design, so this scenario records
//                 `blocked` with the exact platform reason - never a proxy pass, and it never
//                 becomes aggregate acceptance through a skipped scenario.
//     cleanup     one-shot; assert the process exited.
//
// Every scenario runs the built binary directly with a captured sha256 + mtime before and after, so a
// binary that moved under the run is unverified. No fixed sleeps: the child's own exit is awaited.
// A timeout is recorded as a FAILURE: the driver signals the process it owns, reaps the exit and
// every stream under a bound, and never writes a successful process-exit cleanup after a failed
// spawn or a timeout-killed exchange (even if replies were captured).
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";

const SCHEMA = "latest-omo-git-bash-qa/v1";
const EXIT_TIMEOUT_MS = 15_000;
// The bounded window a stream/exit gets after the driver has signalled its owned process.
const REAP_TIMEOUT_MS = 5_000;

const delay = (ms) => new Promise((resolvePromise) => setTimeout(resolvePromise, ms));

function usage(message) {
	console.error(`latest-omo-git-bash-qa: ${message}`);
	console.error("usage: bun tools/latest-omo-git-bash-qa.mjs --binary <omo-git-bash> --evidence <E> [--scenario disabled|usage|protocol|all]");
	process.exit(2);
}

function sha256(data) {
	return createHash("sha256").update(data).digest("hex");
}

function parseArgs(argv) {
	const args = { scenario: "all" };
	for (let i = 0; i < argv.length; i++) {
		const key = argv[i];
		if (key === "--binary") args.binary = argv[++i];
		else if (key === "--evidence") args.evidence = argv[++i];
		else if (key === "--scenario") args.scenario = argv[++i];
		else usage(`unknown argument ${key}`);
	}
	if (!args.binary) usage("--binary is required");
	if (!args.evidence) usage("--evidence is required");
	return args;
}

/**
 * One bounded stdio round trip. The kill timer is armed at `timeoutMs`, so `await proc.exited`
 * resolves either when the OWNED child exits on its own or when the timer signals it - never
 * before, and the timer is never cleared while the child is still alive. Both captured streams are
 * then reaped under a further bound; an incomplete stream is `null` with `streamsComplete: false`.
 */
async function runStdio(binary, args, lines, timeoutMs = EXIT_TIMEOUT_MS) {
	const proc = Bun.spawn([binary, ...args], { stdin: "pipe", stdout: "pipe", stderr: "pipe" });
	const stdoutText = new Response(proc.stdout).text().catch(() => null);
	const stderrText = new Response(proc.stderr).text().catch(() => null);
	let timedOut = false;
	const timer = setTimeout(() => {
		timedOut = true;
		try {
			process.kill(proc.pid, "SIGKILL");
		} catch {
			/* already gone */
		}
	}, timeoutMs);
	try {
		try {
			for (const line of lines) proc.stdin.write(line + "\n");
			proc.stdin.end();
		} catch {
			/* the server may have returned before reading stdin (Disabled) */
		}
		const exitCode = await proc.exited;
		clearTimeout(timer);
		const [stdout, stderr] = await Promise.all([
			Promise.race([stdoutText, delay(REAP_TIMEOUT_MS).then(() => null)]),
			Promise.race([stderrText, delay(REAP_TIMEOUT_MS).then(() => null)]),
		]);
		return { exitCode: timedOut ? null : exitCode, stdout, stderr, timedOut, exited: true, streamsComplete: stdout !== null && stderr !== null };
	} finally {
		clearTimeout(timer);
	}
}

async function scenarioDisabled(ctx) {
	const receipt = { scenario: "disabled", invocation: "initialize | <bin> mcp", cleanup: { processExited: false } };
	if (process.platform === "win32") {
		receipt.reason = "the disabled path requires an unresolvable Git Bash; this scenario does not control the Windows resolver";
		writeFileSync(join(ctx.qaRoot, "git-bash-disabled-receipt.json"), JSON.stringify(receipt, null, 2) + "\n");
		return { status: "blocked", blocker: receipt.reason, artifacts: ["git-bash-disabled-receipt.json"], cleanup_ok: true };
	}
	let ok = false;
	let detail = "";
	try {
		const result = await runStdio(ctx.binary, ["mcp"], ['{"jsonrpc":"2.0","id":1,"method":"initialize"}']);
		receipt.exitCode = result.exitCode;
		receipt.stdout = result.stdout;
		receipt.stderr = result.stderr === null ? null : result.stderr.trim();
		receipt.timedOut = result.timedOut;
		receipt.streamsComplete = result.streamsComplete;
		receipt.cleanup.processExited = result.exited === true && result.timedOut === false && result.streamsComplete === true;
		ok = result.timedOut === false && result.streamsComplete === true && result.exitCode === 0 && result.stdout === "";
		detail = `exit=${result.exitCode} stdoutEmpty=${result.stdout === ""} timedOut=${result.timedOut} streamsComplete=${result.streamsComplete}`;
	} catch (error) {
		detail = `driver error ${error.message}`;
	}
	writeFileSync(join(ctx.qaRoot, "git-bash-disabled-receipt.json"), JSON.stringify(receipt, null, 2) + "\n");
	return {
		status: ok ? "pass" : "blocked",
		blocker: ok ? null : `disabled: ${detail} (off Windows the stdio server must return without serving, writing nothing)`,
		artifacts: ["git-bash-disabled-receipt.json"],
		cleanup_ok: receipt.cleanup.processExited,
	};
}

async function scenarioUsage(ctx) {
	const receipt = { scenario: "usage", invocation: "<bin> bogus", cleanup: { processExited: false } };
	let ok = false;
	let detail = "";
	try {
		const result = await runStdio(ctx.binary, ["bogus"], []);
		receipt.exitCode = result.exitCode;
		receipt.stdout = result.stdout;
		receipt.stderr = result.stderr === null ? null : result.stderr.trim();
		receipt.timedOut = result.timedOut;
		receipt.streamsComplete = result.streamsComplete;
		receipt.cleanup.processExited = result.exited === true && result.timedOut === false && result.streamsComplete === true;
		ok = result.timedOut === false && result.streamsComplete === true && result.exitCode === 2 && (result.stderr ?? "").includes("Usage: omo-git-bash [mcp]");
		detail = `exit=${result.exitCode} usage=${(result.stderr ?? "").includes("Usage: omo-git-bash [mcp]")} timedOut=${result.timedOut} streamsComplete=${result.streamsComplete}`;
	} catch (error) {
		detail = `driver error ${error.message}`;
	}
	writeFileSync(join(ctx.qaRoot, "git-bash-usage-receipt.json"), JSON.stringify(receipt, null, 2) + "\n");
	return {
		status: ok ? "pass" : "blocked",
		blocker: ok ? null : `usage: ${detail} (an unknown subcommand must print the usage line and exit 2)`,
		artifacts: ["git-bash-usage-receipt.json"],
		cleanup_ok: receipt.cleanup.processExited,
	};
}

async function scenarioProtocol(ctx) {
	const receipt = { scenario: "protocol", platform: process.platform, cleanup: { processExited: false } };
	if (process.platform !== "win32") {
		receipt.reason = "the git_bash MCP tool family is Windows-only; off win32 the stdio server is disabled by design";
		writeFileSync(join(ctx.qaRoot, "git-bash-protocol-receipt.json"), JSON.stringify(receipt, null, 2) + "\n");
		return {
			status: "blocked",
			blocker: `protocol: ${receipt.reason}`,
			reason: `protocol: ${receipt.reason}`,
			artifacts: ["git-bash-protocol-receipt.json"],
			cleanup_ok: true,
		};
	}
	let ok = false;
	let detail = "";
	try {
		const result = await runStdio(ctx.binary, ["mcp"], [
			'{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"qa","version":"1.0.0"}}}',
			'{"jsonrpc":"2.0","method":"notifications/initialized"}',
			'{"jsonrpc":"2.0","id":2,"method":"tools/list"}',
		]);
		const replies = (result.stdout ?? "").split("\n").map((line) => line.trim()).filter(Boolean).map((line) => {
			try {
				return JSON.parse(line);
			} catch {
				return null;
			}
		});
		receipt.replies = replies;
		receipt.stderr = result.stderr === null ? null : result.stderr.trim();
		receipt.timedOut = result.timedOut;
		receipt.streamsComplete = result.streamsComplete;
		receipt.cleanup.processExited = result.exited === true && result.timedOut === false && result.streamsComplete === true;
		const init = replies.find((reply) => reply && reply.id === 1);
		const tools = replies.find((reply) => reply && reply.id === 2);
		const serverNamed = init?.result?.serverInfo?.name === "git_bash";
		const runToolListed = (tools?.result?.tools ?? []).some((tool) => tool?.name === "run");
		// A PASS requires the owned process to have exited 0 with both streams complete.
		ok = result.timedOut === false && result.streamsComplete === true && result.exitCode === 0 && serverNamed && runToolListed;
		detail = `serverNamed=${serverNamed} runToolListed=${runToolListed} exit=${result.exitCode} timedOut=${result.timedOut} streamsComplete=${result.streamsComplete}`;
	} catch (error) {
		detail = `driver error ${error.message}`;
	}
	writeFileSync(join(ctx.qaRoot, "git-bash-protocol-receipt.json"), JSON.stringify(receipt, null, 2) + "\n");
	return {
		status: ok ? "pass" : "blocked",
		blocker: ok ? null : `protocol: ${detail} (a served host must answer initialize/tools-list)`,
		artifacts: ["git-bash-protocol-receipt.json"],
		cleanup_ok: receipt.cleanup.processExited,
	};
}

const SCENARIOS = { disabled: scenarioDisabled, usage: scenarioUsage, protocol: scenarioProtocol };

async function main() {
	const args = parseArgs(process.argv.slice(2));
	const ctx = { binary: resolve(args.binary), qaRoot: join(resolve(args.evidence), "qa") };
	mkdirSync(ctx.qaRoot, { recursive: true });

	const binaryBefore = existsSync(ctx.binary) ? { sha256: sha256(readFileSync(ctx.binary)), mtimeMs: statSync(ctx.binary).mtimeMs } : null;
	if (!binaryBefore) console.error(`latest-omo-git-bash-qa: --binary ${ctx.binary} is missing; every scenario will be blocked`);

	const names = args.scenario === "all" ? Object.keys(SCENARIOS) : [args.scenario];
	for (const name of names) if (!SCENARIOS[name]) usage(`unknown scenario ${name}`);

	const scenarios = {};
	let anyBlocked = false;
	for (const name of names) {
		console.log(`latest-omo-git-bash-qa: scenario ${name}`);
		let result;
		try {
			result = await SCENARIOS[name](ctx);
		} catch (error) {
			result = { status: "blocked", blocker: `${name}: driver error ${error.message}`, artifacts: [], cleanup_ok: false };
		}
		if (result.status === "blocked" || result.cleanup_ok === false) anyBlocked = true;
		scenarios[name] = { status: result.status, blocker: result.blocker ?? null, reason: result.reason ?? null, artifacts: result.artifacts ?? [], cleanup_ok: result.cleanup_ok ?? true };
	}

	const binaryAfter = existsSync(ctx.binary) ? { sha256: sha256(readFileSync(ctx.binary)), mtimeMs: statSync(ctx.binary).mtimeMs } : null;
	const binaryStable = JSON.stringify(binaryBefore) === JSON.stringify(binaryAfter);
	if (!binaryStable) anyBlocked = true;

	const report = {
		schema: SCHEMA,
		binary: ctx.binary,
		binary_before: binaryBefore,
		binary_after: binaryAfter,
		binary_stable: binaryStable,
		scenarios,
		status: anyBlocked ? "blocked" : "pass",
	};
	writeFileSync(join(ctx.qaRoot, "latest-omo-git-bash-qa.json"), JSON.stringify(report, null, 2) + "\n");
	console.log(`latest-omo-git-bash-qa: ${report.status} -> ${join(ctx.qaRoot, "latest-omo-git-bash-qa.json")}`);
	process.exit(report.status === "pass" ? 0 : 1);
}

await main();
