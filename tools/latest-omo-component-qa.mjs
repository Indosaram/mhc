#!/usr/bin/env bun
// Real-surface QA driver for the two registered OMO component hooks the installed-invocation
// drivers do NOT cover: the `model-profile` SessionStart hook and the `git-master` ToolResult hook.
// `tools/latest-omo-skill-qa.mjs` proves the RPC command surface (packaged skills) only; this driver
// exercises the components' OWN event handlers on the real multi-session RPC host.
//
//   bun tools/latest-omo-component-qa.mjs --binary <built mhc> --skills <staged skills root> \
//        --evidence <E> [--scenario profile-applied|profile-unknown|profile-invalid-auth|profile-scoped-boundary|git-master|all]
//
// The host is the REAL `mhc --mode rpc --multi-session --listen unix://<home>/host.sock --offline`,
// started with an isolated HOME + MAHO_CODING_AGENT_DIR and OMO_SENPI_SKILLS_ROOT=<skills>. The host
// CLI is started WITHOUT --provider/--model on purpose: the multi-session factory folds
// `config.provider/model` into the session launch profile and marks that origin "cli", which is
// exactly the provenance the profile gate treats as explicit user state. The session model is pinned
// deterministically through the agent `settings.json` (defaultProvider/defaultModel) instead.
//
// Scenarios (invocation, observables and cleanup recorded in
// `.omo/evidence/latest-omo-component-qa/before-edit-scenarios.md`, authored BEFORE this file):
//
//   profile-applied   <project>/.omo/omo.json carries a CUSTOM `model_profile` whose chain resolves
//                     to a model the local provider serves, with a `reasoning` token.
//     observable      a session entry with `customType == "omo-model-profile:applied"` whose
//                     `details` carry {profile, model, reasoning}, AND the session's effective
//                     model/thinking level equal to that profile's model/reasoning (the session-only
//                     startup actually applied it). The state is read from `get_state` when the host
//                     answers it, else from the `open_session` state projection (same builder).
//     unmet           no applied entry within the bound -> blocked with the observed model/level and
//                     the observed session-start gate state.
//
//   profile-unknown   `model_profile` names a profile that does not exist.
//     observable      a session entry with `customType == "omo-model-profile:unknown"` naming the
//                     unknown id, AND the effective model UNCHANGED (an unknown profile must not
//                     move the session model).
//
//   profile-invalid-auth   a custom profile whose only rung is a provider whose `apiKey` is a
//                     failing `!command` embedding a sentinel.
//     observable      the captured notice is the auth-failure shape (authFailed names that provider)
//                     AND the sentinel string appears in NEITHER the notice content/details NOR the
//                     host stderr. Determinism: a non-zero `!command` is a `ModelsError`, so the
//                     failure is deterministic; if the profile hook never runs the scenario is
//                     blocked rather than asserting absence vacuously.
//
//   git-master        a scripted provider drives three REAL `read` tool calls (the staged
//                     `<skills>/git-master/SKILL.md`, another file, and an absent git-master path)
//                     and the driver inspects the ACTUAL tool-result messages.
//     observable      exactly ONE toolResult text carries `<commit_attribution>` and the custom
//                     `1. **Footer in the commit body:** <footer>` line from `git_master.commit_footer`;
//                     the other read and the errored read carry NO directive.
//
// Every scenario uses an isolated HOME, a loopback `openai-completions` provider, and an exact event
// subscription registered BEFORE each trigger (no fixed sleeps and no polling: the turn settle is
// the `agent_settled` record awaited under a bound). Cleanup is the owned host teardown (SIGTERM with
// the socket removal subscribed BEFORE the signal, then a bounded force kill), a bounded stream reap,
// provider stop and home removal. A surface the binary does not reach records "blocked" with the
// exact missing contract - never a proxy pass.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, watch, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";

const SCHEMA = "latest-omo-component-qa/v1";
const READY_TIMEOUT_MS = 30_000;
const REQUEST_TIMEOUT_MS = 30_000;
const EVENT_TIMEOUT_MS = 30_000;
const DEATH_TIMEOUT_MS = 15_000;
const GRACE_TIMEOUT_MS = 5_000;
const REAP_TIMEOUT_MS = 5_000;

