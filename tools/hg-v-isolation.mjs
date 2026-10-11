#!/usr/bin/env bun
// Installed V01 acceptance driver for config-only task isolation and owned patch merge.
// Writes <evidence>/qa/hg-v-isolation.json. Host, HOME, agent config and CWD are isolated.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, watch, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";

const SCHEMA = "hg-v-isolation/v1";
const CHILD_EDIT = "child-isolation-edit.txt";
const CHILD_CONTENT = "written by isolated child\n";
const PARENT_EDIT = "concurrent parent edit\n";
const CHILD_READY = "V01_ISOLATION_CHILD_EDIT_READY_91db";
const CHILD_DONE = "V01_ISOLATION_CHILD_COMPLETED_91db";
const PARENT_DONE = "V01_ISOLATION_PARENT_COMPLETED_91db";
const CHILD_NAME = "v01-isolation-child";
const TIMEOUT = 60_000;

function usage(message) {
	console.error(`hg-v-isolation: ${message}`);
	console.error("usage: bun tools/hg-v-isolation.mjs --binary <mhc> --install <dir> --evidence <dir> --expected-sha256 <sha256>");
	process.exit(2);
}

function parseArgs(argv) {
	const args = {};
	for (let i = 0; i < argv.length; i++) {
		const key = argv[i];
		if (key === "--binary") args.binary = argv[++i];
		else if (key === "--install") args.install = argv[++i];
		else if (key === "--evidence") args.evidence = argv[++i];
		else if (key === "--expected-sha256") args.expectedSha = argv[++i];
		else usage(`unknown argument ${key}`);
	}
	for (const key of ["binary", "install", "evidence", "expectedSha"]) if (!args[key]) usage(`${key} is required`);
	if (!/^[a-f0-9]{64}$/i.test(args.expectedSha)) usage("--expected-sha256 must be 64 hexadecimal characters");
	return args;
}

const sha256 = (data) => createHash("sha256").update(data).digest("hex");
const fingerprint = (path) => existsSync(path) ? { sha256: sha256(readFileSync(path)), mtime_ms: statSync(path).mtimeMs } : null;
const isolatedDirs = () => {
	const root = mkdtempSync(join(tmpdir(), "hg-v-isolation-"));
	const dirs = { root, home: join(root, "home"), cwd: join(root, "cwd"), agent: join(root, "agent"), memory: join(root, "memory") };
	for (const dir of [dirs.home, dirs.cwd, dirs.agent, dirs.memory]) mkdirSync(dir, { recursive: true });
	return dirs;
};

function makeProvider(replyForRequest) {
	const requests = [];
	const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
		let raw = "";
		try { raw = await request.text(); } catch { /* client disconnected */ }
		let parsed = null;
		try { parsed = JSON.parse(raw); } catch { /* preserve request evidence */ }
		requests.push({ raw, parsed });
		const answer = await replyForRequest(parsed);
		if (answer === null) return new Response(new ReadableStream({ start() {} }), { headers: { "content-type": "text/event-stream" } });
		const chunk = (delta, finish = null) => `data: ${JSON.stringify({ id: "offline", object: "chat.completion.chunk", created: 0, model: "offline", choices: [{ index: 0, delta, finish_reason: finish }] })}\n\n`;
		const body = answer?.toolCall
			? chunk({ role: "assistant", tool_calls: [{ index: 0, id: "call_v01_isolation", type: "function", function: { name: answer.toolCall.name ?? "eval", arguments: JSON.stringify(answer.toolCall.arguments ?? answer.toolCall) } }] }, "tool_calls") + "data: [DONE]\n\n"
			: chunk({ role: "assistant", content: answer }) + chunk({}, "stop") + "data: [DONE]\n\n";
		return new Response(body, { headers: { "content-type": "text/event-stream" } });
	} });
	return { server, requests, baseUrl: `http://127.0.0.1:${server.port}/v1` };
}

function configureAgent(agent, baseUrl) {
	const provider = { api: "openai-completions", baseUrl, apiKey: "offline-fixture", models: [{ id: "offline", reasoning: false, input: ["text"], contextWindow: 128000, maxTokens: 4096 }] };
	writeFileSync(join(agent, "models.json"), JSON.stringify({ providers: { offline: provider } }));
	const configDir = join(agent, "omo-senpi", "omo-native");
	mkdirSync(configDir, { recursive: true });
	writeFileSync(join(configDir, "onboarding-completed"), "");
}

function isolatedEnv(dirs) {
	return { PATH: process.env.PATH, TMPDIR: dirs.root, HOME: dirs.home, MAHO_CODING_AGENT_DIR: dirs.agent, MAHO_MEMORY_HOME: dirs.memory,
		PI_OFFLINE: "1", SENPI_OFFLINE: "1", PI_TELEMETRY: "0", XDG_CONFIG_HOME: join(dirs.home, ".config"), XDG_CACHE_HOME: join(dirs.home, ".cache") };
}

