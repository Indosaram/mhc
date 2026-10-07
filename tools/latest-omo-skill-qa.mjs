#!/usr/bin/env bun
// Real-surface QA driver for the registered skill surface (latest `455dee62`): the packaged skills
// root must reach the RPC command surface (`bundled-skills` + `get_commands`), and the bare
// `/<bundled-skill> args` form must expand exactly like `/skill:<name> args` (#9042,
// `skill-commands`).
//
//   bun tools/latest-omo-skill-qa.mjs --binary <built mhc> --evidence <isolated E> \
//        [--skills <skills root>] [--scenario get-commands|bare-rewrite|all]
//
// The host is the REAL `mhc --mode rpc --multi-session --listen unix://<home>/host.sock`, started
// with `OMO_SENPI_SKILLS_ROOT=<skills root>` and an EMPTY isolated agent dir, so the packaged root is
// the only possible skill source (the seam `omo_mount.rs` resolves).
//
//   --skills <root>  when given, the root is a STAGED skills tree (`tools/package-native.mjs
//                    --skill-source latest`). The driver then selects an ACTUAL skill that ships a
//                    SKILL.md inside that root, so the scenario proves a real packaged skill reaches
//                    the surface - never a fixture name searched inside an arbitrary root. When it
//                    is omitted, the driver writes its OWN one-skill fixture root and uses that.
//
//   get-commands  {"type":"get_commands"} on the host socket
//     observable  `data.commands` contains `skill:<selected>` with a `sourceInfo.path` under the
//                 skills root - the packaged skill reached the RPC command surface, not a library
//                 listing and not an app-server route.
//     cleanup     close the client; SIGTERM the owned host (SIGKILL cannot unlink its socket),
//                 subscribe the socket removal BEFORE the trigger, bounded force cleanup; rm -rf the
//                 isolated home; assert the socket is gone.
//
//   bare-rewrite  {"type":"prompt","sessionId":<sid>,"message":"/<selected> hello"} then
//                 {"type":"get_messages","sessionId":<sid>}
//     observable  the recorded user message text contains the selected SKILL.md body marker - the
//                 bare `/<selected>` expanded exactly like `/skill:<selected>`.
//     cleanup     same as get-commands.
//
// The `bare-rewrite` scenario needs a model to settle, so the driver serves a loopback `offline`
// provider (`api: openai-completions`, SSE) exactly like `crates/maho-cli/tests/print_entry.rs` and
// starts the host with `--offline --provider offline --model offline`. A host that cannot open a
// session records `blocked` with the exact missing contract, never a proxy pass.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, watch, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";

const SCHEMA = "latest-omo-skill-qa/v1";
const READY_TIMEOUT_MS = 30_000;
const REQUEST_TIMEOUT_MS = 30_000;
const DEATH_TIMEOUT_MS = 15_000;
const GRACE_TIMEOUT_MS = 5_000;
const REAP_TIMEOUT_MS = 5_000;
const FIXTURE_SKILL = "qa-fixture-skill";
const FIXTURE_MARKER = "QA_FIXTURE_SKILL_BODY_9c41";

const delay = (ms) => new Promise((resolvePromise) => setTimeout(resolvePromise, ms));

