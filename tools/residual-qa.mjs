#!/usr/bin/env bun
// Real-surface residual QA driver for the assembled session2-residual gate.
//
//   bun tools/residual-qa.mjs --binary <mhc> --installed <E/install/mhc> --scenario all --evidence <E>
//   bun tools/residual-qa.mjs --binary <mhc> --scenario tui|theme|install|mini|server|registry ...
//
// Every scenario drives the REAL binary with an isolated HOME, an offline loopback provider, exact
// pre-subscribed predicates and bounded timeouts, and records its actual generated artifacts. A
// scenario whose required surface is not reachable records status "blocked" with the exact missing
// contract — never a proxy pass and never a stub. It writes `$E/qa/residual-qa.json`
// (schema session2-residual-qa/v1) plus per-scenario artifacts.
import { createHash } from "node:crypto";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "..");
const HARNESS = join(REPO, ".omo", "evidence", "session2-residual", "harness-v2");
const XTERM = "/home/indo/.bun/install/cache/@xterm/headless@6.0.0@@@1/lib-headless/xterm-headless.js";

const SCHEMA = "session2-residual-qa/v1";
const REPLY = "offline acceptance";
const ACCENT = "#ff8800"; // custom theme accent; its truecolor must appear in the TUI stream

function usage(message) {
	console.error(`residual-qa: ${message}`);
	console.error("usage: bun tools/residual-qa.mjs --binary <mhc> --installed <mhc> --scenario <tui|theme|install|mini|server|registry|all> --evidence <E>");
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
		else if (key === "--installed") args.installed = argv[++i];
		else if (key === "--scenario") args.scenario = argv[++i];
		else if (key === "--evidence") args.evidence = argv[++i];
		else usage(`unknown argument ${key}`);
	}
	if (!args.binary) usage("--binary is required");
	if (!args.evidence) usage("--evidence is required");
	return args;
}

const { startLoopback, writeOfflineAgent, markOnboardingComplete } = await import(join(HARNESS, "qa-loopback-lib.mjs"));

const newHome = (prefix) => mkdtempSync(join(tmpdir(), prefix));

/** Spawns a command with a hard timeout. On timeout the child is SIGKILLed and its exit is still
 *  awaited, so no process is left behind. Returns { exitCode, stdout, stderr, timedOut }. */
async function run(command, argv, { cwd, env, timeoutMs = 120_000, stdin } = {}) {
	const proc = Bun.spawn([command, ...argv], {
		cwd: cwd ?? process.cwd(),
		env: env ?? { PATH: process.env.PATH },
		stdin: stdin === undefined ? "ignore" : "pipe",
		stdout: "pipe",
		stderr: "pipe",
	});
	let timedOut = false;
	const timer = setTimeout(() => {
		timedOut = true;
		proc.kill("SIGKILL");
	}, timeoutMs);
	try {
		if (stdin !== undefined) {
			proc.stdin.write(stdin);
			proc.stdin.end();
		}
		const [stdout, stderr, exitCode] = await Promise.all([
			new Response(proc.stdout).text(),
			new Response(proc.stderr).text(),
			proc.exited,
		]);
		return { exitCode, stdout, stderr, timedOut };
	} finally {
		clearTimeout(timer);
	}
}

/** Renders a raw ANSI stream to a same-geometry text grid. xterm parses asynchronously, so the
 *  write callbacks are awaited before the buffer is read. */
async function renderGrid(ansiPath, cols, rows) {
	const { Terminal } = await import(XTERM);
	const vt = new Terminal({ cols, rows, allowProposedApi: true, scrollback: 0 });
	const text = readFileSync(ansiPath, "utf8");
	await new Promise((resolvePromise) => vt.write(text, () => resolvePromise()));
	const buf = vt.buffer.active;
	const lines = [];
	for (let y = 0; y < rows; y++) lines.push(buf.getLine(buf.viewportY + y)?.translateToString(true) ?? "");
	vt.dispose();
	return lines.join("\n") + "\n";
}