function subscribePath(path, wanted) {
	const parent = dirname(path);
	mkdirSync(parent, { recursive: true });
	const present = () => existsSync(path);
	if (present() === wanted) return Promise.resolve(true);
	return new Promise((resolvePromise) => {
		const watcher = watch(parent, () => {
			if (present() === wanted) { watcher.close(); clearTimeout(timer); resolvePromise(true); }
		});
		const timer = setTimeout(() => { watcher.close(); resolvePromise(present() === wanted); }, TIMEOUT);
	});
}

function connectRpc(socketPath) {
	return new Promise((resolvePromise, reject) => {
		const client = { buffer: "", waiters: new Map(), events: [], eventWaiters: new Set(), socket: null, sequence: 1 };
		const timeout = setTimeout(() => reject(new Error("RPC socket connection timed out")), TIMEOUT);
		Bun.connect({ unix: socketPath, socket: {
			open(socket) { clearTimeout(timeout); client.socket = socket; resolvePromise(client); },
			data(_socket, bytes) {
				client.buffer += bytes.toString();
				for (;;) {
					const end = client.buffer.indexOf("\n"); if (end < 0) break;
					const line = client.buffer.slice(0, end); client.buffer = client.buffer.slice(end + 1);
					let frame; try { frame = JSON.parse(line); } catch { continue; }
					if (frame.type === "response") {
						const waiter = client.waiters.get(String(frame.id));
						if (waiter) { client.waiters.delete(String(frame.id)); waiter(frame); }
					} else {
						client.events.push(frame);
						for (const waiter of client.eventWaiters) if (waiter.predicate(frame)) { client.eventWaiters.delete(waiter); waiter.resolve(frame); }
					}
				}
			},
			close() { for (const waiter of client.waiters.values()) waiter(null); client.waiters.clear(); }, error() {},
		} }).catch((error) => { clearTimeout(timeout); reject(error); });
		client.request = (type, fields = {}) => new Promise((res) => {
			const id = String(client.sequence++);
			const timer = setTimeout(() => { client.waiters.delete(id); res(null); }, TIMEOUT);
			client.waiters.set(id, (frame) => { clearTimeout(timer); res(frame); });
			client.socket.write(JSON.stringify({ id, type, ...fields }) + "\n");
		});
		client.waitEvent = (predicate, timeoutMs = TIMEOUT) => {
			const existing = client.events.find(predicate);
			if (existing) return Promise.resolve(existing);
			return new Promise((res) => {
				const waiter = { predicate, resolve: (frame) => { clearTimeout(timer); res(frame); } };
				const timer = setTimeout(() => { client.eventWaiters.delete(waiter); res(null); }, timeoutMs);
				client.eventWaiters.add(waiter);
			});
		};
		client.close = () => { try { client.socket.end(); } catch { /* already closed */ } };
	});
}

async function startHost(binary, dirs, provider) {
	configureAgent(dirs.agent, provider.baseUrl);
	const socket = join(dirs.root, "host.sock");
	const appeared = subscribePath(socket, true);
	const child = Bun.spawn([binary, "--mode", "rpc", "--multi-session", "--listen", `unix://${socket}`, "--offline", "--provider", "offline", "--model", "offline"], {
		cwd: dirs.cwd, env: isolatedEnv(dirs), stdin: "ignore", stdout: "pipe", stderr: "pipe",
	});
	const stdout = new Response(child.stdout).text();
	const stderr = new Response(child.stderr).text();
	const ready = await appeared;
	return { child, socket, ready, stdout, stderr, client: ready ? await connectRpc(socket) : null };
}

function pidAlive(pid) {
	try { process.kill(pid, 0); return true; }
	catch (error) { return !(error instanceof Error && "code" in error && error.code === "ESRCH"); }
}