function usage(message) {
	console.error(`latest-omo-skill-qa: ${message}`);
	console.error("usage: bun tools/latest-omo-skill-qa.mjs --binary <mhc> --evidence <E> [--skills <root>] [--scenario get-commands|bare-rewrite|all]");
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

/** A fixture skills root holding ONE bundled skill, when the caller did not stage one. */
function fixtureSkillsRoot(home) {
	const root = join(home, "skills");
	mkdirSync(join(root, FIXTURE_SKILL), { recursive: true });
	writeFileSync(join(root, FIXTURE_SKILL, "SKILL.md"), `---\nname: ${FIXTURE_SKILL}\ndescription: QA fixture skill\n---\n\n${FIXTURE_MARKER}\n`);
	return root;
}

/** The body marker a bare `/<skill>` expansion must reproduce: a distinctive non-heading line. */
function skillMarker(body, name) {
	const stripped = body.replace(/^---[\s\S]*?---\n/, "").trim();
	const line = stripped.split("\n").map((candidate) => candidate.trim()).find((candidate) => candidate.length >= 8 && !candidate.startsWith("#"));
	return line ?? stripped.split("\n")[0]?.trim() ?? name;
}

/**
 * Resolve the skill under test. With a STAGED root the driver must select an ACTUAL staged skill
 * (a directory shipping a SKILL.md), never the fixture name searched inside that root; without one
 * it writes and uses its own fixture.
 */
function resolveSkill(home, stagedRoot) {
	if (!stagedRoot) {
		const root = fixtureSkillsRoot(home);
		return { root, name: FIXTURE_SKILL, marker: FIXTURE_MARKER, staged: false };
	}
	if (!existsSync(stagedRoot)) return { root: stagedRoot, name: null, marker: null, staged: true, reason: `${stagedRoot} does not exist` };
	const entries = readdirSync(stagedRoot, { withFileTypes: true })
		.filter((entry) => entry.isDirectory() && existsSync(join(stagedRoot, entry.name, "SKILL.md")))
		.map((entry) => entry.name)
		.sort((left, right) => left.localeCompare(right));
	if (entries.length === 0) return { root: stagedRoot, name: null, marker: null, staged: true, reason: `${stagedRoot} holds no staged skill with a SKILL.md` };
	const directory = entries[0];
	const body = readFileSync(join(stagedRoot, directory, "SKILL.md"), "utf8");
	return { root: stagedRoot, name: skillName(body, directory), marker: skillMarker(body, directory), staged: true };
}

/** The registered skill NAME: the frontmatter `name:` when present, else the directory name. */
function skillName(body, directory) {
	const frontmatter = /^---\n([\s\S]*?)\n---/.exec(body)?.[1] ?? "";
	const declared = /^name:\s*(.+)$/m.exec(frontmatter)?.[1]?.trim().replace(/^["']|["']$/g, "");
	return declared && declared.length > 0 ? declared : directory;
}

/** The loopback `offline` provider, byte-shaped like the print-entry fixture. */
function startOfflineProvider() {
	const body = [
		'data: {"id":"offline","object":"chat.completion.chunk","created":0,"model":"offline","choices":[{"index":0,"delta":{"role":"assistant","content":"offline"},"finish_reason":null}]}',
		"",
		'data: {"id":"offline","object":"chat.completion.chunk","created":0,"model":"offline","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}',
		"",
		"data: [DONE]",
		"",
	].join("\n");
	const server = Bun.serve({ port: 0, fetch: () => new Response(body, { headers: { "content-type": "text/event-stream" } }) });
	return { server, port: server.port };
}

/**
 * Bounded await of a path APPEARING, driven by the filesystem event rather than a poll: register
 * BEFORE the spawn that creates it, so the creation cannot be missed.
 */
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

/** A persistent JSONL client on the host socket: one request per id, answers keyed by id. */

/** Start consuming a pipe immediately, so a chatty host can never block on a full pipe. */
function drain(stream) {
	return new Response(stream).text().catch(() => null);
}

/** The recorded USER message text from a `get_messages` answer, not the whole JSON blob. */
function userMessageText(messages) {
	const rows = messages?.data?.messages ?? messages?.data?.entries ?? [];
	return rows
		.filter((row) => row?.role === "user" || row?.type === "user" || row?.message?.role === "user")
		.slice(-1)
		.map((row) => row?.content ?? row?.text ?? row?.message?.content ?? "")
		.map((content) => (typeof content === "string" ? content : JSON.stringify(content)))
		.join("\n");
}
async function connectHost(socketPath) {
	const client = { buffer: "", waiters: new Map(), nextId: 1, socket: null };
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
					if (message && client.waiters.has(message.id)) {
						const settle = client.waiters.get(message.id);
						client.waiters.delete(message.id);
						settle(message);
					}
				}
			},
			close() {
				for (const settle of client.waiters.values()) settle(null);
				client.waiters.clear();
			},
			error() {},
		},
	});
	client.request = (fields, ms = REQUEST_TIMEOUT_MS) =>
		new Promise((resolvePromise) => {
			const id = client.nextId++;
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
	client.close = () => {
		try {
			client.socket.end();
		} catch {
			/* already closed */
		}
	};
	return client;
}

/** Start the real multi-session host over an isolated home; return the child and the socket path. */
async function startHost(ctx, home, skillsRoot) {
	const agent = join(home, "agent");
	mkdirSync(agent, { recursive: true });
	const socket = join(home, "host.sock");
	const env = {
		PATH: process.env.PATH,
		HOME: home,
		MAHO_CODING_AGENT_DIR: agent,
		OMO_SENPI_SKILLS_ROOT: skillsRoot,
	};
	const provider = startOfflineProvider();
	writeFileSync(join(agent, "models.json"), JSON.stringify({ providers: { offline: {
		api: "openai-completions", baseUrl: `http://127.0.0.1:${provider.port}/v1`, apiKey: "offline-fixture",
		models: [{ id: "offline", reasoning: false, input: ["text"], contextWindow: 128000, maxTokens: 4096 }],
	} } }));
	// Subscribe BEFORE the spawn: readiness is the socket appearing, not a fixed-interval poll.
	const appeared = waitForPathAppear(socket, READY_TIMEOUT_MS);
	const host = Bun.spawn(
		[ctx.binary, "--mode", "rpc", "--multi-session", "--listen", `unix://${socket}`, "--offline", "--provider", "offline", "--model", "offline"],
		{ cwd: home, env, detached: true, stdin: "ignore", stdout: "pipe", stderr: "pipe" },
	);
	// Drain BOTH pipes from the instant of spawn, or the host can block writing a full pipe.
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

async function scenarioGetCommands(ctx) {
	const home = mkdtempSync(join(tmpdir(), "skill-qa-home-"));
	const receipt = { home, skillsRoot: ctx.skills ?? null, selectedSkill: null, staged: Boolean(ctx.skills), socket: null, commands: null, cleanup: {} };
	let ok = false;
	let detail = "";
	let host = null;
	let provider = null;
	let started = null;
	let socket = join(home, "host.sock");
	let client = null;
	try {
		mkdirSync(join(home, "project"), { recursive: true });
		const skill = resolveSkill(home, ctx.skills);
		receipt.skillsRoot = skill.root;
		receipt.selectedSkill = skill.name;
		if (!skill.name) {
			detail = skill.reason;
			throw new Error(skill.reason);
		}
		started = await startHost(ctx, home, skill.root);
		host = started.host; provider = started.provider; socket = started.socket;
		receipt.socket = socket;
		const ready = await started.appeared;
		if (!ready) {
			detail = "the host never bound its socket";
		} else {
			client = await connectHost(socket);
			const opened = await client.request({ type: "open_session", cwd: join(home, "project") });
			const sessionId = opened?.data?.sessionId ?? null;
			receipt.sessionId = sessionId;
			if (!sessionId) {
				detail = "the host answered no sessionId to open_session";
			} else {
				const commands = await client.request({ type: "get_commands", sessionId });
				receipt.commands = commands;
				const list = commands?.data?.commands ?? [];
				const found = list.find((command) => command?.name === `skill:${skill.name}`);
				receipt.skillCommand = found ?? null;
				ok = Boolean(found) && String(found?.sourceInfo?.path ?? "").startsWith(skill.root);
				detail = `skillCommand=${found ? "present" : "absent"} pathUnderRoot=${String(found?.sourceInfo?.path ?? "").startsWith(skill.root)}`;
			}
		}
	} catch (error) {
		detail = `driver error ${error.message}`;
	} finally {
		if (client) client.close();
		if (host !== null && provider !== null && started !== null) {
			await teardown(receipt, home, host, provider, socket, started.stdoutText, started.stderrText);
		} else {
			receipt.cleanup = { gracefulExit: false, socketGone: !existsSync(socket), streamsComplete: false, providerStopped: provider === null, homeRemoved: false };
			rmSync(home, { recursive: true, force: true });
			receipt.cleanup.homeRemoved = !existsSync(home);
		}
	}
	writeFileSync(join(ctx.qaRoot, "skill-get-commands-receipt.json"), JSON.stringify(receipt, null, 2) + "\n");
	const cleanupOk = receipt.cleanup.gracefulExit === true && receipt.cleanup.socketGone === true && receipt.cleanup.streamsComplete === true && receipt.cleanup.providerStopped === true && receipt.cleanup.homeRemoved === true;
	return {
		status: ok && cleanupOk ? "pass" : "blocked",
		blocker: ok && cleanupOk ? null : `get-commands: ${detail} cleanup=${JSON.stringify(receipt.cleanup)} (a packaged skill must appear as skill:<name> on the RPC command surface)`,
		artifacts: ["skill-get-commands-receipt.json"],
		cleanup_ok: cleanupOk,
	};
}

async function scenarioBareRewrite(ctx) {
	const home = mkdtempSync(join(tmpdir(), "skill-qa-home-"));
	const receipt = { home, skillsRoot: ctx.skills ?? null, selectedSkill: null, staged: Boolean(ctx.skills), marker: null, socket: null, messages: null, cleanup: {} };
	let ok = false;
	let detail = "";
	let host = null;
	let provider = null;
	let started = null;
	let socket = join(home, "host.sock");
	let client = null;
	try {
		mkdirSync(join(home, "project"), { recursive: true });
		const skill = resolveSkill(home, ctx.skills);
		receipt.skillsRoot = skill.root;
		receipt.selectedSkill = skill.name;
		receipt.marker = skill.marker;
		if (!skill.name) {
			detail = skill.reason;
			throw new Error(skill.reason);
		}
		started = await startHost(ctx, home, skill.root);
		host = started.host; provider = started.provider; socket = started.socket;
		receipt.socket = socket;
		const ready = await started.appeared;
		if (!ready) {
			detail = "the host never bound its socket";
		} else {
			client = await connectHost(socket);
			const opened = await client.request({ type: "open_session", cwd: join(home, "project") });
			const sessionId = opened?.data?.sessionId ?? null;
			receipt.sessionId = sessionId;
			if (!sessionId) {
				detail = "the host answered no sessionId to open_session";
			} else {
				await client.request({ type: "prompt", sessionId, message: `/${skill.name} hello` });
				const bareMessages = await client.request({ type: "get_messages", sessionId });
				receipt.bareMessage = userMessageText(bareMessages);
				const bareExpanded = receipt.bareMessage.includes(skill.marker);
				await client.request({ type: "prompt", sessionId, message: `/skill:${skill.name} hello` });
				const namedMessages = await client.request({ type: "get_messages", sessionId });
				receipt.namedMessage = userMessageText(namedMessages);
				const namedExpanded = receipt.namedMessage.includes(skill.marker);
				receipt.expandedBare = bareExpanded;
				receipt.expandedNamed = namedExpanded;
				ok = Boolean(skill.marker) && bareExpanded && namedExpanded;
				detail = `expandedBare=${bareExpanded} expandedNamed=${namedExpanded}`;
			}
		}
	} catch (error) {
		detail = `driver error ${error.message}`;
	} finally {
		if (client) client.close();
		if (host !== null && provider !== null && started !== null) {
			await teardown(receipt, home, host, provider, socket, started.stdoutText, started.stderrText);
		} else {
			receipt.cleanup = { gracefulExit: false, socketGone: !existsSync(socket), streamsComplete: false, providerStopped: provider === null, homeRemoved: false };
			rmSync(home, { recursive: true, force: true });
			receipt.cleanup.homeRemoved = !existsSync(home);
		}
	}
	writeFileSync(join(ctx.qaRoot, "skill-bare-rewrite-receipt.json"), JSON.stringify(receipt, null, 2) + "\n");
	const cleanupOk = receipt.cleanup.gracefulExit === true && receipt.cleanup.socketGone === true && receipt.cleanup.streamsComplete === true && receipt.cleanup.providerStopped === true && receipt.cleanup.homeRemoved === true;
	return {
		status: ok && cleanupOk ? "pass" : "blocked",
		blocker: ok && cleanupOk ? null : `bare-rewrite: ${detail} cleanup=${JSON.stringify(receipt.cleanup)} (the bare /<skill> form must expand like /skill:<name>)`,
		artifacts: ["skill-bare-rewrite-receipt.json"],
		cleanup_ok: cleanupOk,
	};
}

const SCENARIOS = { "get-commands": scenarioGetCommands, "bare-rewrite": scenarioBareRewrite };

async function main() {
	const args = parseArgs(process.argv.slice(2));
	const ctx = { binary: resolve(args.binary), qaRoot: join(resolve(args.evidence), "qa"), skills: args.skills ? resolve(args.skills) : null };
	mkdirSync(ctx.qaRoot, { recursive: true });

	const binaryBefore = existsSync(ctx.binary) ? { sha256: sha256(readFileSync(ctx.binary)), mtimeMs: statSync(ctx.binary).mtimeMs } : null;
	if (!binaryBefore) console.error(`latest-omo-skill-qa: --binary ${ctx.binary} is missing; every scenario will be blocked`);

	const names = args.scenario === "all" ? Object.keys(SCENARIOS) : [args.scenario];
	for (const name of names) if (!SCENARIOS[name]) usage(`unknown scenario ${name}`);

	const scenarios = {};
	let anyBlocked = false;
	for (const name of names) {
		console.log(`latest-omo-skill-qa: scenario ${name}`);
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
	writeFileSync(join(ctx.qaRoot, "latest-omo-skill-qa.json"), JSON.stringify(report, null, 2) + "\n");
	console.log(`latest-omo-skill-qa: ${report.status} -> ${join(ctx.qaRoot, "latest-omo-skill-qa.json")}`);
	process.exit(report.status === "pass" ? 0 : 1);
}

await main();