async function renderPng(dir, cols, rows) {
	const ansiPath = join(dir, "terminal-ansi.txt");
	if (!existsSync(ansiPath)) return { ok: false, detail: "no terminal-ansi.txt" };
	writeFileSync(join(dir, "grid.txt"), await renderGrid(ansiPath, cols, rows));
	const png = await run("bun", [join(HARNESS, "png-render.mjs"), join(dir, "grid.txt"), String(cols), String(rows), join(dir, "terminal.png"), join(dir, "terminal.png.json")], {
		cwd: REPO,
		env: { PATH: process.env.PATH },
		timeoutMs: 60_000,
	});
	if (png.exitCode !== 0 || !existsSync(join(dir, "terminal.png"))) {
		return { ok: false, detail: `png-render exit=${png.exitCode} ${png.stderr.trim()}` };
	}
	const meta = JSON.parse(readFileSync(join(dir, "terminal.png.json"), "utf8"));
	const pngBytes = readFileSync(join(dir, "terminal.png"));
	if (meta.cols !== cols || meta.rows !== rows || meta.pngSha256 !== sha256(pngBytes)) {
		return { ok: false, detail: "png metadata mismatch" };
	}
	return { ok: true, detail: `png ${cols * 8}x${rows * 16}` };
}

// ---------------------------------------------------------------------------------------------
// Scenario: tui — real causal interactive sequence per geometry x mode + same-geometry PNG.
// ---------------------------------------------------------------------------------------------

