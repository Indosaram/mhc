#!/usr/bin/env bun
// Real-surface QA driver for the REGISTERED memory projection pin and the `/recompile` refresh on
// the installed RPC host.
//
//   bun tools/latest-omo-memory-projection-qa.mjs --binary <staged mhc> --evidence <owned evidence> \
//        [--scenario bound-projection|unbound-boundary|all]
//
// This drives the SHIPPED surface - never a direct handler call and never fake dependencies:
//   * the real `mhc --mode rpc --multi-session --listen unix://<home>/host.sock --offline` host,
//     started with an isolated HOME + MAHO_CODING_AGENT_DIR + MAHO_MEMORY_HOME (the
//     `crates/maho-cli/tests/memory_recall_entry.rs` identity/environment seeding contract) and a
//     user `~/.maho/omo.jsonc` whose `memory` block enables the component and pins the agent;
//   * the real slash dispatch `crates/maho-core/src/agent_session.rs:1938` (`text.starts_with('/')`
//     -> `try_execute_extension_command`) -> `commands/recompile.rs` ->
//     `MemoryCommandDeps.bust_prompt_cache` -> `prompt.cache.clear()` + `prompt.pins.request_refresh()`;
//   * a scripted loopback `openai-completions` provider that RECORDS every request body, so the
//     projection is asserted from the exact bytes the model received.
//
// Required observations (contract `.omo/evidence/latest-omo-implementation-20261007/contracts/
// before-edit-memory-projection-qa.md`):
//   1. bound identity + committed system sentinel A: the FIRST ordinary prompt's provider request
//      carries A, and the session persists an `omo-memory:projection-pin` entry whose `revision` is
//      the A commit.
//   2. an externally committed sentinel B is NOT consumed: the next ordinary prompt still carries A
//      and not B, while the pin's `noticedThrough` advances to B and `revision` stays at A.
//   3. `/recompile` through the shipped RPC command path: the prompt is `handled` (NO provider turn),
//      the `command_invocation` event names recompile/extension/slash, the `extension_ui_request`
//      notify is the cache-cleared line, and the NEXT ordinary prompt carries B with a NEW persisted
//      pin whose `revision` is the B commit.
//   4. unbound identity boundary: `/recompile` on a session that lost its binding returns the
//      registered NOT_BOUND error notification and fabricates NO pin.
//
// Determinism: every trigger subscribes its exact event (the socket `agent_idle` record and the
// provider request body) BEFORE the action; there is no fixed sleep and no poll. Cleanup is the owned
// host teardown (SIGTERM with the socket removal subscribed BEFORE the signal, then a bounded force
// kill), a bounded stream reap, provider stop and temporary-home removal. A surface the binary does
// not reach records "blocked" with the exact missing contract - never a proxy pass.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, watch, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";

const SCHEMA = "latest-omo-memory-projection-qa/v1";
const READY_TIMEOUT_MS = 30_000;
const REQUEST_TIMEOUT_MS = 30_000;
const EVENT_TIMEOUT_MS = 30_000;
const DEATH_TIMEOUT_MS = 15_000;
const GRACE_TIMEOUT_MS = 5_000;
const REAP_TIMEOUT_MS = 5_000;

// Source-established entry types and markers (read from the ported modules, never invented).
const PROJECTION_PIN_TYPE = "omo-memory:projection-pin"; // projection_pin.rs PROJECTION_PIN_ENTRY_TYPE
const BINDING_TYPE = "senpi-memory.session-binding"; // binding.rs MEMORY_BINDING_CUSTOM_TYPE
const CACHE_CLEARED_MARKER = "memory prompt cache cleared"; // commands/recompile.rs response text
const NOT_BOUND_MARKER = "not bound"; // commands/types.rs NOT_BOUND_ERROR
const MEMORY_BLOCK_OPEN = "<!-- senpi-memory:"; // compile::render::mark_memory_block
const IDENTITY = "qa-projection"; // memory.agent (deterministic <slug>-<sha256-8> id)
const CONFLICT_IDENTITY = "qa-projection-conflict"; // a DIFFERENT identity for the unbound boundary
const OFFLINE_PROVIDER = "offline";
const SESSION_MODEL = { provider: OFFLINE_PROVIDER, id: "offline" };
const SENTINEL_A = "QA_PROJECTION_SENTINEL_A_9f31c0";
const SENTINEL_B = "QA_PROJECTION_SENTINEL_B_9f31c0";
const GIT_NAME = "QA Projection Fixture";
const GIT_EMAIL = "qa@example.invalid";
const PROMPTS = {
	first: { marker: "qa-projection-turn-1", text: "Reply with the single word ok. (qa-projection-turn-1)" },
	second: { marker: "qa-projection-turn-2", text: "Reply with the single word ok. (qa-projection-turn-2)" },
	third: { marker: "qa-projection-turn-3", text: "Reply with the single word ok. (qa-projection-turn-3)" },
	unbound: { marker: "qa-projection-turn-unbound", text: "Reply with the single word ok. (qa-projection-turn-unbound)" },
};