const APPLIED_TYPE = "omo-model-profile:applied";
const UNAVAILABLE_TYPE = "omo-model-profile:unavailable";
const UNKNOWN_TYPE = "omo-model-profile:unknown";
const MODEL_PROFILE_PREFIX = "omo-model-profile:";
const DIRECTIVE_OPEN = "<commit_attribution>";
const AUTH_SENTINEL = "OMOSENTINEL_auth_9f31c0";
const FOOTER_SENTINEL = "QA-GITMASTER-FOOTER-9f31c0";
const OFFLINE_PROVIDER = "offline";
const SESSION_MODEL = { provider: OFFLINE_PROVIDER, id: "offline-a" };
const PROFILE_MODEL = { provider: OFFLINE_PROVIDER, id: "offline-b" };
const DEFAULT_THINKING_LEVEL = "medium";
const SCOPE_PATTERN = "offline-a:high";
const SCOPE_THINKING_LEVEL = "high";

const delay = (ms) => new Promise((resolvePromise) => setTimeout(resolvePromise, ms));

function usage(message) {
	console.error(`latest-omo-component-qa: ${message}`);
	console.error("usage: bun tools/latest-omo-component-qa.mjs --binary <mhc> --skills <staged skills root> --evidence <E> [--scenario profile-applied|profile-unknown|profile-invalid-auth|profile-scoped-boundary|git-master|all]");
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
		else if (key === "--skills") args.skills = argv[++i];
		else if (key === "--scenario") args.scenario = argv[++i];
		else usage(`unknown argument ${key}`);
	}
	if (!args.binary) usage("--binary is required");
	if (!args.evidence) usage("--evidence is required");
	return args;
}

// ---------------------------------------------------------------------------------------------
// isolated project / agent configuration
// ---------------------------------------------------------------------------------------------

/** The `openai-completions` provider the loopback SSE server serves, declared in models.json. */
function offlineProviderConfig(port, extraProviders = {}) {
	return {
		[OFFLINE_PROVIDER]: {
			api: "openai-completions",
			baseUrl: `http://127.0.0.1:${port}/v1`,
			apiKey: "offline-fixture",
			models: [
				{ id: "offline-a", reasoning: true, input: ["text"], contextWindow: 128000, maxTokens: 4096 },
				{ id: "offline-b", reasoning: true, input: ["text"], contextWindow: 128000, maxTokens: 4096 },
			],
		},
		...extraProviders,
	};
}

/** `<agent>/models.json` + `<agent>/settings.json`: the session model is pinned WITHOUT a CLI flag. */
function writeAgentConfig(agent, providers, defaults) {
	writeFileSync(join(agent, "models.json"), JSON.stringify({ providers }, null, 2) + "\n");
	writeFileSync(join(agent, "settings.json"), JSON.stringify(defaults, null, 2) + "\n");
}

/** The project config layer the components resolve: `<cwd>/.omo/omo.json`. */
function writeProjectConfig(project, config) {
	const dir = join(project, ".omo");
	mkdirSync(dir, { recursive: true });
	const path = join(dir, "omo.json");
	writeFileSync(path, JSON.stringify(config, null, 2) + "\n");
	return path;
}

// ---------------------------------------------------------------------------------------------
// the scripted loopback provider
// ---------------------------------------------------------------------------------------------

function sse(objects) {
	return objects.map((object) => `data: ${JSON.stringify(object)}\n\n`).join("") + "data: [DONE]\n\n";
}

function textEvents(text, finish = "stop") {
	return sse([
		{ id: "offline", object: "chat.completion.chunk", created: 0, model: "offline-a", choices: [{ index: 0, delta: { role: "assistant", content: text }, finish_reason: finish }] },
	]);
}

/** The `delta.tool_calls` chunk shape the memory-recall fixture uses for a REAL tool call. */
function toolCallEvents(call) {
	const delta = {
		tool_calls: [{ index: 0, id: call.id, type: "function", function: { name: call.name, arguments: JSON.stringify(call.arguments) } }],
	};
	return sse([
		{ id: "offline", object: "chat.completion.chunk", created: 0, model: "offline-a", choices: [{ index: 0, delta, finish_reason: "tool_calls" }] },
	]);
}

