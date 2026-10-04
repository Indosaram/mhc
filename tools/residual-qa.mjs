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
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, watch, writeFileSync } from "node:fs";
import { createConnection } from "node:net";
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

// The mini scenario uses a UNIQUE prompt + reply so a settled transcript can be proven causally
// (not merely accepted): the loopback records the request body and returns MINI_REPLY, and the
// scenario asserts both that the provider saw MINI_PROMPT and that a lane event carried MINI_REPLY.
const MINI_PROMPT = "residual-mini-probe-unique";
const MINI_REPLY = "mini-loopback-transcript-ack";

const newHome = (prefix) => mkdtempSync(join(tmpdir(), prefix));

/** A dependency is not yet assembled (the mini entry is not dispatched): blocked, not a regression. */
class MiniBlocked extends Error {}

/** A loopback provider that records every request body and always streams MINI_REPLY. */
function startRecordingLoopback() {
	const requests = [];
	const server = Bun.serve({
		hostname: "127.0.0.1",
		port: 0,
		async fetch(request) {
			let body = "";
			try {
				body = await request.text();
			} catch {
				/* ignore */
			}
			requests.push(body);
			const chunk = (delta, finish = null) =>
				`data: ${JSON.stringify({ id: "offline", object: "chat.completion.chunk", created: 0, model: "offline", choices: [{ index: 0, delta, finish_reason: finish }] })}\n\n`;
			const text = chunk({ role: "assistant", content: MINI_REPLY }) + chunk({}, "stop") + "data: [DONE]\n\n\n";
			return new Response(text, { headers: { "content-type": "text/event-stream" } });
		},
	});
	return { server, baseUrl: `http://127.0.0.1:${server.port}/v1`, requests };
}

/** Reads the pinned builtin skill name set from the native telemetry crate source. */
function pinnedBuiltinSkills(repo) {
	const src = readFileSync(join(repo, "crates", "omo", "components", "maho-omo-telemetry", "src", "product_identity.rs"), "utf8");
	const match = src.match(/BUILTIN_SKILL_NAMES\s*:\s*&\[&str\]\s*=\s*&\[([\s\S]*?)\]/);
	if (!match) throw new Error("could not read BUILTIN_SKILL_NAMES from product_identity.rs");
	return [...match[1].matchAll(/"([^"]+)"/g)].map((entry) => entry[1]);
}

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

/** Connects to the mini unix socket and returns a minimal NDJSON client with `call`, `events`, and
 *  `waitForEvent`. */
async function miniConnect(socketPath, timeoutMs) {
	return await new Promise((resolvePromise, reject) => {
		const socket = createConnection({ path: socketPath });
		let buffer = "";
		const pending = new Map();
		const eventBacklog = [];
		let client = null;
		const timer = setTimeout(() => reject(new Error("mini socket connect timeout")), timeoutMs);
		socket.on("data", (chunk) => {
			buffer += chunk.toString("utf8");
			let index;
			while ((index = buffer.indexOf("\n")) >= 0) {
				const line = buffer.slice(0, index).trim();
				buffer = buffer.slice(index + 1);
				if (!line) continue;
				let frame;
				try {
					frame = JSON.parse(line);
				} catch {
					continue;
				}
				if ((frame.kind === "result" || frame.kind === "error") && pending.has(frame.id)) {
					const waiter = pending.get(frame.id);
					pending.delete(frame.id);
					if (frame.kind === "result") waiter.resolve(frame.result);
					else waiter.reject(new Error(frame.error));
				} else if (frame.kind === "event") {
					if (client?._push) client._push(frame.payload);
					else eventBacklog.push(frame.payload);
				}
			}
		});
		socket.once("connect", () => {
			clearTimeout(timer);
			const events = [];
			const eventWaiters = new Set();
			const deliver = (payload) => {
				for (const w of eventWaiters) if (w.pred(payload)) (eventWaiters.delete(w), w.resolve(payload));
			};
			client = {
				call(id, method, args, callTimeoutMs = 60_000) {
					return new Promise((res, rej) => {
						const callTimer = setTimeout(() => (pending.delete(id), rej(new Error(`mini call ${method} timed out`))), callTimeoutMs);
						pending.set(id, { resolve: (v) => (clearTimeout(callTimer), res(v)), reject: (e) => (clearTimeout(callTimer), rej(e)) });
						socket.write(JSON.stringify({ kind: "call", id, method, args }) + "\n");
					});
				},
				events,
				waitForEvent(pred, timeoutMs) {
					const existing = events.find((event) => pred(event));
					if (existing !== undefined) return Promise.resolve(existing);
					return new Promise((res, rej) => {
						const w = { pred, resolve: res };
						const eventTimer = setTimeout(() => (eventWaiters.delete(w), rej(new Error("mini lane event timeout"))), timeoutMs);
						w.resolve = (v) => (clearTimeout(eventTimer), res(v));
						eventWaiters.add(w);
					});
				},
				destroy: () => socket.destroy(),
				_push: (payload) => {
					events.push(payload);
					deliver(payload);
				},
			};
			for (const buffered of eventBacklog.splice(0)) client._push(buffered);
			resolvePromise(client);
		});
		socket.once("error", (error) => {
			clearTimeout(timer);
			reject(error);
		});
	});
}