async function cleanup(host, client, dirs, provider) {
	const receipt = { pids: [], sockets: [], dirs: [], gone: {} };
	if (host?.child) {
		receipt.pids.push(host.child.pid);
		receipt.sockets.push(host.socket);
		const socketGone = subscribePath(host.socket, false);
		try { process.kill(host.child.pid, "SIGTERM"); } catch { /* exited */ }
		let timer;
		const exited = await Promise.race([host.child.exited.then(() => true), new Promise((res) => { timer = setTimeout(() => res(false), 5000); })]);
		clearTimeout(timer);
		if (!exited) { try { process.kill(host.child.pid, "SIGKILL"); } catch { /* exited */ } await host.child.exited; }
		receipt.gone.host_process = !pidAlive(host.child.pid);
		receipt.gone.socket = await socketGone;
		await Promise.all([host.stdout?.catch(() => ""), host.stderr?.catch(() => "")]);
	}
	if (client) client.close();
	if (provider) { provider.server.stop(true); receipt.pids.push(`provider:${provider.server.port}`); receipt.gone.provider_socket = true; }
	const sandboxRoot = join(dirs.home, ".omo", "wt");
	receipt.sandbox_entries_before_removal = existsSync(sandboxRoot) ? readdirSync(sandboxRoot) : [];
	receipt.gone.owned_worktrees = receipt.sandbox_entries_before_removal.length === 0;
	if (dirs) { receipt.dirs.push(dirs.root); rmSync(dirs.root, { recursive: true, force: true }); receipt.gone.temp_dir = !existsSync(dirs.root); }
	return receipt;
}

function messagesOf(request) { return request?.messages ?? []; }
function hasText(request, text) { return messagesOf(request).some((message) => JSON.stringify(message).includes(text)); }