/**
 * One connection per request, answered from the script in order. `state.requests` keeps every request
 * body so the driver can prove the provider really saw the tool-result turns (not a fixture shortcut).
 */
function scriptedProvider(script) {
	const state = { requests: [], served: 0 };
	const server = Bun.serve({
		port: 0,
		async fetch(request) {
			const body = await request.text().catch(() => "");
			state.requests.push(body);
			const step = script[state.served++];
			const events = step === undefined ? textEvents("offline script exhausted") : step.kind === "toolCall" ? toolCallEvents(step.call) : textEvents(step.text);
			return new Response(events, { headers: { "content-type": "text/event-stream" } });
		},
	});
	return { server, port: server.port, state };
}

// ---------------------------------------------------------------------------------------------
// filesystem subscriptions (readiness / teardown) - never a poll
// ---------------------------------------------------------------------------------------------

function waitForPathAppear(path, timeoutMs) {
	return new Promise((resolvePromise) => {
		if (existsSync(path)) return resolvePromise(true);
		const directory = dirname(path);
		mkdirSync(directory, { recursive: true });
		const watcher = watch(directory, () => {
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
async function startHost(ctx, home, socket, provider, extraArgs = []) {
	const agent = join(home, "agent");
	const env = {
		PATH: process.env.PATH,
		HOME: home,
		MAHO_CODING_AGENT_DIR: agent,
		OMO_SENPI_SKILLS_ROOT: ctx.skills,
	};
	// Subscribe BEFORE the spawn: readiness is the socket appearing, not a fixed-interval poll.
	const appeared = waitForPathAppear(socket, READY_TIMEOUT_MS);
	const host = Bun.spawn(
		[ctx.binary, "--mode", "rpc", "--multi-session", "--listen", `unix://${socket}`, "--offline", ...extraArgs],
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

function isProfileEntry(entry) {
	return typeof entry?.customType === "string" && entry.customType.startsWith(MODEL_PROFILE_PREFIX);
}

function profileEntries(entriesResponse) {
	return (entriesResponse?.data?.entries ?? []).filter(isProfileEntry);
}

function profileEvents(client) {
	return client.events.filter((event) => event?.type === "entry_appended" && isProfileEntry(event?.entry)).map((event) => event.entry);
}

/** The applied-state projection: `get_state` when the host answers it, else the open_session state. */
function effectiveState(receipt) {
	if (receipt.getState) return { source: "get_state", state: receipt.getState };
	if (receipt.openState) return { source: "open_session.state", state: receipt.openState };
	return { source: null, state: null };
}

function stateModel(state) {
	const model = state?.model ?? null;
	return { provider: model?.provider ?? null, id: model?.id ?? null, thinkingLevel: state?.thinkingLevel ?? null };
}

function toolResultTexts(messagesResponse) {
	return (messagesResponse?.data?.messages ?? [])
		.filter((message) => message?.role === "toolResult")
		.map((message) => ({
			toolName: message.toolName ?? null,
			isError: message.isError === true,
			text: (Array.isArray(message.content) ? message.content : [])
				.filter((block) => block?.type === "text")
				.map((block) => block.text ?? "")
				.join("\n"),
		}));
}

// ---------------------------------------------------------------------------------------------
// scenario plumbing
// ---------------------------------------------------------------------------------------------

/**
 * Shared setup: isolated home, local provider, agent config, project config, host, client, session.
 * On a setup failure the host is torn down, the receipt is written, and `failure` carries the reason.
 */
async function openScenario(ctx, { receiptName, projectConfig, providers, defaults, script = [], hostArgs = [] }) {
	const home = mkdtempSync(join(tmpdir(), "component-qa-"));
	const receipt = { home, project: join(home, "project"), socket: join(home, "host.sock"), skillsRoot: ctx.skills, configPath: null, cleanup: {} };
	mkdirSync(join(home, "agent"), { recursive: true });
	mkdirSync(receipt.project, { recursive: true });
	const onboardingState = join(home, ".maho", "agent", "omo-senpi", "omo-native");
	mkdirSync(onboardingState, { recursive: true });
	writeFileSync(join(onboardingState, "onboarding-completed"), JSON.stringify({ completedAt: "2026-10-06T00:00:00Z", version: 1 }));
	const provider = scriptedProvider(script);
	writeAgentConfig(join(home, "agent"), providers(provider.port), defaults);
	receipt.configPath = writeProjectConfig(receipt.project, projectConfig);
	const started = await startHost(ctx, home, receipt.socket, provider, hostArgs);
	const state = { receipt, client: null, started, home, provider, failure: null };
	try {
		const ready = await started.appeared;
		if (!ready) throw new Error("the host never bound its socket");
		state.client = await connectHost(receipt.socket);
		const opened = await state.client.request({ type: "open_session", cwd: receipt.project });
		receipt.openResponse = opened;
		receipt.sessionId = opened?.data?.sessionId ?? null;
		receipt.openState = opened?.data?.state ?? null;
		if (!receipt.sessionId) throw new Error("the host answered no sessionId to open_session");
	} catch (error) {
		state.failure = error.message;
		if (state.client) state.client.close();
		await teardown(receipt, home, started.host, provider, receipt.socket, started.stdoutText, started.stderrText);
		writeFileSync(join(ctx.qaRoot, receiptName), JSON.stringify(receipt, null, 2) + "\n");
	}
	return state;
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

/** Read the profile observables every profile scenario needs (entries + state + captured events). */
async function readProfileObservables(state) {
	const { receipt, client } = state;
	const entries = await client.request({ type: "get_entries", sessionId: receipt.sessionId });
	receipt.entries = entries;
	const stateResponse = await client.request({ type: "get_state", sessionId: receipt.sessionId });
	if (stateResponse?.success === true) receipt.getState = stateResponse.data;
	else receipt.getStateError = stateResponse ? { command: stateResponse.command, error: stateResponse.error } : null;
	receipt.customEntries = profileEntries(entries);
	receipt.customEvents = profileEvents(client);
	receipt.openStateEntries = (receipt.openState?.entries ?? []).filter(isProfileEntry);
	return [...receipt.customEntries, ...receipt.customEvents, ...receipt.openStateEntries];
}

// ---------------------------------------------------------------------------------------------
// scenarios
// ---------------------------------------------------------------------------------------------

async function scenarioProfileApplied(ctx) {
	const state = await openScenario(ctx, {
		receiptName: "profile-applied-receipt.json",
		projectConfig: {
			model_profile: "qa-lane",
			model_profiles: { "qa-lane": { display_name: "QA Lane", models: [{ model: "offline/offline-b", reasoning: "high" }] } },
		},
		providers: (port) => offlineProviderConfig(port),
		defaults: { defaultProvider: SESSION_MODEL.provider, defaultModel: SESSION_MODEL.id },
	});
	if (state.failure) return blockedResult(state, "profile-applied");
	const notices = await readProfileObservables(state);
	const applied = notices.find((entry) => entry.customType === APPLIED_TYPE) ?? null;
	state.receipt.applied = applied;
	return closeScenario(ctx, state, "profile-applied-receipt.json", (receipt) => {
		const { source, state: effective } = effectiveState(receipt);
		receipt.effectiveStateSource = source;
		const observed = stateModel(effective);
		receipt.effectiveModel = observed;
		const details = applied?.details ?? null;
		const ok =
			Boolean(applied) &&
			details?.profile === "qa-lane" &&
			details?.model === "offline/offline-b" &&
			details?.reasoning === "high" &&
			observed.provider === PROFILE_MODEL.provider &&
			observed.id === PROFILE_MODEL.id &&
			observed.thinkingLevel === "high";
		const detail = ok
			? `profile-applied: the custom profile applied (details ${JSON.stringify(details)}; session model ${observed.provider}/${observed.id} at ${observed.thinkingLevel})`
			: `profile-applied: appliedEntry=${applied ? "present" : "absent"} details=${JSON.stringify(details)} effectiveModel=${observed.provider}/${observed.id} thinkingLevel=${observed.thinkingLevel} stateSource=${source ?? "none"} getStateError=${JSON.stringify(receipt.getStateError)} (no applied entry arrived within the bound; inspect session-start origin, extension mode and session identity)`;
		return { ok, detail };
	});
}

async function scenarioProfileUnknown(ctx) {
	const state = await openScenario(ctx, {
		receiptName: "profile-unknown-receipt.json",
		projectConfig: { model_profile: "qa-does-not-exist" },
		providers: (port) => offlineProviderConfig(port),
		defaults: { defaultProvider: SESSION_MODEL.provider, defaultModel: SESSION_MODEL.id },
	});
	if (state.failure) return blockedResult(state, "profile-unknown");
	const notices = await readProfileObservables(state);
	const unknown = notices.find((entry) => entry.customType === UNKNOWN_TYPE) ?? null;
	state.receipt.unknown = unknown;
	return closeScenario(ctx, state, "profile-unknown-receipt.json", (receipt) => {
		const { source, state: effective } = effectiveState(receipt);
		receipt.effectiveStateSource = source;
		const observed = stateModel(effective);
		receipt.effectiveModel = observed;
		const content = unknown ? JSON.stringify(unknown.content ?? null) : "";
		const ok =
			Boolean(unknown) &&
			content.includes("qa-does-not-exist") &&
			observed.provider === SESSION_MODEL.provider &&
			observed.id === SESSION_MODEL.id &&
			observed.thinkingLevel === DEFAULT_THINKING_LEVEL;
		const detail = ok
			? `profile-unknown: the unknown id was reported and the session model stayed ${observed.provider}/${observed.id} at ${observed.thinkingLevel}`
			: `profile-unknown: unknownEntry=${unknown ? "present" : "absent"} content=${content} effectiveModel=${observed.provider}/${observed.id} thinkingLevel=${observed.thinkingLevel} stateSource=${source ?? "none"} getStateError=${JSON.stringify(receipt.getStateError)}`;
		return { ok, detail };
	});
}

async function scenarioProfileInvalidAuth(ctx) {
	const state = await openScenario(ctx, {
		receiptName: "profile-invalid-auth-receipt.json",
		projectConfig: { model_profile: "qa-dead", model_profiles: { "qa-dead": { models: [{ model: "dead/dead-model" }] } } },
		providers: (port) =>
			offlineProviderConfig(port, {
				dead: {
					api: "openai-completions",
					baseUrl: `http://127.0.0.1:${port}/v1`,
					apiKey: `!exit 1 # ${AUTH_SENTINEL}`,
					models: [{ id: "dead-model", reasoning: false, input: ["text"], contextWindow: 128000, maxTokens: 4096 }],
				},
			}),
		defaults: { defaultProvider: SESSION_MODEL.provider, defaultModel: SESSION_MODEL.id },
	});
	if (state.failure) return blockedResult(state, "profile-invalid-auth");
	const notices = await readProfileObservables(state);
	state.receipt.notices = notices;
	return closeScenario(ctx, state, "profile-invalid-auth-receipt.json", (receipt) => {
		const captured = receipt.notices ?? [];
		const noticeText = JSON.stringify(captured);
		const authFailure = captured.find((entry) => Array.isArray(entry?.details?.authFailed) && entry.details.authFailed.some((failure) => failure?.provider === "dead")) ?? null;
		receipt.authFailureNotice = authFailure;
		receipt.sentinelInNotices = noticeText.includes(AUTH_SENTINEL);
		receipt.sentinelInStderr = typeof receipt.stderr === "string" && receipt.stderr.includes(AUTH_SENTINEL);
		const noticeOk = Boolean(authFailure) && (authFailure.customType === UNAVAILABLE_TYPE || authFailure.customType === APPLIED_TYPE);
		const ok = noticeOk && receipt.sentinelInNotices === false && receipt.sentinelInStderr === false;
		const detail = ok
			? `profile-invalid-auth: the auth-failure notice (${authFailure.customType}) names provider dead and the sentinel appears in neither the notices nor the host stderr`
			: noticeOk
				? `profile-invalid-auth: auth-failure notice captured but the sentinel leaked (notices=${receipt.sentinelInNotices} stderr=${receipt.sentinelInStderr})`
				: `profile-invalid-auth: no auth-failure notice captured (${captured.length} profile notices); the profile hook never ran, so absence of the sentinel is not proven`;
		return { ok, detail };
	});
}

async function scenarioGitMaster(ctx) {
	if (!ctx.skillsProvided) {
		return { status: "blocked", blocker: "git-master: --skills <staged skills root> is required (the real read must target the STAGED <skills>/git-master/SKILL.md)", artifacts: [], cleanup_ok: true };
	}
	const skillPath = join(ctx.skills, "git-master", "SKILL.md");
	if (!existsSync(skillPath)) {
		return { status: "blocked", blocker: `git-master: ${skillPath} is absent from the staged skills root`, artifacts: [], cleanup_ok: true };
	}
	const script = [
		{ kind: "toolCall", call: { id: "gm-1", name: "read", arguments: { path: skillPath } } },
		{ kind: "text", text: "offline turn 1 done" },
		{ kind: "toolCall", call: { id: "gm-2", name: "read", arguments: { path: null } } },
		{ kind: "text", text: "offline turn 2 done" },
		{ kind: "toolCall", call: { id: "gm-3", name: "read", arguments: { path: null } } },
		{ kind: "text", text: "offline turn 3 done" },
	];
	const state = await openScenario(ctx, {
		receiptName: "git-master-receipt.json",
		projectConfig: { git_master: { commit_footer: FOOTER_SENTINEL } },
		providers: (port) => offlineProviderConfig(port),
		defaults: { defaultProvider: SESSION_MODEL.provider, defaultModel: SESSION_MODEL.id },
		script,
	});
	if (state.failure) return blockedResult(state, "git-master");
	const { receipt, client, provider } = state;
	const otherPath = join(receipt.project, "README.md");
	const absentPath = join(receipt.project, "absent", "git-master", "SKILL.md");
	writeFileSync(otherPath, "# unrelated file\n");
	// The two boundary reads target real paths only now that the isolated project exists.
	script[2].call.arguments.path = otherPath;
	script[4].call.arguments.path = absentPath;
	receipt.skillPath = skillPath;
	receipt.otherPath = otherPath;
	receipt.absentPath = absentPath;
	const turns = [
		{ message: "read the git-master skill" },
		{ message: "read an unrelated file" },
		{ message: "read a missing git-master skill" },
	];
	receipt.turns = [];
	for (const turn of turns) {
		// AgentIdle follows settlement and the work barrier; subscribe before this turn.
		const settled = client.waitForEvent((event) => event?.type === "agent_idle");
		const response = await client.request({ type: "prompt", sessionId: receipt.sessionId, message: turn.message, sessionTitlePrompt: false });
		const settleEvent = await settled;
		receipt.turns.push({ message: turn.message, promptResponse: response, settled: settleEvent !== null });
	}
	const messages = await client.request({ type: "get_messages", sessionId: receipt.sessionId });
	receipt.messages = messages;
	receipt.providerRequests = provider.state.requests.length;
	receipt.providerServed = provider.state.served;
	receipt.toolResults = toolResultTexts(messages);
	return closeScenario(ctx, state, "git-master-receipt.json", (final) => {
		const results = final.toolResults;
		const withDirective = results.filter((result) => result.text.includes(DIRECTIVE_OPEN));
		const footerLine = `1. **Footer in the commit body:** ${FOOTER_SENTINEL}`;
		const promptsSettled = (final.turns ?? []).every((turn) => turn.promptResponse?.success === true && turn.settled === true);
		const ok =
			withDirective.length === 1 &&
			withDirective[0].isError === false &&
			withDirective[0].text.includes(footerLine) &&
			results.length === 3 &&
			results.filter((result) => result.isError).length === 1 &&
			promptsSettled &&
			final.providerServed === 6;
		const detail = ok
			? `git-master: exactly one toolResult (of ${results.length}) carried the directive with the custom footer "${FOOTER_SENTINEL}"; the other read and the errored read carried none`
			: `git-master: toolResults=${results.length} withDirective=${withDirective.length} errored=${results.filter((result) => result.isError).length} footerPresent=${withDirective.some((result) => result.text.includes(footerLine))} promptsSettled=${promptsSettled} providerServed=${final.providerServed}/6`;
		return { ok, detail };
	});
}

// ---------------------------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------------------------

// open_session responds after the mount awaits bind_extensions and SessionStart dispatch.
// State reads therefore observe completed hook effects; absence needs no timed wait.
async function scenarioProfileScopedBoundary(ctx) {
	const state = await openScenario(ctx, {
		receiptName: "profile-scoped-boundary-receipt.json",
		projectConfig: {
			model_profile: "qa-scoped",
			model_profiles: { "qa-scoped": { display_name: "QA Scoped", models: [{ model: "offline/offline-b", reasoning: "high" }] } },
		},
		providers: (port) => offlineProviderConfig(port),
		defaults: { defaultProvider: PROFILE_MODEL.provider, defaultModel: PROFILE_MODEL.id },
		hostArgs: ["--models", SCOPE_PATTERN],
	});
	if (state.failure) return blockedResult(state, "profile-scoped-boundary");
	state.receipt.notices = await readProfileObservables(state);
	return closeScenario(ctx, state, "profile-scoped-boundary-receipt.json", (receipt) => {
		const { source, state: effective } = effectiveState(receipt);
		receipt.effectiveStateSource = source;
		const observed = stateModel(effective);
		receipt.effectiveModel = observed;
		const entries = receipt.customEntries ?? [];
		const events = receipt.customEvents ?? [];
		const openEntries = receipt.openStateEntries ?? [];
		const ok =
			receipt.entries?.success === true &&
			entries.length === 0 && events.length === 0 && openEntries.length === 0 &&
			observed.provider === SESSION_MODEL.provider && observed.id === SESSION_MODEL.id &&
			observed.thinkingLevel === SCOPE_THINKING_LEVEL;
		const detail = `profile-scoped-boundary: profileEntries=${entries.length} profileEvents=${events.length} openStateEntries=${openEntries.length} effectiveModel=${observed.provider}/${observed.id} thinkingLevel=${observed.thinkingLevel} stateSource=${source ?? "none"}; expected offline/offline-a at high rather than settings/profile offline/offline-b, with no profile entry`;
		return { ok, detail };
	});
}

const SCENARIOS = {
	"profile-applied": scenarioProfileApplied,
	"profile-unknown": scenarioProfileUnknown,
	"profile-invalid-auth": scenarioProfileInvalidAuth,
	"profile-scoped-boundary": scenarioProfileScopedBoundary,
	"git-master": scenarioGitMaster,
};

async function main() {
	const args = parseArgs(process.argv.slice(2));
	const evidence = resolve(args.evidence);
	const ctx = { binary: resolve(args.binary), qaRoot: join(evidence, "qa"), skills: args.skills ? resolve(args.skills) : null, skillsProvided: Boolean(args.skills) };
	mkdirSync(ctx.qaRoot, { recursive: true });
	if (!ctx.skills) {
		// The mount still needs a skills root; the driver writes an EMPTY one, and the git-master
		// scenario records blocked because a staged git-master/SKILL.md is required.
		ctx.skills = join(evidence, "empty-skills");
		mkdirSync(ctx.skills, { recursive: true });
		console.error(`latest-omo-component-qa: --skills is absent; using an empty root at ${ctx.skills} (the git-master scenario will be blocked)`);
	}

	const binaryBefore = existsSync(ctx.binary) ? { sha256: sha256(readFileSync(ctx.binary)), mtimeMs: statSync(ctx.binary).mtimeMs } : null;
	if (!binaryBefore) console.error(`latest-omo-component-qa: --binary ${ctx.binary} is missing; every scenario will be blocked`);

	const names = args.scenario === "all" ? Object.keys(SCENARIOS) : [args.scenario];
	for (const name of names) if (!SCENARIOS[name]) usage(`unknown scenario ${name}`);

	const scenarios = {};
	let anyBlocked = false;
	for (const name of names) {
		console.log(`latest-omo-component-qa: scenario ${name}`);
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
		skills: ctx.skills,
		scenarios,
		status: anyBlocked ? "blocked" : "pass",
	};
	writeFileSync(join(ctx.qaRoot, "latest-omo-component-qa.json"), JSON.stringify(report, null, 2) + "\n");
	console.log(`latest-omo-component-qa: ${report.status} -> ${join(ctx.qaRoot, "latest-omo-component-qa.json")}`);
	process.exit(report.status === "pass" ? 0 : 1);
}

await main();