const TUI_REQUIRED = [
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

async function scenarioTui(ctx) {
	const artifacts = [];
	const cases = [];
	let cleanupOk = true;
	for (const geometry of ["80x24", "120x36", "200x50"]) {
		for (const mode of ["regular", "fullscreen"]) {
			const [cols, rows] = geometry.split("x").map(Number);
			const dir = join(ctx.qaRoot, `tui-${geometry}-${mode}`);
			mkdirSync(dir, { recursive: true });
			const home = newHome("residual-tui-home-");
			try {
				const harness = await run("bun", [join(HARNESS, "tui-driver.mjs"), ctx.binary, dir, String(cols), String(rows), mode], {
					cwd: REPO,
					env: { PATH: process.env.PATH, HOME: home },
					timeoutMs: 300_000,
				});
				writeFileSync(join(dir, "driver.log"), harness.stdout + harness.stderr);
				const evidence = existsSync(join(dir, "evidence.json")) ? JSON.parse(readFileSync(join(dir, "evidence.json"), "utf8")) : {};
				const png = await renderPng(dir, cols, rows);
				const missing = TUI_REQUIRED.filter((field) => evidence[field] !== true);
				const pass = harness.exitCode === 0 && !harness.timedOut && evidence.exit_code === 0 && missing.length === 0 && png.ok;
				artifacts.push(`tui-${geometry}-${mode}`);
				cases.push({ geometry, mode, pass, detail: missing.length ? `missing ${missing.join(",")}` : png.detail });
			} finally {
				rmSync(home, { recursive: true, force: true });
				if (existsSync(home)) cleanupOk = false;
			}
		}
	}
	const failing = cases.filter((c) => !c.pass);
	return {
		status: failing.length === 0 && cleanupOk ? "pass" : "blocked",
		blocker: failing.length === 0 && cleanupOk ? null : `tui: ${failing.map((c) => `${c.geometry}/${c.mode}(${c.detail})`).join("; ")}${cleanupOk ? "" : " cleanup=false"}`,
		artifacts,
		cleanup_ok: cleanupOk,
	};
}

// ---------------------------------------------------------------------------------------------
// Scenario: theme — custom registered theme renders (accent truecolor) / malformed falls back.
// ---------------------------------------------------------------------------------------------

function customThemeDocument(name, accent) {
	return (
		JSON.stringify(
			{
				name,
				vars: { accent },
				colors: { accent: "accent", border: "accent", borderAccent: "accent", text: accent, muted: accent },
			},
			null,
			2,
		) + "\n"
	);
}

async function scenarioTheme(ctx) {
	const artifacts = [];
	const home = newHome("residual-theme-home-");
	const scratch = newHome("residual-theme-scratch-");
	let cleanupOk = false;
	let ok = false;
	let detail = "";
	try {
		const good = join(scratch, "custom-accent.json");
		const bad = join(scratch, "broken-fallback.json");
		writeFileSync(good, customThemeDocument("custom-accent", ACCENT));
		writeFileSync(bad, "{ this is not valid theme json ");
		const goodDir = join(ctx.qaRoot, "theme-custom");
		const badDir = join(ctx.qaRoot, "theme-fallback");
		mkdirSync(goodDir, { recursive: true });
		mkdirSync(badDir, { recursive: true });
		const goodRun = await run("bun", [join(HARNESS, "tui-driver.mjs"), ctx.binary, goodDir, "120", "36", "regular", good, "custom-accent"], {
			cwd: REPO,
			env: { PATH: process.env.PATH, HOME: home },
			timeoutMs: 300_000,
		});
		const badRun = await run("bun", [join(HARNESS, "tui-driver.mjs"), ctx.binary, badDir, "120", "36", "regular", bad, "broken-fallback"], {
			cwd: REPO,
			env: { PATH: process.env.PATH, HOME: home },
			timeoutMs: 300_000,
		});
		writeFileSync(join(ctx.qaRoot, "theme-custom.log"), goodRun.stdout + goodRun.stderr);
		writeFileSync(join(ctx.qaRoot, "theme-fallback.log"), badRun.stdout + badRun.stderr);
		const goodEvidence = existsSync(join(goodDir, "evidence.json")) ? JSON.parse(readFileSync(join(goodDir, "evidence.json"), "utf8")) : {};
		const badEvidence = existsSync(join(badDir, "evidence.json")) ? JSON.parse(readFileSync(join(badDir, "evidence.json"), "utf8")) : {};
		artifacts.push("theme-custom.log", "theme-fallback.log", "theme-custom/", "theme-fallback/");
		const accentRendered = goodEvidence.theme_accent_rendered === true;
		const fallbackStarted = badEvidence.startup_predicate === true;
		const fallbackDiagnostic = badEvidence.theme_fallback_diagnostic === true;
		ok = goodRun.exitCode === 0 && badRun.exitCode === 0 && accentRendered && fallbackStarted && fallbackDiagnostic;
		detail = `accentRendered=${accentRendered} fallbackStarted=${fallbackStarted} fallbackDiagnostic=${fallbackDiagnostic}`;
	} finally {
		rmSync(home, { recursive: true, force: true });
		rmSync(scratch, { recursive: true, force: true });
		cleanupOk = !existsSync(home) && !existsSync(scratch);
	}
	return {
		status: ok && cleanupOk ? "pass" : "blocked",
		blocker: ok && cleanupOk ? null : `theme: ${detail} cleanup=${cleanupOk}`,
		artifacts,
		cleanup_ok: cleanupOk,
	};
}

// ---------------------------------------------------------------------------------------------
// Scenario: install — staged runtime verifies; a removed helper MUST be present and diagnosed.
// ---------------------------------------------------------------------------------------------

async function scenarioInstall(ctx) {
	const artifacts = [];
	if (!ctx.installed || !existsSync(ctx.installed)) {
		return { status: "blocked", blocker: `install: staged binary missing (${ctx.installed}); run tools/package-native.mjs first`, artifacts, cleanup_ok: true };
	}
	const stageDir = dirname(ctx.installed);
	const verify = await run("bun", [join(REPO, "tools", "package-native.mjs"), "--verify-only", stageDir], {
		cwd: REPO,
		env: { PATH: process.env.PATH, OMO_SRC: process.env.OMO_SRC ?? "/home/indo/code/oh-my-openagent" },
		timeoutMs: 120_000,
	});
	writeFileSync(join(ctx.qaRoot, "install-verify.log"), verify.stdout + verify.stderr);
	artifacts.push("install-verify.log", "manifest.json");
	const scratch = newHome("residual-install-scratch-");
	let cleanupOk = false;
	let ok = false;
	let detail = "";
	try {
		cpSync(stageDir, scratch, { recursive: true });
		const helper = join(scratch, "ast-grep-mcp");
		const hadHelper = existsSync(helper);
		if (hadHelper) rmSync(helper, { force: true });
		const negative = await run("bun", [join(REPO, "tools", "package-native.mjs"), "--verify-only", scratch], { cwd: REPO, env: { PATH: process.env.PATH }, timeoutMs: 120_000 });
		writeFileSync(join(ctx.qaRoot, "install-missing-helper.log"), negative.stdout + negative.stderr);
		artifacts.push("install-missing-helper.log");
		const diagnosed = negative.exitCode !== 0 && /ast-grep-mcp|missing|helper/i.test(negative.stdout + negative.stderr);
		// The staged runtime MUST include the native helper; removing it MUST then be diagnosed.
		ok = verify.exitCode === 0 && hadHelper && diagnosed;
		detail = `verifyExit=${verify.exitCode} hadHelper=${hadHelper} diagnosed=${diagnosed}`;
	} finally {
		rmSync(scratch, { recursive: true, force: true });
		cleanupOk = !existsSync(scratch);
	}
	return {
		status: ok && cleanupOk ? "pass" : "blocked",
		blocker: ok && cleanupOk ? null : `install: ${detail} cleanup=${cleanupOk}`,
		artifacts,
		cleanup_ok: cleanupOk,
	};
}

// ---------------------------------------------------------------------------------------------
// Scenario: mini — the native mini server/attach loopback.
// ---------------------------------------------------------------------------------------------

async function scenarioMini() {
	// The mini surface is NOT reachable from the assembled binary: main.rs dispatches no
	// experimental/mini entry (only `experimental::cli::parse` exists, with no caller), so a real
	// mini server/attach round-trip cannot be driven through the CLI. Its protocol is exercised by
	// the Rust integration targets (crates/maho-cli/tests/mini_server.rs, mini_session.rs) in the
	// nextest gate. Driving a generic `--mode rpc` request and labelling it "mini" would be a proxy
	// false green, so it is reported blocked with the exact contract.
	return {
		status: "blocked",
		blocker:
			"mini: no reachable CLI entry for the native mini server/attach loopback " +
			"(main.rs does not dispatch experimental/mini::{run_server_entry,run_worker_entry,run_tui_entry}); " +
			"the mini protocol is covered by crates/maho-cli/tests/mini_server.rs + mini_session.rs in the nextest gate. " +
			"Owner: task 16 CLI registry/entry wiring must expose the mini entry before a real-binary mini scenario can run.",
		artifacts: [],
		cleanup_ok: true,
	};
}

// ---------------------------------------------------------------------------------------------
// Scenario: server — the ported senpi app-server ndjson round-trip.
// ---------------------------------------------------------------------------------------------

async function scenarioServer(ctx) {
	const home = newHome("residual-server-home-");
	const artifacts = ["server-app-server.log"];
	let cleanupOk = false;
	let ok = false;
	let detail = "";
	try {
		markOnboardingComplete(home);
		const agent = join(home, ".maho", "agent");
		mkdirSync(agent, { recursive: true });
		const proc = Bun.spawn([ctx.binary, "app-server", "--listen", "stdio://"], {
			cwd: home,
			env: { PATH: process.env.PATH, HOME: home, MAHO_CODING_AGENT_DIR: agent },
			stdin: "pipe",
			stdout: "pipe",
			stderr: "pipe",
		});
		const stdoutText = new Response(proc.stdout).text();
		const stderrText = new Response(proc.stderr).text();
		proc.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: {} })}\n`);
		proc.stdin.end();
		const deadline = setTimeout(() => proc.kill("SIGKILL"), 30_000);
		const [text, code, stderr] = await Promise.all([stdoutText, proc.exited, stderrText]);
		clearTimeout(deadline);
		writeFileSync(join(ctx.qaRoot, "server-app-server.log"), text + stderr);
		// A live ndjson app-server replies with a line carrying the request id (a response or a
		// typed error both prove the protocol loop); exit 0 on stdin EOF proves a clean shutdown.
		const responded = text.trim().split("\n").filter(Boolean).some((line) => {
			try {
				return JSON.parse(line).id === 1;
			} catch {
				return false;
			}
		});
		ok = code === 0 && responded;
		detail = `exit=${code} responded=${responded}`;
	} finally {
		rmSync(home, { recursive: true, force: true });
		cleanupOk = !existsSync(home);
	}
	return {
		status: ok && cleanupOk ? "pass" : "blocked",
		blocker: ok && cleanupOk ? null : `server: ${detail} cleanup=${cleanupOk} (app-server did not complete a live ndjson round-trip and clean EOF shutdown)`,
		artifacts,
		cleanup_ok: cleanupOk,
	};
}

// ---------------------------------------------------------------------------------------------
// Scenario: registry — registered provider list semantics + RPC own-prompt lifecycle.
// ---------------------------------------------------------------------------------------------

async function scenarioRegistry(ctx) {
	const home = newHome("residual-registry-home-");
	const artifacts = [];
	let cleanupOk = false;
	let ok = false;
	let detail = "";
	try {
		const agent = join(home, ".maho", "agent");
		mkdirSync(agent, { recursive: true });
		markOnboardingComplete(home);
		// A registered provider + model in the isolated agent dir; `--list-models` must surface it.
		writeFileSync(
			join(agent, "models.json"),
			JSON.stringify({
				providers: {
					"residual-registry": {
						api: "openai-completions",
						baseUrl: "http://127.0.0.1:9/v1",
						apiKey: "offline-fixture",
						models: [{ id: "residual-model", reasoning: false, input: ["text"], contextWindow: 128000, maxTokens: 4096 }],
					},
				},
			}),
		);
		const listModels = await run(ctx.binary, ["--list-models", "residual"], {
			cwd: home,
			env: { PATH: process.env.PATH, HOME: home, MAHO_CODING_AGENT_DIR: agent },
			timeoutMs: 30_000,
		});
		writeFileSync(join(ctx.qaRoot, "registry-list-models.log"), listModels.stdout + listModels.stderr);
		artifacts.push("registry-list-models.log");
		const providerListed = listModels.exitCode === 0 && listModels.stdout.includes("residual-registry") && listModels.stdout.includes("residual-model");

		// RPC own-prompt lifecycle over the loopback: a real prompt produces a response, not just stats.
		const { server, baseUrl } = startLoopback();
		let rpcPrompted = false;
		let rpcExit = null;
		try {
			const rpcAgent = writeOfflineAgent(home, baseUrl);
			const proc = Bun.spawn([ctx.binary, "--mode", "rpc", "--offline", "--no-session", "--no-tools", "--no-skills", "--no-prompt-templates", "--model", "offline/offline"], {
				cwd: home,
				env: { PATH: process.env.PATH, HOME: home, MAHO_CODING_AGENT_DIR: rpcAgent },
				stdin: "pipe",
				stdout: "pipe",
				stderr: "pipe",
			});
			const stdoutText = new Response(proc.stdout).text();
			const stderrText = new Response(proc.stderr).text();
			proc.stdin.write(`${JSON.stringify({ type: "prompt", id: "prompt_1", prompt: "registry probe" })}\n`);
			proc.stdin.end();
			const deadline = setTimeout(() => proc.kill("SIGKILL"), 30_000);
			const [text, code, stderr] = await Promise.all([stdoutText, proc.exited, stderrText]);
			clearTimeout(deadline);
			writeFileSync(join(ctx.qaRoot, "registry-rpc.log"), text + stderr);
			artifacts.push("registry-rpc.log");
			rpcExit = code;
			rpcPrompted = text.includes("prompt_1") && text.includes(REPLY);
		} finally {
			server.stop(true);
		}
		ok = providerListed && rpcPrompted && rpcExit === 0;
		detail = `providerListed=${providerListed} rpcPrompted=${rpcPrompted} rpcExit=${rpcExit}`;
	} finally {
		rmSync(home, { recursive: true, force: true });
		cleanupOk = !existsSync(home);
	}
	return {
		status: ok && cleanupOk ? "pass" : "blocked",
		blocker: ok && cleanupOk ? null : `registry: ${detail} cleanup=${cleanupOk} (registered provider/model must appear in --list-models and an RPC prompt must return the reply)`,
		artifacts,
		cleanup_ok: cleanupOk,
	};
}

// ---------------------------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------------------------

const SCENARIOS = { tui: scenarioTui, theme: scenarioTheme, install: scenarioInstall, mini: scenarioMini, server: scenarioServer, registry: scenarioRegistry };

async function main() {
	const args = parseArgs(process.argv.slice(2));
	const ctx = { binary: resolve(args.binary), installed: args.installed ? resolve(args.installed) : null, qaRoot: join(resolve(args.evidence), "qa") };
	mkdirSync(ctx.qaRoot, { recursive: true });
	const names = args.scenario === "all" ? Object.keys(SCENARIOS) : [args.scenario];
	for (const name of names) if (!SCENARIOS[name]) usage(`unknown scenario ${name}`);

	const scenarios = {};
	let anyBlocked = false;
	for (const name of names) {
		console.log(`residual-qa: scenario ${name}`);
		let result;
		try {
			result = await SCENARIOS[name](ctx);
		} catch (error) {
			result = { status: "blocked", blocker: `${name}: driver error ${error.message}`, artifacts: [], cleanup_ok: false };
		}
		if (result.status !== "pass") anyBlocked = true;
		scenarios[name] = {
			status: result.status,
			blocker: result.blocker ?? null,
			artifacts: result.artifacts ?? [],
			cleanup_ok: result.cleanup_ok ?? true,
		};
		console.log(`residual-qa: ${name} -> ${result.status}${result.blocker ? ` (${result.blocker})` : ""}`);
	}

	writeFileSync(
		join(ctx.qaRoot, "residual-qa.json"),
		JSON.stringify(
			{ schema: SCHEMA, binary: ctx.binary, binary_sha256: existsSync(ctx.binary) ? sha256(readFileSync(ctx.binary)) : null, scenarios },
			null,
			2,
		) + "\n",
	);
	console.log(`residual-qa: wrote ${join(ctx.qaRoot, "residual-qa.json")}`);
	process.exit(anyBlocked ? 1 : 0);
}

if (import.meta.main) {
	await main();
}