async function runScenario(ctx) {
	const dirs = isolatedDirs();
	let parentSession = null;
	let childObserved = false;
	let parentEditMade = false;
	const childPrompt = `V01_ISOLATION_CHILD_ROUTING: write ${CHILD_EDIT} and return ${CHILD_DONE}`;
	const evalCode = `const host=await tool.task({prompt:${JSON.stringify(childPrompt)},subagent_type:"hephaestus",model:"offline/offline",name:${JSON.stringify(CHILD_NAME)},task_summary:"V01 isolated config-only child edit",run_in_background:true}); print("ISOLATION_TASK="+JSON.stringify(host));`;
	let readyResolve;
	const childReadySignal = new Promise(resolve => { readyResolve = resolve; });
	let childWriteIssued = false;
	const provider = makeProvider((request) => {
		const childRequest = messagesOf(request).some(message => message.role === "user" && JSON.stringify(message).includes("V01_ISOLATION_CHILD_ROUTING"));
		const parentRequest = messagesOf(request).some(message => message.role === "user" && JSON.stringify(message).includes("V01_ISOLATION_PARENT_ROUTING"));
		if (childRequest && !parentRequest) {
			if (!childWriteIssued) { childWriteIssued = true; return { toolCall: { name: "write", arguments: { path: CHILD_EDIT, content: CHILD_CONTENT } } }; }
			if (messagesOf(request).some(message => message.role === "tool" && JSON.stringify(message).includes(CHILD_EDIT) && !JSON.stringify(message).includes("not found") && !JSON.stringify(message).includes('"isError":true'))) {
				if (existsSync(join(dirs.cwd, CHILD_EDIT))) throw new Error("isolated child write leaked into parent before merge");
				writeFileSync(join(dirs.cwd, "parent-concurrent.txt"), PARENT_EDIT);
				parentEditMade = true; readyResolve(true);
				return CHILD_DONE;
			}
			throw new Error("child write result was not observed");
		}
		if (parentRequest && !childObserved) {
			childObserved = true;
			return { toolCall: { language: "js", code: evalCode, summary: "Spawn isolated config-only child task", on_timeout: "error", timeout: 45 } };
		}
		return PARENT_DONE;
	});
	let host = null, client = null;
	let observable = {};
	let pass = false;
	try {
		const git = async args => {
			const process = Bun.spawn(["git", "-C", dirs.cwd, ...args], { stdout: "pipe", stderr: "pipe", env: { ...globalThis.process.env, GIT_AUTHOR_NAME: "QA", GIT_AUTHOR_EMAIL: "qa@example.invalid", GIT_COMMITTER_NAME: "QA", GIT_COMMITTER_EMAIL: "qa@example.invalid" } });
			const [, stderr, code] = await Promise.all([new Response(process.stdout).text(), new Response(process.stderr).text(), process.exited]);
			if (code !== 0) throw new Error(stderr);
		};
		await git(["init", "-q"]);
		writeFileSync(join(dirs.cwd, "seed.txt"), "base\n");
		await git(["add", "seed.txt"]); await git(["commit", "-qm", "base"]);
		mkdirSync(join(dirs.home, ".maho"), { recursive: true });
		writeFileSync(join(dirs.home, ".maho", "omo.jsonc"), JSON.stringify({ task: { isolation: { enabled: true, backend: "auto", apply: true, merge: "patch" } } }));
		host = await startHost(ctx.binary, dirs, provider);
		client = host.client;
		if (!host.ready || !client) throw new Error("RPC host did not become ready");
		const opened = await client.request("open_session", { cwd: dirs.cwd });
		parentSession = opened?.data?.sessionId;
		if (!parentSession) throw new Error(`open_session returned no sessionId: ${JSON.stringify(opened)}`);
		const parentComplete = client.waitEvent((frame) => frame.type === "message_end" && frame.message?.role === "assistant" && JSON.stringify(frame.message).includes(PARENT_DONE));
		const evalEnded = client.waitEvent((frame) => frame.type === "tool_execution_end" && JSON.stringify(frame).includes("ISOLATION_TASK="));
		let readyTimer;
		const childReadyFromProvider = Promise.race([childReadySignal, new Promise(resolve => { readyTimer = setTimeout(() => resolve(false), TIMEOUT); })]).finally(() => clearTimeout(readyTimer));
		await client.request("prompt", { sessionId: parentSession, message: "V01_ISOLATION_PARENT_ROUTING: spawn a task; config-only isolation must apply, do not pass isolated/apply/merge flags. Let the child write the requested file and signal readiness before it completes." });
		const [evalEvent, childReady] = await Promise.all([evalEnded, childReadyFromProvider]);
		if (!evalEvent) throw new Error("task eval completion event was not observed");
		if (!childReady) throw new Error("child never signaled its requested edit before timeout");
		const parentEdit = existsSync(join(dirs.cwd, "parent-concurrent.txt")) ? readFileSync(join(dirs.cwd, "parent-concurrent.txt"), "utf8") : null;
		if (parentEdit !== PARENT_EDIT) throw new Error("parent concurrent edit was not preserved");
		const completionEvent = client.waitEvent((frame) => frame.type === "message_end" && frame.message?.role === "assistant" && JSON.stringify(frame.message).includes(PARENT_DONE));
		const childResultEvent = client.waitEvent((frame) => frame.type === "message_end" && (frame.message?.role === "custom" || frame.message?.Custom?.role === "custom") && JSON.stringify(frame.message).includes(CHILD_DONE));
		const [parentResult, childResult] = await Promise.all([completionEvent, childResultEvent]);
		const childBytes = existsSync(join(dirs.cwd, CHILD_EDIT)) ? readFileSync(join(dirs.cwd, CHILD_EDIT), "utf8") : null;
		const parentBytes = existsSync(join(dirs.cwd, "parent-concurrent.txt")) ? readFileSync(join(dirs.cwd, "parent-concurrent.txt"), "utf8") : null;
		observable = { host_ready: host.ready, parent_session_id: parentSession, config_only: true, task_eval_observed: Boolean(evalEvent), child_ready_observed: childReady,
			child_file_content: childBytes, parent_file_content: parentBytes, child_result_observed: Boolean(childResult), parent_result_observed: Boolean(parentResult), provider_requests: provider.requests.length };
		pass = Boolean(parentResult && childResult && parentEditMade && childBytes === CHILD_CONTENT && parentBytes === PARENT_EDIT);
	} catch (error) {
		observable = { ...observable, error: error.message, parent_session_id: parentSession };
	}
	const cleanupReceipt = await cleanup(host, client, dirs, provider);
	const artifacts = { provider_requests: provider.requests, host_events: client?.events ?? [], host_stdout: host?.stdout ? await host.stdout : "", host_stderr: host?.stderr ? await host.stderr : "" };
	return { status: pass && cleanupReceipt.gone.host_process && cleanupReceipt.gone.socket && cleanupReceipt.gone.temp_dir && cleanupReceipt.gone.provider_socket && cleanupReceipt.gone.owned_worktrees ? "pass" : "blocked",
		observable, artifacts, cleanup: cleanupReceipt };
}

async function main() {
	const args = parseArgs(process.argv.slice(2));
	const ctx = { binary: resolve(args.binary), install: resolve(args.install), evidence: resolve(args.evidence) };
	const qa = join(ctx.evidence, "qa"); mkdirSync(qa, { recursive: true });
	const before = fingerprint(ctx.binary);
	const scenario = await runScenario(ctx);
	const after = fingerprint(ctx.binary);
	const stable = JSON.stringify(before) === JSON.stringify(after);
	const shaValid = before?.sha256 === args.expectedSha;
	const report = { schema: SCHEMA, binary: ctx.binary, install: ctx.install, expected_sha256: args.expectedSha, binary_before: before, binary_after: after,
		binary_stable: stable, binary_sha_valid: shaValid, scenario, status: stable && shaValid && scenario.status === "pass" ? "pass" : "blocked" };
	writeFileSync(join(qa, "hg-v-isolation.json"), JSON.stringify(report, null, 2) + "\n");
	console.log(`hg-v-isolation: ${report.status} -> ${join(qa, "hg-v-isolation.json")}`);
	process.exitCode = report.status === "pass" ? 0 : 1;
}

await main();