const delay = (ms) => new Promise((resolvePromise) => setTimeout(resolvePromise, ms));

function usage(message) {
	console.error(`latest-omo-memory-projection-qa: ${message}`);
	console.error("usage: bun tools/latest-omo-memory-projection-qa.mjs --binary <mhc> --evidence <E> [--scenario bound-projection|unbound-boundary|all]");
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

// ---------------------------------------------------------------------------------------------
// identity resolution (pure port of memory-core identity/resolve.rs, ASCII inputs only)
// ---------------------------------------------------------------------------------------------

/** `short_hash`: the first 8 hex characters of sha256(input). */
function shortHash(input) {
	return sha256(Buffer.from(input, "utf8")).slice(0, 8);
}

/** `sanitize_to_slug`: lowercase ASCII, dash-separated, capped at 40, trimmed, `agent` fallback. */
function sanitizeToSlug(input) {
	let folded = "";
	for (const character of input) {
		const code = character.codePointAt(0);
		if (code >= 0x300 && code <= 0x36f) continue; // drop combining diacritics
		folded += character;
	}
	let dashed = "";
	for (const character of folded.toLowerCase()) dashed += /[a-z0-9]/.test(character) ? character : "-";
	const capped = dashed.replace(/-+/g, "-").slice(0, 40);
	const trimmed = capped.replace(/-+$/, "");
	return trimmed.length > 0 ? trimmed : "agent";
}

/** The resolved memory identity id for a non-empty, non-`auto` `memory.agent` value. */
function identityId(agent) {
	const trimmed = agent.trim();
	return `${sanitizeToSlug(trimmed)}-${shortHash(trimmed)}`;
}

// ---------------------------------------------------------------------------------------------
// isolated project / agent configuration (memory_recall_entry.rs seeding contract)
// ---------------------------------------------------------------------------------------------

/** `<HOME>/.maho/omo.jsonc`: the ONE user-layer config the loader reads (`loader/paths.rs`). */
function writeUserConfig(home, memory) {
	const directory = join(home, ".maho");
	mkdirSync(directory, { recursive: true });
	const path = join(directory, "omo.jsonc");
	writeFileSync(path, JSON.stringify({ memory }, null, 2) + "\n");
	return path;
}

/** `<agent>/models.json` + `<agent>/settings.json`: the session model pinned WITHOUT a CLI flag. */
function writeAgentConfig(agent, port) {
	writeFileSync(
		join(agent, "models.json"),
		JSON.stringify({ providers: { [OFFLINE_PROVIDER]: {
			api: "openai-completions",
			baseUrl: `http://127.0.0.1:${port}/v1`,
			apiKey: "offline-fixture",
			models: [{ id: "offline", reasoning: false, input: ["text"], contextWindow: 128000, maxTokens: 4096 }],
		} } }, null, 2) + "\n",
	);
	writeFileSync(join(agent, "settings.json"), JSON.stringify({ defaultProvider: SESSION_MODEL.provider, defaultModel: SESSION_MODEL.id }, null, 2) + "\n");
}

/** The fixture git identity, so every driver git read/write ignores the developer's global config. */
function gitEnv(home) {
	return {
		PATH: process.env.PATH,
		HOME: home,
		GIT_CONFIG_GLOBAL: "/dev/null",
		GIT_CONFIG_SYSTEM: "/dev/null",
		GIT_AUTHOR_NAME: GIT_NAME,
		GIT_AUTHOR_EMAIL: GIT_EMAIL,
		GIT_COMMITTER_NAME: GIT_NAME,
		GIT_COMMITTER_EMAIL: GIT_EMAIL,
	};
}

async function runGit(args, cwd, env) {
	const process_ = Bun.spawn(["git", ...args], { cwd, env, stdin: "ignore", stdout: "pipe", stderr: "pipe" });
	const [out, err, code] = await Promise.all([
		new Response(process_.stdout).text(),
		new Response(process_.stderr).text(),
		process_.exited,
	]);
	return { code, out: out.trim(), err: err.trim() };
}

async function headOf(repoDir, home) {
	return (await runGit(["rev-parse", "HEAD"], repoDir, gitEnv(home))).out;
}

/** `git init` + ONE committed system memory file, so the component adopts a repo that already has A. */
async function initRepoWithSentinel(repoDir, home, relativePath, body, message) {
	mkdirSync(repoDir, { recursive: true });
	const env = gitEnv(home);
	const init = await runGit(["init", "-q", "-b", "main"], repoDir, env);
	const full = join(repoDir, relativePath);
	mkdirSync(dirname(full), { recursive: true });
	writeFileSync(full, `---\ndescription: ${relativePath}\n---\n${body}\n`);
	await runGit(["add", "-A"], repoDir, env);
	const committed = await runGit(["commit", "-q", "-m", message], repoDir, env);
	return { init, committed, head: await headOf(repoDir, home) };
}

/** Commit an EXTERNAL change (another writer) into the memory repo, then report the new HEAD. */
async function commitExternal(repoDir, home, relativePath, body, message) {
	const env = gitEnv(home);
	const full = join(repoDir, relativePath);
	mkdirSync(dirname(full), { recursive: true });
	writeFileSync(full, `---\ndescription: ${relativePath}\n---\n${body}\n`);
	await runGit(["add", "-A"], repoDir, env);
	const committed = await runGit(["commit", "-q", "-m", message], repoDir, env);
	return { committed, head: await headOf(repoDir, home) };
}

// ---------------------------------------------------------------------------------------------
// the scripted loopback provider (records every request body; content-addressed waiters)
// ---------------------------------------------------------------------------------------------

function sse(objects) {
	return objects.map((object) => `data: ${JSON.stringify(object)}\n\n`).join("") + "data: [DONE]\n\n";
}

function textEvents(text) {
	return sse([{ id: "offline", object: "chat.completion.chunk", created: 0, model: "offline", choices: [{ index: 0, delta: { role: "assistant", content: text }, finish_reason: "stop" }] }]);
}

/**
 * Answers EVERY request with the same text completion and records its body. `waitFor(predicate)` is
 * registered BEFORE the trigger, so a caller can await the EXACT request its prompt produced - an
 * unsolicited startup turn (or any other turn) can neither be masked nor desynchronize the script.
 */
function scriptedProvider() {
	const state = { requests: [], waiters: [] };
	const server = Bun.serve({
		port: 0,
		async fetch(request) {
			const body = await request.text().catch(() => "");
			state.requests.push(body);
			for (const waiter of [...state.waiters]) {
				if (!waiter.predicate(body)) continue;
				state.waiters.splice(state.waiters.indexOf(waiter), 1);
				clearTimeout(waiter.timer);
				waiter.resolve(body);
			}
			return new Response(textEvents("ok"), { headers: { "content-type": "text/event-stream" } });
		},
	});
	const waitFor = (predicate, ms = EVENT_TIMEOUT_MS) =>
		new Promise((resolvePromise) => {
			const waiter = { predicate, resolve: resolvePromise };
			waiter.timer = setTimeout(() => {
				const index = state.waiters.indexOf(waiter);
				if (index >= 0) state.waiters.splice(index, 1);
				resolvePromise(null);
			}, ms);
			state.waiters.push(waiter);
		});
	return { server, port: server.port, state, waitFor };
}

// ---------------------------------------------------------------------------------------------
// filesystem subscriptions (readiness / teardown) - never a poll
// ---------------------------------------------------------------------------------------------

function waitForPathAppear(path, timeoutMs) {
	return new Promise((resolvePromise) => {
		if (existsSync(path)) return resolvePromise(true);
		mkdirSync(dirname(path), { recursive: true });
		const watcher = watch(dirname(path), () => {
			if (existsSync(path)) {
				watcher.close();
				clearTimeout(timer);
				resolvePromise(true);
			}
		});
		const timer = setTimeout(() => {
			watcher.close();
			resolvePromise(existsSync(path));
		}, timeoutMs);
	});
}

function waitForPathGone(path, timeoutMs) {
	return new Promise((resolvePromise) => {
		if (!existsSync(path)) return resolvePromise(true);
		const watcher = watch(dirname(path), () => {
			if (!existsSync(path)) {
				watcher.close();
				clearTimeout(timer);
				resolvePromise(true);
			}
		});
		const timer = setTimeout(() => {
			watcher.close();
			resolvePromise(!existsSync(path));
		}, timeoutMs);
	});
}

/** Start consuming a pipe immediately, so a chatty host can never block on a full pipe. */
function drain(stream) {
	return new Response(stream).text().catch(() => null);
}

// ---------------------------------------------------------------------------------------------
// the JSONL RPC client: responses keyed by id, every other record kept as an event
// ---------------------------------------------------------------------------------------------

async function connectHost(socketPath) {
	const client = { buffer: "", waiters: new Map(), nextId: 1, socket: null, events: [], eventWaiters: [] };
	client.socket = await Bun.connect({
		unix: socketPath,
		socket: {
			data(_socket, chunk) {
				client.buffer += chunk.toString();
				let index;
				while ((index = client.buffer.indexOf("\n")) >= 0) {
					const line = client.buffer.slice(0, index);
					client.buffer = client.buffer.slice(index + 1);
					let message;
					try {
						message = JSON.parse(line);
					} catch {
						continue;
					}
					if (!message || typeof message !== "object") continue;
					if (typeof message.id === "string" && client.waiters.has(message.id)) {
						const settle = client.waiters.get(message.id);
						client.waiters.delete(message.id);
						settle(message);
						continue;
					}
					if (message.type === "response") continue;
					client.events.push(message);
					for (const waiter of [...client.eventWaiters]) {
						if (!waiter.predicate(message)) continue;
						client.eventWaiters.splice(client.eventWaiters.indexOf(waiter), 1);
						clearTimeout(waiter.timer);
						waiter.resolve(message);
					}
				}
			},
			close() {
				for (const settle of client.waiters.values()) settle(null);
				client.waiters.clear();
				for (const waiter of client.eventWaiters) {
					clearTimeout(waiter.timer);
					waiter.resolve(null);
				}
				client.eventWaiters.length = 0;
			},
			error() {},
		},
	});
	client.request = (fields, ms = REQUEST_TIMEOUT_MS) =>
		new Promise((resolvePromise) => {
			const id = String(client.nextId++);
			const timer = setTimeout(() => {
				client.waiters.delete(id);
				resolvePromise(null);
			}, ms);
			client.waiters.set(id, (message) => {
				clearTimeout(timer);
				resolvePromise(message);
			});
			client.socket.write(JSON.stringify({ id, ...fields }) + "\n");
		});
	// Subscribe BEFORE the trigger: only records that arrive after registration settle this waiter.
	client.waitForEvent = (predicate, ms = EVENT_TIMEOUT_MS) =>
		new Promise((resolvePromise) => {
			const waiter = { predicate, resolve: resolvePromise };
			waiter.timer = setTimeout(() => {
				const index = client.eventWaiters.indexOf(waiter);
				if (index >= 0) client.eventWaiters.splice(index, 1);
				resolvePromise(null);
			}, ms);
			client.eventWaiters.push(waiter);
		});
	client.close = () => {
		try {
			client.socket.end();
		} catch {
			/* already closed */
		}
	};
	return client;
}

// ---------------------------------------------------------------------------------------------
// host lifecycle
// ---------------------------------------------------------------------------------------------

/** Start the real multi-session host over an isolated home; return the child and the socket path. */
async function startHost(ctx, home, socket, provider) {
	const agent = join(home, "agent");
	const env = {
		PATH: process.env.PATH,
		HOME: home,
		MAHO_CODING_AGENT_DIR: agent,
		MAHO_MEMORY_HOME: join(home, "memory"),
		OMO_SENPI_SKILLS_ROOT: join(home, "skills"),
	};
	// Subscribe BEFORE the spawn: readiness is the socket appearing, not a fixed-interval poll.
	const appeared = waitForPathAppear(socket, READY_TIMEOUT_MS);
	const host = Bun.spawn(
		[ctx.binary, "--mode", "rpc", "--multi-session", "--listen", `unix://${socket}`, "--offline"],
		{ cwd: home, env, detached: true, stdin: "ignore", stdout: "pipe", stderr: "pipe" },
	);
	const stdoutText = drain(host.stdout);
	const stderrText = drain(host.stderr);
	return { host, socket, provider, appeared, stdoutText, stderrText };
}

/**
 * Bounded cleanup of the OWNED host: subscribe the socket removal BEFORE the trigger, SIGTERM the
 * process the driver owns (SIGKILL cannot unlink the socket), await its own `exited` promise, and
 * only then force-kill under a bound. Both outcomes are reported.
 */
async function teardown(receipt, home, host, provider, socket, stdoutText, stderrText) {
	const socketGone = waitForPathGone(socket, DEATH_TIMEOUT_MS);
	const exited = host.exited.then(() => true).catch(() => true);
	try {
		process.kill(host.pid, "SIGTERM");
	} catch {
		/* already gone */
	}
	receipt.cleanup.gracefulExit = await Promise.race([exited, delay(GRACE_TIMEOUT_MS).then(() => false)]);
	if (!receipt.cleanup.gracefulExit) {
		try {
			process.kill(host.pid, "SIGKILL");
		} catch {
			/* already gone */
		}
		receipt.cleanup.forcedKill = true;
		receipt.cleanup.forcedExit = await Promise.race([exited, delay(REAP_TIMEOUT_MS).then(() => false)]);
	}
	receipt.cleanup.socketGone = await socketGone;
	const [stdout, stderr] = await Promise.all([
		Promise.race([stdoutText, delay(REAP_TIMEOUT_MS).then(() => null)]),
		Promise.race([stderrText, delay(REAP_TIMEOUT_MS).then(() => null)]),
	]);
	receipt.cleanup.streamsComplete = stdout !== null && stderr !== null;
	receipt.stdoutBytes = stdout === null ? null : stdout.length;
	receipt.stderr = stderr === null ? null : stderr.trim();
	provider.server.stop(true);
	receipt.cleanup.providerStopped = true;
	rmSync(home, { recursive: true, force: true });
	receipt.cleanup.homeRemoved = !existsSync(home);
}

function cleanupOk(receipt) {
	const cleanup = receipt.cleanup;
	return cleanup.gracefulExit === true && cleanup.socketGone === true && cleanup.streamsComplete === true && cleanup.providerStopped === true && cleanup.homeRemoved === true;
}

// ---------------------------------------------------------------------------------------------
// observables
// ---------------------------------------------------------------------------------------------

/** The persisted projection-pin records, newest last (`projection_pin.rs` ProjectionPinRecord). */
function pinRecords(entriesResponse) {
	return (entriesResponse?.data?.entries ?? [])
		.filter((entry) => entry?.customType === PROJECTION_PIN_TYPE)
		.map((entry) => entry?.data ?? null);
}

/** The persisted memory-binding entries (`binding.rs`). */
function bindingRecords(entriesResponse) {
	return (entriesResponse?.data?.entries ?? []).filter((entry) => entry?.customType === BINDING_TYPE).map((entry) => entry?.data ?? null);
}

/** Every `extension_ui_request`/`method:notify` the host pushed to this connection. */
function notifyRecords(client) {
	return client.events
		.filter((event) => event?.type === "extension_ui_request" && event?.method === "notify")
		.map((event) => ({ message: typeof event.message === "string" ? event.message : "", notifyType: event.notifyType ?? null }));
}

function commandInvocations(client) {
	return client.events.filter((event) => event?.type === "command_invocation").map((event) => event.command);
}

function sentinelState(body) {
	const text = typeof body === "string" ? body : "";
	return { bytes: text.length, containsA: text.includes(SENTINEL_A), containsB: text.includes(SENTINEL_B) };
}

/** Same revision under a full or abbreviated sha. */
function sameRevision(left, right) {
	return typeof left === "string" && typeof right === "string" && left.length > 0 && right.length > 0 && (left === right || left.startsWith(right) || right.startsWith(left));
}

// ---------------------------------------------------------------------------------------------
// prompt helpers (exact-event synchronization: subscribe BEFORE the trigger)
// ---------------------------------------------------------------------------------------------

/** An ordinary prompt: subscribe the settle + the provider request, send, then await both. */
async function promptAndSettle(client, provider, sessionId, prompt) {
	const settled = client.waitForEvent((event) => event?.type === "agent_idle");
	const served = provider.waitFor((body) => body.includes(prompt.marker));
	const response = await client.request({ type: "prompt", sessionId, message: prompt.text, sessionTitlePrompt: false });
	const idle = await settled;
	const body = await served;
	return {
		marker: prompt.marker,
		success: response?.success === true,
		disposition: response?.data?.disposition ?? null,
		settled: idle !== null,
		servedBody: body,
		served: body !== null,
		provider: sentinelState(body),
	};
}

/**
 * A registered slash command through the shipped RPC command path. It must NOT produce a provider
 * turn: subscribe the invocation, the marker notify and the provider baseline, send, then await.
 */
async function runSlashCommand(client, provider, sessionId, { name, message, notifyMarker }) {
	const invocation = client.waitForEvent((event) => event?.type === "command_invocation" && event?.command?.name === name);
	const notify = client.waitForEvent(
		(event) => event?.type === "extension_ui_request" && event?.method === "notify" && typeof event.message === "string" && event.message.includes(notifyMarker),
	);
	const servedBefore = provider.state.requests.length;
	const response = await client.request({ type: "prompt", sessionId, message, sessionTitlePrompt: false });
	const invocationEvent = await invocation;
	const notifyEvent = await notify;
	return {
		success: response?.success === true,
		disposition: response?.data?.disposition ?? null,
		invocation: invocationEvent?.command ?? null,
		notify: notifyEvent ? { message: notifyEvent.message, notifyType: notifyEvent.notifyType } : null,
		providerServedDelta: provider.state.requests.length - servedBefore,
	};
}

// ---------------------------------------------------------------------------------------------
// scenario plumbing
// ---------------------------------------------------------------------------------------------

/** Shared setup: isolated home, user config, agent config, git identity, provider, host, session. */
async function openScenario(ctx, { receiptName, memory, repoSeed, projectConfig }) {
	const home = mkdtempSync(join(tmpdir(), "memory-projection-qa-"));
	const agent = join(home, "agent");
	const project = join(home, "project");
	const socket = join(home, "host.sock");
	const receipt = {
		home,
		project,
		socket,
		configPath: null,
		identity: IDENTITY,
		identityId: identityId(IDENTITY),
		repoDir: join(home, "memory", "agents", identityId(IDENTITY), "repo"),
		commits: {},
		cleanup: {},
	};
	mkdirSync(agent, { recursive: true });
	mkdirSync(project, { recursive: true });
	mkdirSync(join(home, "skills"), { recursive: true });
	// The host onboarding gate, matching the component driver.
	const onboardingState = join(home, ".maho", "agent", "omo-senpi", "omo-native");
	mkdirSync(onboardingState, { recursive: true });
	writeFileSync(join(onboardingState, "onboarding-completed"), JSON.stringify({ completedAt: "2026-10-06T00:00:00Z", version: 1 }));
	// A `git` identity local to the fixture HOME, so no developer global/system config leaks in.
	writeFileSync(join(home, ".gitconfig"), `[user]\n\tname = ${GIT_NAME}\n\temail = ${GIT_EMAIL}\n`);
	receipt.configPath = writeUserConfig(home, memory);
	if (projectConfig) {
		const directory = join(project, ".omo");
		mkdirSync(directory, { recursive: true });
		writeFileSync(join(directory, "omo.json"), JSON.stringify(projectConfig, null, 2) + "\n");
	}
	if (repoSeed) receipt.commits.A = await initRepoWithSentinel(receipt.repoDir, home, repoSeed.path, repoSeed.body, repoSeed.message);

	const provider = scriptedProvider();
	writeAgentConfig(agent, provider.port);
	const started = await startHost(ctx, home, socket, provider);
	const state = { receipt, client: null, started, home, provider, failure: null };
	try {
		const ready = await started.appeared;
		if (!ready) throw new Error("the host never bound its socket");
		state.client = await connectHost(socket);
		const opened = await state.client.request({ type: "open_session", cwd: project });
		receipt.openResponse = opened;
		receipt.sessionId = opened?.data?.sessionId ?? null;
		receipt.openState = opened?.data?.state ?? null;
		if (!receipt.sessionId) throw new Error("the host answered no sessionId to open_session");
		receipt.commandsResponse = await state.client.request({ type: "get_commands", sessionId: receipt.sessionId });
		receipt.commits.headAtOpen = await headOf(receipt.repoDir, home);
	} catch (error) {
		state.failure = error.message;
		if (state.client) state.client.close();
		await teardown(receipt, home, started.host, provider, socket, started.stdoutText, started.stderrText);
		writeFileSync(join(ctx.qaRoot, receiptName), JSON.stringify(receipt, null, 2) + "\n");
	}
	return state;
}

async function readEntries(client, sessionId) {
	const response = await client.request({ type: "get_entries", sessionId });
	return response;
}

/** Teardown, write the receipt, then let `verdict(receipt)` decide pass/blocked from the final state. */
async function closeScenario(ctx, state, receiptName, verdict) {
	const { receipt, client, started, home, provider } = state;
	if (client) client.close();
	await teardown(receipt, home, started.host, provider, receipt.socket, started.stdoutText, started.stderrText);
	const { ok, detail } = verdict(receipt);
	writeFileSync(join(ctx.qaRoot, receiptName), JSON.stringify(receipt, null, 2) + "\n");
	const cleanup_ok = cleanupOk(receipt);
	return {
		status: ok && cleanup_ok ? "pass" : "blocked",
		blocker: ok && cleanup_ok ? null : `${detail} cleanup=${JSON.stringify(receipt.cleanup)}`,
		artifacts: [receiptName],
		cleanup_ok,
	};
}

function blockedResult(state, scenario) {
	return { status: "blocked", blocker: `${scenario}: ${state.failure}`, artifacts: [], cleanup_ok: cleanupOk(state.receipt) };
}

// ---------------------------------------------------------------------------------------------
// scenario: bound projection pin + /recompile refresh
// ---------------------------------------------------------------------------------------------

async function scenarioBoundProjection(ctx) {
	const state = await openScenario(ctx, {
		receiptName: "bound-projection-receipt.json",
		memory: { enabled: true, agent: IDENTITY },
		repoSeed: { path: "system/persona.md", body: SENTINEL_A, message: "qa: seed system sentinel A" },
	});
	if (state.failure) return blockedResult(state, "bound-projection");
	const { receipt, client, provider } = state;
	const id = receipt.identityId;
	const blockOpen = `${MEMORY_BLOCK_OPEN}${id}:begin -->`;

	// 1. bound identity + committed sentinel A on the FIRST ordinary prompt.
	const first = await promptAndSettle(client, provider, receipt.sessionId, PROMPTS.first);
	receipt.first = first;
	const entriesAfterFirst = await readEntries(client, receipt.sessionId);
	receipt.entriesAfterFirst = entriesAfterFirst;
	receipt.pinsAfterFirst = pinRecords(entriesAfterFirst);
	receipt.bindingAfterFirst = bindingRecords(entriesAfterFirst);
	receipt.blockOpen = blockOpen;
	const firstPin = receipt.pinsAfterFirst.at(-1) ?? null;
	receipt.firstPin = firstPin;

	// 2. an EXTERNAL sentinel B must not change the pinned bytes.
	const external = await commitExternal(receipt.repoDir, receipt.home, "system/human.md", SENTINEL_B, "qa: external sentinel B");
	receipt.commits.B = external;
	const second = await promptAndSettle(client, provider, receipt.sessionId, PROMPTS.second);
	receipt.second = second;
	const entriesAfterSecond = await readEntries(client, receipt.sessionId);
	receipt.pinsAfterSecond = pinRecords(entriesAfterSecond);
	const secondPin = receipt.pinsAfterSecond.at(-1) ?? null;
	receipt.secondPin = secondPin;

	// 3. `/recompile` through the shipped RPC command path, then B on the next ordinary prompt.
	const recompile = await runSlashCommand(client, provider, receipt.sessionId, { name: "recompile", message: "/recompile", notifyMarker: CACHE_CLEARED_MARKER });
	receipt.recompile = recompile;
	receipt.commandInvocations = commandInvocations(client);
	const third = await promptAndSettle(client, provider, receipt.sessionId, PROMPTS.third);
	receipt.third = third;
	const entriesAfterThird = await readEntries(client, receipt.sessionId);
	receipt.pinsAfterThird = pinRecords(entriesAfterThird);
	const thirdPin = receipt.pinsAfterThird.at(-1) ?? null;
	receipt.thirdPin = thirdPin;

	return closeScenario(ctx, state, "bound-projection-receipt.json", (final) => {
		const headA = final.commits?.headAtOpen ?? null;
		const headB = final.commits?.B?.head ?? null;
		const checks = {
			firstAdmitted: final.first?.success === true && final.first?.disposition === "started" && final.first?.settled === true,
			firstCarriedA: final.first?.provider?.containsA === true && final.first?.provider?.containsB === false,
			firstHadBlock: typeof final.first?.servedBody === "string" && final.first.servedBody.includes(final.blockOpen),
			firstPinAtA: Boolean(final.firstPin) && sameRevision(final.firstPin?.revision, final.commits.headAtOpen) && final.firstPin?.sessionId === final.openState?.sessionId,
			bound: (final.bindingAfterFirst ?? []).some((binding) => binding?.identity === final.identityId),
			secondAdmitted: final.second?.success === true && final.second?.disposition === "started" && final.second?.settled === true,
			secondKeptA: final.second?.provider?.containsA === true && final.second?.provider?.containsB === false,
			secondPinNoticedB: Boolean(final.secondPin) && sameRevision(final.secondPin?.revision, headA) && sameRevision(final.secondPin?.noticedThrough, headB),
			recompileHandled: final.recompile?.success === true && final.recompile?.disposition === "handled" && final.recompile?.providerServedDelta === 0,
			recompileInvoked: final.recompile?.invocation?.name === "recompile" && final.recompile?.invocation?.source === "extension" && final.recompile?.invocation?.syntax === "slash",
			recompileNotified: typeof final.recompile?.notify?.message === "string" && final.recompile.notify.message.includes(CACHE_CLEARED_MARKER) && final.recompile.notify.notifyType === "info",
			thirdAdmitted: final.third?.success === true && final.third?.disposition === "started" && final.third?.settled === true,
			thirdCarriedB: final.third?.provider?.containsB === true,
			thirdPinAtB: Boolean(final.thirdPin) && sameRevision(final.thirdPin?.revision, headB),
		};
		final.checks = checks;
		const failed = Object.entries(checks).filter(([, value]) => value !== true).map(([key]) => key);
		const ok = failed.length === 0;
		const detail = ok
			? `bound-projection: A pinned then held across an external B, /recompile handled and repinned to B (A=${headA?.slice(0, 7)} B=${headB?.slice(0, 7)})`
			: `bound-projection: failed=${failed.join(",")} first=${JSON.stringify(final.first?.provider)} second=${JSON.stringify(final.second?.provider)} third=${JSON.stringify(final.third?.provider)} firstPin=${JSON.stringify(final.firstPin)} secondPin=${JSON.stringify(final.secondPin)} thirdPin=${JSON.stringify(final.thirdPin)} recompile=${JSON.stringify(final.recompile)} invocation=${JSON.stringify(final.commandInvocations)}`;
		return { ok, detail };
	});
}

// ---------------------------------------------------------------------------------------------
// scenario: unbound identity command boundary
// ---------------------------------------------------------------------------------------------

async function scenarioUnboundBoundary(ctx) {
	// Memory is ENABLED (so `/recompile` is registered), but the session is bound to a DIFFERENT
	// identity than config resolves: `index.rs` SessionStart refuses the conflict, leaving the session
	// with `context == None` -> `identity()` returns None -> `require_identity` reports NOT_BOUND.
	const state = await openScenario(ctx, {
		receiptName: "unbound-boundary-receipt.json",
		memory: { enabled: true, agent: IDENTITY },
		repoSeed: { path: "system/persona.md", body: SENTINEL_A, message: "qa: seed system sentinel A" },
	});
	if (state.failure) return blockedResult(state, "unbound-boundary");
	const { receipt, client, provider } = state;

	receipt.bindingBeforeConflict = bindingRecords(await readEntries(client, receipt.sessionId));
	// Append a persisted binding entry for a DIFFERENT identity, then reload so SessionStart re-reads it.
	const conflictEntry = {
		type: "custom",
		id: "qa-projection-conflict-binding",
		parentId: null,
		timestamp: "2026-10-06T00:00:00.000Z",
		customType: BINDING_TYPE,
		data: { identity: CONFLICT_IDENTITY, repoPathHash: "qa-conflict", boundAt: 0 },
	};
	const appended = await client.request({ type: "append_session_entry", sessionId: receipt.sessionId, entry: conflictEntry });
	receipt.appendResponse = appended;
	const reload = await client.request({ type: "reload", sessionId: receipt.sessionId });
	receipt.reloadResponse = reload;
	const entriesAfterReload = await readEntries(client, receipt.sessionId);
	receipt.bindingAfterConflict = bindingRecords(entriesAfterReload);
	receipt.pinsBeforeCommand = pinRecords(entriesAfterReload);

	// `/recompile` on the unbound session: handled disposition, NOT_BOUND error notification, no pin.
	const recompile = await runSlashCommand(client, provider, receipt.sessionId, { name: "recompile", message: "/recompile", notifyMarker: NOT_BOUND_MARKER });
	receipt.recompile = recompile;
	receipt.commandInvocations = commandInvocations(client);
	receipt.notifies = notifyRecords(client);
	const entriesAfterCommand = await readEntries(client, receipt.sessionId);
	receipt.pinsAfterCommand = pinRecords(entriesAfterCommand);

	// The unbound session must not accept a projection either: an ordinary prompt carries NO block.
	const unbound = await promptAndSettle(client, provider, receipt.sessionId, PROMPTS.unbound);
	receipt.unbound = unbound;
	receipt.pinsAfterUnboundPrompt = pinRecords(await readEntries(client, receipt.sessionId));

	return closeScenario(ctx, state, "unbound-boundary-receipt.json", (final) => {
		const checks = {
			conflictAppendAccepted: final.appendResponse?.success === true,
			reloadAccepted: final.reloadResponse?.success === true,
			conflictPersisted: (final.bindingAfterConflict ?? []).some((binding) => binding?.identity === CONFLICT_IDENTITY),
			recompileHandled: final.recompile?.success === true && final.recompile?.disposition === "handled" && final.recompile?.providerServedDelta === 0,
			recompileInvoked: final.recompile?.invocation?.name === "recompile" && final.recompile?.invocation?.source === "extension",
			notBoundNotified: typeof final.recompile?.notify?.message === "string" && final.recompile.notify.message.includes(NOT_BOUND_MARKER) && final.recompile.notify.notifyType === "error",
			noPinFabricated: (final.pinsAfterCommand ?? []).length === 0 && (final.pinsBeforeCommand ?? []).length === 0,
			unboundPromptAdmitted: final.unbound?.success === true && final.unbound?.disposition === "started" && final.unbound?.settled === true,
			noPinAfterUnboundPrompt: (final.pinsAfterUnboundPrompt ?? []).length === 0,
			noProjectionAccepted: typeof final.unbound?.servedBody === "string" && !final.unbound.servedBody.includes(MEMORY_BLOCK_OPEN),
		};
		final.checks = checks;
		const failed = Object.entries(checks).filter(([, value]) => value !== true).map(([key]) => key);
		const ok = failed.length === 0;
		const detail = ok
			? `unbound-boundary: the conflicting binding left the session unbound; /recompile returned the registered NOT_BOUND error and fabricated no pin`
			: `unbound-boundary: failed=${failed.join(",")} recompile=${JSON.stringify(final.recompile)} bindings=${JSON.stringify(final.bindingAfterConflict)} pins=${JSON.stringify(final.pinsAfterCommand)} unboundBodyBytes=${final.unbound?.provider?.bytes ?? null}`;
		return { ok, detail };
	});
}

// ---------------------------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------------------------

const SCENARIOS = {
	"bound-projection": scenarioBoundProjection,
	"unbound-boundary": scenarioUnboundBoundary,
};

async function main() {
	const args = parseArgs(process.argv.slice(2));
	const ctx = { binary: resolve(args.binary), qaRoot: join(resolve(args.evidence), "qa") };
	mkdirSync(ctx.qaRoot, { recursive: true });

	const binaryBefore = existsSync(ctx.binary) ? { sha256: sha256(readFileSync(ctx.binary)), mtimeMs: statSync(ctx.binary).mtimeMs } : null;
	if (!binaryBefore) console.error(`latest-omo-memory-projection-qa: --binary ${ctx.binary} is missing; every scenario will be blocked`);

	const names = args.scenario === "all" ? Object.keys(SCENARIOS) : [args.scenario];
	for (const name of names) if (!SCENARIOS[name]) usage(`unknown scenario ${name}`);

	const scenarios = {};
	let anyBlocked = false;
	for (const name of names) {
		console.log(`latest-omo-memory-projection-qa: scenario ${name}`);
		let result;
		try {
			result = await SCENARIOS[name](ctx);
		} catch (error) {
			result = { status: "blocked", blocker: `${name}: driver error ${error.message}`, artifacts: [], cleanup_ok: false };
		}
		if (result.status !== "pass") anyBlocked = true;
		scenarios[name] = { status: result.status, blocker: result.blocker ?? null, artifacts: result.artifacts ?? [], cleanup_ok: result.cleanup_ok ?? true };
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
	writeFileSync(join(ctx.qaRoot, "latest-omo-memory-projection-qa.json"), JSON.stringify(report, null, 2) + "\n");
	console.log(`latest-omo-memory-projection-qa: ${report.status} -> ${join(ctx.qaRoot, "latest-omo-memory-projection-qa.json")}`);
	process.exit(report.status === "pass" ? 0 : 1);
}

await main();