async function miniCall(client, id, method, args) {
	return await client.call(id, method, args);
}

/** Waits for a socket file to appear using fs.watch on its parent (no polling loop); rejects on a
 *  bounded timeout. The watcher is registered BEFORE the caller spawns the producer. */
function waitForSocket(socketPath, parentDir, timeoutMs) {
	if (existsSync(socketPath)) return Promise.resolve();
	return new Promise((resolve, reject) => {
		const watcher = watch(parentDir, () => {
			if (existsSync(socketPath)) {
				clearTimeout(timer);
				watcher.close();
				resolve();
			}
		});
		const timer = setTimeout(() => {
			watcher.close();
			reject(new Error("mini socket did not appear"));
		}, timeoutMs);
	});
}

/** True when the process group is fully gone (no member answers signal 0). */
function processGroupGone(pid) {
	try {
		process.kill(-pid, 0);
		return false;
	} catch (error) {
		return error.code === "ESRCH";
	}
}

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
	const cellsPath = join(dir, "reply-cells.json");
	const ansiPath = join(dir, "terminal-ansi.txt");
	let input;
	if (existsSync(cellsPath)) {
		input = cellsPath; // per-cell positions + colors (theme-faithful)
	} else if (existsSync(ansiPath)) {
		writeFileSync(join(dir, "grid.txt"), await renderGrid(ansiPath, cols, rows));
		input = join(dir, "grid.txt");
	} else {
		return { ok: false, detail: "no reply-cells.json or terminal-ansi.txt" };
	}
	const png = await run("bun", [join(HARNESS, "png-render.mjs"), input, String(cols), String(rows), join(dir, "terminal.png"), join(dir, "terminal.png.json")], {
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
	return { ok: true, detail: `png ${cols * 8}x${rows * 16}${meta.colored ? " colored" : ""}` };
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
	// Real surface (plan task 3): the staged runtime must carry ALL pinned builtin skills, and the
	// native ast-grep MCP helper must answer an MCP initialize handshake.
	const skillNames = pinnedBuiltinSkills(REPO);
	const missingSkills = skillNames.filter((name) => !existsSync(join(stageDir, "skills", name, "SKILL.md")));
	const helper = join(stageDir, "ast-grep-mcp");
	const hadHelper = existsSync(helper);
	let handshakeOk = false;
	if (hadHelper) {
		try {
			const proc = Bun.spawn([helper], { stdin: "pipe", stdout: "pipe", stderr: "pipe" });
			proc.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2024-11-05", capabilities: {}, clientInfo: { name: "residual-qa", version: "1" } } })}\n`);
			proc.stdin.end();
			const deadline = setTimeout(() => proc.kill("SIGKILL"), 15_000);
			const [out] = await Promise.all([new Response(proc.stdout).text(), proc.exited]);
			clearTimeout(deadline);
			handshakeOk = out.split("\n").filter(Boolean).some((line) => {
				try {
					const msg = JSON.parse(line);
					return msg.id === 1 && (msg.result !== undefined || msg.error !== undefined);
				} catch {
					return false;
				}
			});
		} catch {
			handshakeOk = false;
		}
	}
	const scratch = newHome("residual-install-scratch-");
	let cleanupOk = false;
	let ok = false;
	let detail = "";
	try {
		cpSync(stageDir, scratch, { recursive: true });
		const scratchHelper = join(scratch, "ast-grep-mcp");
		if (existsSync(scratchHelper)) rmSync(scratchHelper, { force: true });
		const negative = await run("bun", [join(REPO, "tools", "package-native.mjs"), "--verify-only", scratch], { cwd: REPO, env: { PATH: process.env.PATH }, timeoutMs: 120_000 });
		writeFileSync(join(ctx.qaRoot, "install-missing-helper.log"), negative.stdout + negative.stderr);
		artifacts.push("install-missing-helper.log");
		const diagnosed = negative.exitCode !== 0 && /ast-grep-mcp|missing|helper/i.test(negative.stdout + negative.stderr);
		ok = verify.exitCode === 0 && missingSkills.length === 0 && hadHelper && handshakeOk && diagnosed;
		detail = `verifyExit=${verify.exitCode} skills=${skillNames.length - missingSkills.length}/${skillNames.length} helper=${hadHelper} handshake=${handshakeOk} diagnosed=${diagnosed}`;
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

async function scenarioMini(ctx) {
	// Real mini server/attach loopback: spawn the mini server (the `__PI_INTERNAL_SPAWN=server` role
	// the pinned `experimental/mini` main.ts uses) in its OWN process group, connect a client over its
	// unix socket, and drive the pinned NDJSON protocol: `sessions.list` -> `sessions.attach` ->
	// `lane.watch` -> `lane.start` -> `lane.prompt`, then assert a UNIQUE reply reached the settled
	// transcript (not merely that the prompt was accepted).
	const artifacts = [];
	const home = newHome("residual-mini-home-");
	const socketPath = join(home, "mini.sock");
	const sessionsRoot = join(home, "mini-sessions");
	let cleanupOk = false;
	let ok = false;
	let detail = "";
	let status = "blocked";
	let server;
	let serverOut;
	let serverErr;
	const { server: loop, baseUrl, requests } = startRecordingLoopback();
	try {
		const agent = writeOfflineAgent(home, baseUrl);
		markOnboardingComplete(home);
		// Register the socket-creation watch BEFORE spawning the producer, then spawn.
		const socketReady = waitForSocket(socketPath, home, 15_000);
		// `detached: true` makes the server a process-group/session leader (pgid==sid==pid) so every
		// worker it spawns inherits the group; teardown targets ONLY that group (never a broad pkill).
		server = Bun.spawn([ctx.binary, socketPath, sessionsRoot], {
			cwd: home,
			env: { PATH: process.env.PATH, HOME: home, MAHO_CODING_AGENT_DIR: agent, __PI_INTERNAL_SPAWN: "server" },
			detached: true,
			stdout: "pipe",
			stderr: "pipe",
		});
		serverOut = new Response(server.stdout).text();
		serverErr = new Response(server.stderr).text();
		try {
			await socketReady;
		} catch (error) {
			// The socket never appeared: if the server exited, the mini entry was not dispatched (a
			// dependency, not a regression) -> blocked; a live server that never listened -> fail.
			detail = `mini: ${error.message} (server exit=${server.exitCode ?? "running"})`;
			throw new MiniBlocked(detail);
		}
		status = "fail";
		const client = await miniConnect(socketPath, 15_000);
		let nextId = 1;
		try {
			const sessions = await miniCall(client, nextId++, "sessions.list", []);
			const sessionId = await miniCall(client, nextId++, "sessions.attach", [null, home, "qa-presentation"]);
			const watch = await miniCall(client, nextId++, "lane.watch", ["qa-presentation"]);
			const subscriptionId = watch?.subscriptionId ?? watch?.subscription_id ?? null;
			if (subscriptionId) await miniCall(client, nextId++, "lane.start", [subscriptionId]);
			// Subscribe to the settled-transcript event BEFORE the prompt so no event is missed
			// (the client replays any event buffered before registration).
			const settledEvent = client.waitForEvent(
				(payload) => payload?.event?.type === "entry_added" && JSON.stringify(payload).includes(MINI_REPLY),
				60_000,
			);
			const promptResult = await miniCall(client, nextId++, "lane.prompt", [MINI_PROMPT]);
			const settled = promptResult && promptResult.ok === true;
			const providerSawPrompt = requests.some((body) => body.includes(MINI_PROMPT));
			let transcript = null;
			try {
				transcript = await settledEvent;
			} catch {
				transcript = null;
			}
			const listed = Array.isArray(sessions);
			const attached = typeof sessionId === "string" && sessionId.length > 0;
			ok = listed && attached && subscriptionId !== null && settled && providerSawPrompt && transcript !== null;
			detail = `list=${listed} attach=${attached} watch=${subscriptionId !== null} settled=${settled} providerSawPrompt=${providerSawPrompt} transcript=${transcript !== null}`;
			writeFileSync(join(ctx.qaRoot, "mini-protocol.log"), JSON.stringify({ sessions, sessionId, subscriptionId, promptResult, providerSawPrompt, eventCount: client.events.length }, null, 2) + "\n");
			artifacts.push("mini-protocol.log");
		} finally {
			client.destroy();
		}
	} catch (error) {
		if (error instanceof MiniBlocked) {
			status = "blocked";
			detail = error.message;
		} else {
			status = "fail";
			detail = `mini: ${error.message}`;
		}
	} finally {
		// Teardown of the OWNED tree. The server installs its own SIGINT/SIGTERM handler
		// (entry.rs::run_server_entry) that aborts the shutdown signal and stops every route worker,
		// so SIGTERM to the group is the graceful path; the group also carries the workers. Then the
		// group is SIGKILLed unconditionally and its absence is asserted (no member answers signal 0),
		// so a leaked worker is a cleanup failure. Never a broad pkill.
		if (server) {
			try {
				process.kill(-server.pid, "SIGTERM");
			} catch {
				/* group already gone */
			}
			await Promise.race([server.exited, new Promise((r) => setTimeout(r, 8_000))]);
			try {
				process.kill(-server.pid, "SIGKILL");
			} catch {
				/* group already gone */
			}
			await Promise.race([server.exited, new Promise((r) => setTimeout(r, 3_000))]);
		}
		await Promise.race([Promise.all([serverOut, serverErr]), new Promise((r) => setTimeout(r, 2_000))]);
		loop.stop(true);
		const groupGone = !server || processGroupGone(server.pid);
		rmSync(home, { recursive: true, force: true });
		cleanupOk = groupGone && !existsSync(home);
		if (!groupGone) detail = `${detail} leaked-group=${server?.pid}`;
	}
	return {
		status: ok && cleanupOk ? "pass" : status === "blocked" ? "blocked" : "fail",
		blocker: ok && cleanupOk ? null : `${detail} cleanup=${cleanupOk}`,
		artifacts,
		cleanup_ok: cleanupOk,
	};
}

// ---------------------------------------------------------------------------------------------
// Scenario: server — the ported senpi app-server ndjson round-trip.
// ---------------------------------------------------------------------------------------------

/** A line-based JSON-RPC client over a child's stdio, with responses by id and a notification
 *  backlog + waiters (for `thread/goal/updated`). */
function appServerClient(child) {
	let buffer = "";
	const responses = new Map();
	const notifications = [];
	const waiters = new Set();
	const responseWaiters = new Map();
	const decoder = new TextDecoder("utf8");
	const handle = (line) => {
		let message;
		try {
			message = JSON.parse(line);
		} catch {
			return;
		}
		if (message.id !== undefined && (message.result !== undefined || message.error !== undefined)) {
			responses.set(message.id, message);
			const waiter = responseWaiters.get(message.id);
			if (waiter) {
				responseWaiters.delete(message.id);
				waiter(message);
			}
		} else if (message.method) {
			notifications.push(message);
			for (const w of waiters) if (w.pred(message)) (waiters.delete(w), w.resolve(message));
		}
	};
	(async () => {
		for await (const chunk of child.stdout) buffer += decoder.decode(chunk, { stream: true });
	})();
	const poll = setInterval(() => {
		let index;
		while ((index = buffer.indexOf("\n")) >= 0) {
			const line = buffer.slice(0, index).trim();
			buffer = buffer.slice(index + 1);
			if (line) handle(line);
		}
	}, 10);
	return {
		notifications,
		call(id, method, params, timeoutMs = 20_000) {
			return new Promise((resolve, reject) => {
				const timer = setTimeout(() => (responseWaiters.delete(id), reject(new Error(`${method} timed out`))), timeoutMs);
				responseWaiters.set(id, (message) => (clearTimeout(timer), resolve(message)));
				child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
			});
		},
		waitForNotification(pred, timeoutMs) {
			const existing = notifications.find(pred);
			if (existing) return Promise.resolve(existing);
			return new Promise((resolve, reject) => {
				const w = { pred, resolve };
				const timer = setTimeout(() => (waiters.delete(w), reject(new Error("notification timeout"))), timeoutMs);
				w.resolve = (v) => (clearTimeout(timer), resolve(v));
				waiters.add(w);
			});
		},
		stop: () => clearInterval(poll),
	};
}

async function scenarioServer(ctx) {
	// Real app-server protocol (pinned senpi `modes/app-server`): a SUCCESSFUL exact `initialize`,
	// then `thread/start`, then `thread/goal/set` + `thread/goal/get` with a `thread/goal/updated`
	// notification. A typed error for `initialize` is NOT accepted as success (that only proves
	// framing). The goal round-trip follows the task-12 handlers.
	const home = newHome("residual-server-home-");
	const artifacts = ["server-app-server.log"];
	const objective = "residual-server-goal-probe";
	let cleanupOk = false;
	let ok = false;
	let detail = "";
	let status = "blocked";
	let proc;
	let client;
	let stderrText;
	let deadline;
	try {
		markOnboardingComplete(home);
		const agent = join(home, ".maho", "agent");
		mkdirSync(agent, { recursive: true });
		proc = Bun.spawn([ctx.binary, "app-server", "--listen", "stdio://"], {
			cwd: home,
			env: { PATH: process.env.PATH, HOME: home, MAHO_CODING_AGENT_DIR: agent },
			detached: true,
			stdin: "pipe",
			stdout: "pipe",
			stderr: "pipe",
		});
		stderrText = new Response(proc.stderr).text();
		client = appServerClient(proc);
		deadline = setTimeout(() => {
			try {
				process.kill(-proc.pid, "SIGKILL");
			} catch {
				/* group already gone */
			}
		}, 60_000);

		// 1. Exact initialize: a successful result carrying `userAgent` (not an error). A failed
		//    initialize is a runtime FAIL (protocol/init broken), never a blocked dependency.
		const init = await client.call(1, "initialize", { clientInfo: { name: "residual-qa", title: "residual gate", version: "1.0.0" } });
		const initOk = init.result !== undefined && typeof init.result.userAgent === "string" && init.result.userAgent.length > 0;
		if (!initOk) {
			status = "fail";
			detail = `initialize did not succeed: ${JSON.stringify(init).slice(0, 200)}`;
			throw new Error(detail);
		}
		status = "fail";

		// 2. thread/start → a thread id (core app-server; a missing id is a runtime FAIL).
		const started = await client.call(2, "thread/start", { cwd: home });
		const threadId = started.result?.thread?.id ?? started.result?.id ?? null;
		if (!threadId) throw new Error(`thread/start returned no thread id: ${JSON.stringify(started).slice(0, 200)}`);

		// 3. thread/goal/set + thread/goal/get (task-12 goal handlers). If the method is not
		//    assembled the error is a DEPENDENCY (blocked); a present-but-wrong result is a FAIL.
		const setResult = await client.call(3, "thread/goal/set", { threadId, objective });
		if (setResult.error !== undefined) {
			status = "blocked";
			throw new Error(`thread/goal/set unavailable: ${JSON.stringify(setResult.error).slice(0, 160)}`);
		}
		const getResult = await client.call(4, "thread/goal/get", { threadId });
		const goal = getResult.result?.goal ?? null;
		const roundTripped = goal?.objective === objective;
		let updated = null;
		try {
			updated = await client.waitForNotification((message) => message.method === "thread/goal/updated", 15_000);
		} catch {
			updated = null;
		}
		ok = roundTripped && updated !== null;
		detail = `initialize=${initOk} threadId=${threadId !== null} goalSet=${setResult.error === undefined} goalRoundTrip=${roundTripped} updatedNotification=${updated !== null}`;
		writeFileSync(join(ctx.qaRoot, "server-app-server.log"), JSON.stringify({ init: init.result, started: started.result, setResult, getResult: getResult.result, notifications: client.notifications.map((n) => n.method) }, null, 2) + "\n");
	} catch (error) {
		detail = `server: ${error.message}`;
	} finally {
		clearTimeout(deadline);
		if (client) client.stop();
		if (proc) {
			try {
				proc.stdin.end();
			} catch {
				/* already closed */
			}
			try {
				process.kill(-proc.pid, "SIGTERM");
			} catch {
				/* group already gone */
			}
			await Promise.race([proc.exited, new Promise((r) => setTimeout(r, 5_000))]);
			try {
				process.kill(-proc.pid, "SIGKILL");
			} catch {
				/* group already gone */
			}
			await Promise.race([proc.exited, new Promise((r) => setTimeout(r, 3_000))]);
		}
		await Promise.race([stderrText, new Promise((r) => setTimeout(r, 2_000))]);
		const groupGone = !proc || processGroupGone(proc.pid);
		rmSync(home, { recursive: true, force: true });
		cleanupOk = groupGone && !existsSync(home);
	}
	return {
		status: ok && cleanupOk ? "pass" : status,
		blocker: ok && cleanupOk ? null : `server: ${detail} cleanup=${cleanupOk}`,
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
	let rpcProc = null;
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
		let deadline;
		try {
			const rpcAgent = writeOfflineAgent(home, baseUrl);
			rpcProc = Bun.spawn([ctx.binary, "--mode", "rpc", "--offline", "--no-session", "--no-tools", "--no-skills", "--no-prompt-templates", "--model", "offline/offline"], {
				cwd: home,
				env: { PATH: process.env.PATH, HOME: home, MAHO_CODING_AGENT_DIR: rpcAgent },
				detached: true,
				stdin: "pipe",
				stdout: "pipe",
				stderr: "pipe",
			});
			const proc = rpcProc;
			const stdoutText = new Response(proc.stdout).text();
			const stderrText = new Response(proc.stderr).text();
			proc.stdin.write(`${JSON.stringify({ type: "prompt", id: "prompt_1", prompt: "registry probe" })}\n`);
			proc.stdin.end();
			deadline = setTimeout(() => {
				try {
					process.kill(-proc.pid, "SIGKILL");
				} catch {
					/* group already gone */
				}
			}, 30_000);
			const [text, code, stderr] = await Promise.all([stdoutText, proc.exited, stderrText]);
			clearTimeout(deadline);
			writeFileSync(join(ctx.qaRoot, "registry-rpc.log"), text + stderr);
			artifacts.push("registry-rpc.log");
			rpcExit = code;
			rpcPrompted = text.includes("prompt_1") && text.includes(REPLY);
		} finally {
			clearTimeout(deadline);
			if (proc) {
				try {
					process.kill(-proc.pid, "SIGKILL");
				} catch {
					/* group already gone */
				}
				await Promise.race([proc.exited, new Promise((r) => setTimeout(r, 3_000))]);
			}
			server.stop(true);
		}
		ok = providerListed && rpcPrompted && rpcExit === 0;
		detail = `providerListed=${providerListed} rpcPrompted=${rpcPrompted} rpcExit=${rpcExit}`;
	} finally {
		const groupGone = !rpcProc || processGroupGone(rpcProc.pid);
		rmSync(home, { recursive: true, force: true });
		cleanupOk = groupGone && !existsSync(home);
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
