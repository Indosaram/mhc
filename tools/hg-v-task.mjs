#!/usr/bin/env bun
// Installed acceptance driver for V01 task/isolation/team.
// Writes <evidence>/qa/hg-v-task.json. Every mhc process is given a dedicated temporary
// HOME, MAHO_CODING_AGENT_DIR, MAHO_MEMORY_HOME, and CWD; no ambient user state is read.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, watch, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";

const SCHEMA = "hg-v-task/v1";
const REPLY = "V01_TASK_CHILD_COMPLETED_7f29";
const PARENT_DONE = "V01_PARENT_TASK_DELIVERED_ONCE_c34a";
const CHILD_NAME = "v01-acceptance-child";
const CANCEL_NAME = "v01-cancel-child";
const TIMEOUT = 45_000;
const EXPECTED_SHA = "b0088a1a4449e3b10d5747d60ecd64c77ea0fbede9538266f98243931e06c50a";

function usage(message) {
	console.error(`hg-v-task: ${message}`);
	console.error("usage: bun tools/hg-v-task.mjs --binary <mhc> --install <dir> --evidence <dir> --expected-sha256 <sha256> --scenario task-complete|task-cancel|restart-reconcile|all");
	process.exit(2);
}

function parseArgs(argv) {
	const args = { scenario: "all" };
	for (let i = 0; i < argv.length; i++) {
		const key = argv[i];
		if (key === "--binary") args.binary = argv[++i];
		else if (key === "--install") args.install = argv[++i];
		else if (key === "--evidence") args.evidence = argv[++i];
		else if (key === "--expected-sha256") args.expectedSha = argv[++i];
		else if (key === "--scenario") args.scenario = argv[++i];
		else usage(`unknown argument ${key}`);
	}
	args.expectedSha ??= EXPECTED_SHA;
	for (const key of ["binary", "install", "evidence"]) if (!args[key]) usage(`--${key} is required`);
	return args;
}

const sha256 = (data) => createHash("sha256").update(data).digest("hex");
const fingerprint = (path) => existsSync(path) ? { sha256: sha256(readFileSync(path)), mtime_ms: statSync(path).mtimeMs } : null;
const unique = (items) => [...new Set(items)];
const isolated = (label) => {
	const root = mkdtempSync(join(tmpdir(), `hg-v-task-${label}-`));
	const home = join(root, "home");
	const cwd = join(root, "cwd");
	const agent = join(root, "agent");
	const memory = join(root, "memory");
	for (const dir of [home, cwd, agent, memory]) mkdirSync(dir, { recursive: true });
	return { root, home, cwd, agent, memory };
};

function makeProvider(replyForBody) {
	const requests = [];
	const requestWaiters = new Set();
	const streamWaiters = new Set();
	const cancelWaiters = new Set();
	const cancelledStreams = [];
	const server = Bun.serve({ hostname: "127.0.0.1", port: 0, idleTimeout: 0, async fetch(request) {
		let body = "";
		try { body = await request.text(); } catch { /* provider client disconnected */ }
		let parsed = null;
		try { parsed = JSON.parse(body); } catch { /* preserve malformed request for evidence */ }
		requests.push({ raw: body, parsed });
		for (const waiter of requestWaiters) if (waiter.predicate(parsed, body)) { requestWaiters.delete(waiter); waiter.resolve({ parsed, body, index: requests.length }); }
		const completion = parsed?.messages?.some(message => message.role === "system" && String(message.content).startsWith("# Facts extractor"))
			? "No durable facts." : replyForBody(parsed, body);
		if (completion === null) {
			const requestId = `held-${requests.length}`;
			for (const waiter of streamWaiters) if (waiter.predicate(parsed, body, requestId)) { streamWaiters.delete(waiter); waiter.resolve({ requestId, parsed, body, opened: true }); }
			return new Response(new ReadableStream({ start() {}, cancel(reason) {
				cancelledStreams.push({ requestId, reason: String(reason ?? "cancelled") });
				for (const waiter of cancelWaiters) if (waiter.cancelPredicate(requestId)) { cancelWaiters.delete(waiter); waiter.resolve({ requestId, cancelled: true }); }
			} }), { headers: { "content-type": "text/event-stream" } });
		}
		const chunk = (delta, finish = null) => `data: ${JSON.stringify({ id: "offline", object: "chat.completion.chunk", created: 0, model: "offline", choices: [{ index: 0, delta, finish_reason: finish }] })}\n\n`;
		const bodyText = completion?.toolCall
			? chunk({ role: "assistant", tool_calls: [{ index: 0, id: "call_v01_task", type: "function", function: { name: completion.toolCall.name ?? "task", arguments: JSON.stringify(completion.toolCall.arguments ?? completion.toolCall) } }] }, "tool_calls") + "data: [DONE]\n\n"
			: chunk({ role: "assistant", content: completion }) + chunk({}, "stop") + "data: [DONE]\n\n";
		return new Response(bodyText, { headers: { "content-type": "text/event-stream" } });
	} });
	return { server, requests, cancelledStreams, baseUrl: `http://127.0.0.1:${server.port}/v1`, waitRequest(predicate, timeoutMs = TIMEOUT) {
		const existing = requests.find(({ parsed, raw }, index) => predicate(parsed, raw, index + 1));
		if (existing) return Promise.resolve(existing);
		return new Promise((resolvePromise) => {
			const waiter = { predicate: (parsed, raw) => predicate(parsed, raw, requests.length), resolve: resolvePromise };
			const timer = setTimeout(() => { requestWaiters.delete(waiter); resolvePromise(null); }, timeoutMs);
			const resolve = waiter.resolve;
			waiter.resolve = (value) => { clearTimeout(timer); resolve(value); };
			requestWaiters.add(waiter);
		});
	}, waitStream(predicate, timeoutMs = TIMEOUT) {
		return new Promise((resolvePromise) => {
			const waiter = { predicate, cancelPredicate: () => false, resolve: resolvePromise };
			const timer = setTimeout(() => { streamWaiters.delete(waiter); resolvePromise(null); }, timeoutMs);
			const resolve = waiter.resolve;
			waiter.resolve = (value) => { clearTimeout(timer); resolve(value); };
			streamWaiters.add(waiter);
		});
	}, waitStreamCancelled(requestId, timeoutMs = TIMEOUT) {
		const existing = cancelledStreams.find(stream => stream.requestId === requestId);
		if (existing) return Promise.resolve(existing);
		return new Promise((resolvePromise) => {
			const waiter = { predicate: () => false, cancelPredicate: id => id === requestId, resolve: resolvePromise };
			const timer = setTimeout(() => { cancelWaiters.delete(waiter); resolvePromise(null); }, timeoutMs);
			const resolve = waiter.resolve;
			waiter.resolve = (value) => { clearTimeout(timer); resolve(value); };
			cancelWaiters.add(waiter);
		});
	} };
}

function writeAgent(agent, baseUrl) {
	const provider = { api: "openai-completions", baseUrl, apiKey: "offline-fixture", models: [{ id: "offline", reasoning: false, input: ["text"], contextWindow: 128000, maxTokens: 4096 }] };
	writeFileSync(join(agent, "models.json"), JSON.stringify({ providers: { offline: provider } }));
	const configDir = join(agent, "omo-senpi", "omo-native");
	mkdirSync(configDir, { recursive: true });
	writeFileSync(join(configDir, "onboarding-completed"), "");
}

function envFor(dirs) {
	return { PATH: process.env.PATH, TMPDIR: dirs.root, HOME: dirs.home, MAHO_CODING_AGENT_DIR: dirs.agent, MAHO_MEMORY_HOME: dirs.memory,
		PI_OFFLINE: "1", SENPI_OFFLINE: "1", PI_TELEMETRY: "0", XDG_CONFIG_HOME: join(dirs.home, ".config"), XDG_CACHE_HOME: join(dirs.home, ".cache") };
}

// Bounded one-shot process runner. Its stdout and stderr are drained immediately, and exit is
// always awaited; owned processes are killed on timeout and recorded by pid.
async function run(binary, args, options = {}) {
	const proc = Bun.spawn([binary, ...args], { cwd: options.cwd, env: options.env, stdin: "ignore", stdout: "pipe", stderr: "pipe" });
	const stdoutPromise = new Response(proc.stdout).text();
	const stderrPromise = new Response(proc.stderr).text();
	let timedOut = false;
	const timer = setTimeout(() => { timedOut = true; try { process.kill(proc.pid, "SIGKILL"); } catch { /* exited */ } }, options.timeoutMs ?? TIMEOUT);
	const exitCode = await proc.exited;
	clearTimeout(timer);
	const [stdout, stderr] = await Promise.all([stdoutPromise, stderrPromise]);
	return { pid: proc.pid, exitCode: timedOut ? null : exitCode, timedOut, stdout, stderr };
}

function subscribePath(path, wanted, timeoutMs = TIMEOUT) {
	const parent = dirname(path);
	mkdirSync(parent, { recursive: true });
	const present = () => existsSync(path);
	if (present() === wanted) return Promise.resolve(true);
	return new Promise((resolvePromise) => {
		const watcher = watch(parent, () => {
			if (present() === wanted) { watcher.close(); clearTimeout(timer); resolvePromise(true); }
		});
		const timer = setTimeout(() => { watcher.close(); resolvePromise(present() === wanted); }, timeoutMs);
	});
}

function connectRpc(socketPath) {
	return new Promise((resolvePromise, reject) => {
		const client = { buffer: "", waiters: new Map(), events: [], eventWaiters: new Set(), socket: null, sequence: 1 };
		const timeout = setTimeout(() => reject(new Error("RPC socket connection timed out")), TIMEOUT);
		client.socket = Bun.connect({ unix: socketPath, socket: {
			open(socket) { clearTimeout(timeout); client.socket = socket; resolvePromise(client); },
			data(_socket, chunk) {
				client.buffer += chunk.toString();
				for (;;) {
					const i = client.buffer.indexOf("\n"); if (i < 0) break;
					const line = client.buffer.slice(0, i); client.buffer = client.buffer.slice(i + 1);
					let frame; try { frame = JSON.parse(line); } catch { continue; }
					if (frame.type === "response") {
						const waiter = client.waiters.get(String(frame.id)); if (waiter) { client.waiters.delete(String(frame.id)); waiter(frame); }
					} else {
						client.events.push(frame);
						for (const waiter of client.eventWaiters) if (waiter.predicate(frame)) { client.eventWaiters.delete(waiter); waiter.resolve(frame); }
					}
				}
			}, close() { for (const waiter of client.waiters.values()) waiter(null); client.waiters.clear(); }, error() {},
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

async function startHost(ctx, dirs, provider) {
	writeAgent(dirs.agent, provider.baseUrl);
	const socket = join(dirs.root, "host.sock");
	if (existsSync(socket)) rmSync(socket);
	const appeared = new Promise(resolveReady => {
		const watcher = watch(dirs.root, (event, filename) => {
			if (event !== "change" || String(filename) !== "host.sock" || !existsSync(socket)) return;
			watcher.close(); clearTimeout(timer); resolveReady(true);
		});
		const timer = setTimeout(() => { watcher.close(); resolveReady(false); }, TIMEOUT);
	});
	const child = Bun.spawn([ctx.binary, "--mode", "rpc", "--multi-session", "--listen", `unix://${socket}`, "--offline", "--provider", "offline", "--model", "offline"], {
		cwd: dirs.cwd, env: envFor(dirs), stdin: "ignore", stdout: "pipe", stderr: "pipe",
	});
	const stdout = new Response(child.stdout).text();
	const stderr = new Response(child.stderr).text();
	const ready = await appeared;
	const host = { child, socket, ready, stdout, stderr, client: null };
	if (ready) {
		try { host.client = await connectRpc(socket); }
		catch (error) { host.connectionError = error.message; }
	}
	return host;
}

async function cleanupHost(host, client, dirs, provider) {
	const cleanup = { pids: [], sockets: [], dirs: [], processes: [], gone: {} };
	if (host?.child) {
		cleanup.pids.push(host.child.pid);
		if (host.socket) {
			cleanup.sockets.push(host.socket);
			const socketGone = subscribePath(host.socket, false);
			try { process.kill(host.child.pid, "SIGTERM"); } catch { /* already exited */ }
			const exited = await Promise.race([host.child.exited.then(() => true), new Promise((res) => setTimeout(() => res(false), 5000))]);
			if (!exited) { try { process.kill(host.child.pid, "SIGKILL"); } catch { /* exited */ } await host.child.exited; }
			cleanup.gone.host_process = !pidAlive(host.child.pid);
			cleanup.processes.push({ role: host.role ?? "host", pid: host.child.pid, exited: host.child.exitCode !== null, gone: cleanup.gone.host_process, exitCode: host.child.exitCode });
			cleanup.gone.socket = await socketGone;
		}
		await Promise.all([host.stdout?.catch(() => ""), host.stderr?.catch(() => "")]);
	}
	for (const owned of host?.ownedProcesses ?? []) {
		cleanup.pids.push(owned.pid);
		cleanup.processes.push({ role: owned.role, pid: owned.pid, exited: owned.proc.exitCode !== null, gone: !pidAlive(owned.pid), exitCode: owned.proc.exitCode });
	}
	if (client) client.close();
	if (provider) { provider.server.stop(true); cleanup.pids.push(`provider:${provider.server.port}`); cleanup.gone.provider_socket = true; }
	if (dirs) {
		cleanup.dirs.push(dirs.root);
		rmSync(dirs.root, { recursive: true, force: true });
		cleanup.gone.temp_dir = !existsSync(dirs.root);
	}
	return cleanup;
}

function writeScenarioArtifacts(ctx, name, values) {
	const directory = join(ctx.artifactRoot, name);
	mkdirSync(directory, { recursive: true });
	const artifacts = [];
	for (const [filename, value] of Object.entries(values)) {
		const path = join(directory, filename);
		writeFileSync(path, typeof value === "string" ? value : JSON.stringify(value, null, 2) + "\n");
		artifacts.push(path);
	}
	return artifacts;
}

function waitUntil(predicate, timeoutMs = TIMEOUT) {
	const deadline = Date.now() + timeoutMs;
	return new Promise((resolvePromise) => {
		const check = () => {
			let result;
			try { result = predicate(); } catch { result = null; }
			if (result) { clearInterval(timer); resolvePromise(result); }
			else if (Date.now() >= deadline) { clearInterval(timer); resolvePromise(null); }
		};
		const timer = setInterval(check, 20);
		check();
	});
}

function pidAlive(pid) {
	try { process.kill(pid, 0); return true; }
	catch (error) { return !(error instanceof Error && "code" in error && error.code === "ESRCH"); }
}

function findToolCall(frames, toolName) {
	for (const frame of frames) {
		const candidates = [frame, frame.data, frame.message, frame.entry, frame.payload].filter(Boolean);
		for (const candidate of candidates) {
			const encoded = JSON.stringify(candidate);
			if (encoded?.includes(`"name":"${toolName}"`) || encoded?.includes(`"toolName":"${toolName}"`)) {
				const call = encoded.match(/"(?:toolName|name)":"([^"]+)"/);
				if (call?.[1] === toolName) return candidate;
			}
		}
	}
	return null;
}

function jsonAfterMarker(value, marker) {
	if (value === undefined || value === null) return null;
	const text = typeof value === "string" ? value : value.result?.content?.map(part => part.text ?? "").join("\n") ?? JSON.stringify(value);
	const at = text.indexOf(marker);
	if (at < 0) return null;
	const start = text.indexOf("{", at + marker.length);
	if (start < 0) return null;
	let depth = 0, quoted = false, escaped = false;
	for (let i = start; i < text.length; i++) {
		const ch = text[i];
		if (quoted) {
			if (escaped) escaped = false;
			else if (ch === "\\") escaped = true;
			else if (ch === '"') quoted = false;
			continue;
		}
		if (ch === '"') quoted = true;
		else if (ch === "{") depth++;
		else if (ch === "}" && --depth === 0) {
			try { return JSON.parse(text.slice(start, i + 1)); } catch { return null; }
		}
	}
	return null;
}

async function taskComplete(ctx) {
	const dirs = isolated("complete");
	let parentCallIssued = false;
	const evalCode = `const child = await tool.task({prompt:${JSON.stringify(`Return exactly ${REPLY}`)},subagent_type:"explore",task_summary:"V01 acceptance child",name:${JSON.stringify(CHILD_NAME)},model:"offline/offline",run_in_background:false}); print("CHILD_RECORD=" + JSON.stringify(await tool.task_output({name:${JSON.stringify(CHILD_NAME)},mode:"status"})));`;
	const provider = makeProvider((request) => {
		const messages = request?.messages ?? [];
		const parent = messages.some((message) => message.role === "user" && JSON.stringify(message).includes("V01_PARENT_TASK_ROUTING"));
		const child = messages.some((message) => message.role === "user" && JSON.stringify(message).includes(`Return exactly ${REPLY}`));
		if (child && !parent) return REPLY;
		if (parent && !parentCallIssued) { parentCallIssued = true; return { toolCall: { name: "eval", arguments: { language: "js", code: evalCode, summary: "Run deterministic task completion probe", on_timeout: "error", timeout: 40 } } }; }
		return PARENT_DONE;
	});
	let host = null; let client = null; let pass = false; let observable = {}; let providerArtifact = [];
	try {
		host = await startHost(ctx, dirs, provider); client = host.client;
		if (!host.ready || !client) throw new Error("RPC host did not become ready");
		const opened = await client.request("open_session", { cwd: dirs.cwd });
		const sessionId = opened?.data?.sessionId;
		if (!sessionId) throw new Error(`open_session returned no sessionId: ${JSON.stringify(opened)}`);
		const completion = client.waitEvent((frame) => frame.type === "message_end" && frame.message?.role === "assistant" && JSON.stringify(frame.message).includes(PARENT_DONE));
		const toolObservedPromise = client.waitEvent(frame => frame.type === "tool_execution_end" && JSON.stringify(frame).includes("CHILD_RECORD="));
		await client.request("prompt", { sessionId, message: "V01_PARENT_TASK_ROUTING: execute the eval tool exactly as supplied and return its output." });
		const [toolObserved, parentEvent] = await Promise.all([toolObservedPromise, completion]);
		const transcript = client.events.map((frame) => JSON.stringify(frame)).join("\n");
		const toolCount = client.events.filter((frame) => frame.type === "tool_execution_end" && JSON.stringify(frame).includes("CHILD_RECORD=")).length;
		const recordFrame = client.events.find(frame => frame.type === "tool_execution_end" && JSON.stringify(frame).includes("CHILD_RECORD="));
		const taskRecord = jsonAfterMarker(recordFrame, "CHILD_RECORD=");
		const finalResponse = taskRecord?.details?.snapshot?.final_response ?? taskRecord?.record?.final_response ?? taskRecord?.final_response ?? taskRecord?.details?.record?.final_response ?? null;
		const recordStatus = taskRecord?.details?.snapshot?.status ?? taskRecord?.record?.status ?? taskRecord?.status ?? taskRecord?.details?.record?.status ?? null;
		const childDelivered = recordStatus === "completed" && finalResponse === REPLY;
		const parentDelivered = Boolean(parentEvent);
		const childRequests = provider.requests.filter(({ parsed }) => (parsed?.messages ?? []).some((message) => JSON.stringify(message).includes(`Return exactly ${REPLY}`)));
		observable = { host_ready: host.ready, session_id: sessionId, task_record_query_observed: Boolean(toolObserved), completed_record_count: recordStatus === "completed" ? 1 : 0, task_record_status: recordStatus, final_response: finalResponse, parent_result_observed: parentDelivered, parent_result_occurrences: client.events.filter(frame => frame.type === "message_end" && frame.message?.role === "assistant" && JSON.stringify(frame.message).includes(PARENT_DONE)).length, child_provider_request_count: childRequests.length, eval_invoked: Boolean(toolObserved), eval_kernel_exit_event: Boolean(toolObserved) };
		pass = Boolean(toolObserved && childDelivered && parentDelivered && observable.parent_result_occurrences === 1 && childRequests.length >= 1);
		observable.task_record = taskRecord;
	} catch (error) { observable.error = error.message; }
	providerArtifact = provider.requests;
	const cleanup = await cleanupHost(host, client, dirs, provider);
	const artifacts = writeScenarioArtifacts(ctx, "task-complete", { "loopback-requests.json": providerArtifact, "host-events.json": client?.events ?? [], "host-stdout.log": host?.stdout ? await host.stdout : "", "host-stderr.log": host?.stderr ? await host.stderr : "", "cleanup.json": cleanup });
	return { status: pass && cleanup.gone.host_process && cleanup.gone.socket && cleanup.gone.temp_dir ? "pass" : "blocked", invocation: `${ctx.binary} --mode rpc --multi-session --listen unix://${join(dirs.root, "host.sock")} --offline --provider offline --model offline; open_session + prompt(eval -> tool.task/task_output)`, observable, artifacts, cleanup };
}

async function taskCancel(ctx) {
	const dirs = isolated("cancel");
	const heldPrompt = "V01_HELD_CHILD_PROVIDER_REQUEST_5aa1";
	const childResult = "V01_CANCEL_CHILD_MUST_NOT_DELIVER_58cd";
	let releaseHeldResponse = false;
	let cancelIssued = false;
	let spawnIssued = false;
	const provider = makeProvider((request) => {
		const messages = request?.messages ?? [];
		if (!spawnIssued && !cancelIssued && messages.some(message => message.role === "user" && JSON.stringify(message).includes("V01_CANCEL_PARENT_ROUTING") && !JSON.stringify(message).includes("CANCEL_NOW"))) {
			spawnIssued = true;
			return { toolCall: { name: "eval", arguments: { language: "js", code: `const id = await tool.task({prompt:${JSON.stringify(heldPrompt)},subagent_type:"explore",task_summary:"V01 cancellation child",name:${JSON.stringify(CANCEL_NAME)},model:"offline/offline",run_in_background:true}); print("CANCEL_TASK_STARTED=" + JSON.stringify(id));`, summary: "Start held cancellation child", on_timeout: "error", timeout: 40 } } };
		}
		if (!cancelIssued && messages.some(message => message.role === "user" && JSON.stringify(message).includes("V01_CANCEL_PARENT_ROUTING") && JSON.stringify(message).includes("CANCEL_NOW"))) { cancelIssued = true; return { toolCall: { name: "eval", arguments: { language: "js", code: `print("CANCEL_RESULT=" + JSON.stringify(await tool.task_cancel({name:${JSON.stringify(CANCEL_NAME)},reason:"V01 acceptance cancellation"}))); print("FINAL_RECORD=" + JSON.stringify(await tool.task_output({name:${JSON.stringify(CANCEL_NAME)},mode:"status"})));`, summary: "Cancel and inspect child task", on_timeout: "error", timeout: 30 } } }; }
		if (messages.some(message => message.role === "user" && JSON.stringify(message).includes(heldPrompt))) {
			return releaseHeldResponse ? childResult : null;
		}
		return "V01_CANCEL_PARENT_ACK";
	});
	let host = null, client = null, pass = false, observable = {}, events = [];
	try {
		host = await startHost(ctx, dirs, provider); client = host.client;
		if (!client) throw new Error("RPC host did not become ready");
		const opened = await client.request("open_session", { cwd: dirs.cwd });
		const sessionId = opened?.data?.sessionId;
		if (!sessionId) throw new Error("open_session returned no sessionId");
		const heldStreamPromise = provider.waitStream((_parsed, body) => body.includes(heldPrompt));
		const spawnEvent = client.waitEvent(frame => frame.type === "tool_execution_end" && JSON.stringify(frame).includes("CANCEL_TASK_STARTED="));
		const parentIdle = client.waitEvent(frame => frame.type === "agent_end");
		await client.request("prompt", { sessionId, message: "V01_CANCEL_PARENT_ROUTING: use eval to start exactly the cancellation child." });
		const spawned = await spawnEvent;
		const heldRequest = await provider.waitRequest((parsed, body) => body.includes(heldPrompt) || JSON.stringify(parsed?.messages ?? []).includes(heldPrompt));
		if (!spawned || !heldRequest) throw new Error("task child did not reach the held provider request before cancellation");
		const heldStream = await heldStreamPromise;
		if (!heldStream) throw new Error("held child SSE response stream did not open before cancellation");
		if (!await parentIdle) throw new Error("parent turn did not settle before cancellation prompt");
		const streamCancelledPromise = provider.waitStreamCancelled(heldStream.requestId);
		const childPidEvidence = client.events.find(frame => JSON.stringify(frame).includes("host_pid")) ?? { execution_mode: "in-process", owning_host_pid: host.child.pid };
		const cancelEvent = client.waitEvent(frame => frame.type === "tool_execution_end" && JSON.stringify(frame).includes("FINAL_RECORD="));
		const evalExitEvent = client.waitEvent(frame => frame.type === "tool_execution_end" && JSON.stringify(frame).includes("CANCEL_RESULT="));
		const cancelCode = `print("CANCEL_RESULT=" + JSON.stringify(await tool.task_cancel({name:${JSON.stringify(CANCEL_NAME)},reason:"V01 acceptance cancellation"}))); print("FINAL_RECORD=" + JSON.stringify(await tool.task_output({name:${JSON.stringify(CANCEL_NAME)},mode:"status"})));`;
		await client.request("prompt", { sessionId, message: `V01_CANCEL_PARENT_ROUTING CANCEL_NOW: use eval with code ${cancelCode}` });
		const [result, evalExit] = await Promise.all([cancelEvent, evalExitEvent]);
		const settledText = JSON.stringify(result ?? {});
		const record = jsonAfterMarker(result, "FINAL_RECORD=");
		const status = record?.details?.snapshot?.status ?? record?.record?.status ?? record?.status ?? record?.details?.record?.status;
		releaseHeldResponse = true;
		const streamCancelled = await streamCancelledPromise;
		const childRequestAfterCancel = provider.requests.some(({ parsed, raw }) => raw.includes(heldPrompt) || JSON.stringify(parsed?.messages ?? []).includes(heldPrompt));
		const childResultDelivered = client.events.some(frame => JSON.stringify(frame).includes(childResult));
		const postCancelEvents = client.events;
		events = client.events;
		observable = { host_ready: host.ready, session_id: sessionId, child_spawn_observed: Boolean(spawned), held_child_provider_request_observed_before_cancel: Boolean(heldRequest), held_child_sse_open_before_cancel: Boolean(heldStream?.opened), task_cancel_response_observed: settledText.includes("CANCEL_RESULT="), task_record_status: status ?? null, child_result_delivered: childResultDelivered, child_provider_request_received: childRequestAfterCancel, held_stream_cancelled_after_cancel: Boolean(streamCancelled), cancel_record: record, child_pid_evidence: childPidEvidence, eval_kernel_exit_event: Boolean(evalExit), post_cancel_event_count: postCancelEvents.length };
		pass = Boolean(spawned && heldRequest && heldStream?.opened && evalExit && settledText.includes("CANCEL_RESULT=") && status === "cancelled" && !childResultDelivered && streamCancelled);
	} catch (error) { observable.error = error.message; events = client?.events ?? []; }
	releaseHeldResponse = true;
	const cleanup = await cleanupHost(host, client, dirs, provider);
	const artifacts = writeScenarioArtifacts(ctx, "task-cancel", { "loopback-requests.json": provider.requests, "cancelled-streams.json": provider.cancelledStreams, "host-events.json": events, "host-stdout.log": host?.stdout ? await host.stdout : "", "host-stderr.log": host?.stderr ? await host.stderr : "", "cleanup.json": cleanup });
	return { status: pass && cleanup.gone.host_process && cleanup.gone.socket && cleanup.gone.temp_dir ? "pass" : "blocked", invocation: `${ctx.binary} --mode rpc --multi-session --listen unix://${join(dirs.root, "host.sock")} --offline --provider offline --model offline; eval -> tool.task, tool.task_cancel, tool.task_output`, observable, artifacts, cleanup };
}

async function restartReconcile(ctx) {
	const dirs = isolated("restart");
	let spawnIssued = false;
	let inspectIssued = false;
	const provider = makeProvider((request) => {
		const messages = request?.messages ?? [];
		if (!inspectIssued && messages.some((message) => message.role === "user" && JSON.stringify(message).includes("V01_RESTART_PARENT_ROUTING") && JSON.stringify(message).includes("AFTER_RESTART"))) { inspectIssued = true; return { toolCall: { name: "eval", arguments: { language: "js", code: `const result = await tool.task_output({name:${JSON.stringify(CHILD_NAME)},mode:"status"}); print("V01_RESTART_TASK_OUTPUT=" + JSON.stringify(result));`, summary: "Inspect task after host restart", on_timeout: "error", timeout: 20 } } }; }
		if (!spawnIssued && messages.some((message) => message.role === "user" && JSON.stringify(message).includes("V01_RESTART_PARENT_ROUTING"))) { spawnIssued = true; return { toolCall: { name: "eval", arguments: { language: "js", code: `await tool.task({prompt:"Wait for host restart",subagent_type:"explore",task_summary:"V01 restart child",name:${JSON.stringify(CHILD_NAME)},model:"offline/offline",run_in_background:true}); print("V01_RESTART_CHILD spawned");`, summary: "Spawn task before host restart", on_timeout: "error", timeout: 30 } } }; }
		if (messages.some((message) => message.role === "user" && JSON.stringify(message).includes("Wait for host restart"))) return null;
		return "V01_RESTART_PARENT_READY";
	});
	let host = null, client = null, firstEvents = [], firstHost = null;
	const ownedHosts = [];
	const invocation = `${ctx.binary} --mode rpc --multi-session --listen unix://${join(dirs.root, "host.sock")} --offline --provider offline --model offline; spawn child, SIGKILL host, restart same HOME, eval -> tool.task_output`;
	let observable = { attempted: true, persisted_task_output: false };
	try {
		host = await startHost(ctx, dirs, provider); client = host.client; ownedHosts.push(host);
		if (!client) throw new Error("first RPC host did not become ready");
		const opened = await client.request("open_session", { cwd: dirs.cwd });
		const sessionId = opened?.data?.sessionId;
		if (!sessionId) throw new Error("open_session returned no sessionId");
		const spawnEvent = client.waitEvent((frame) => frame.type === "tool_execution_end" && JSON.stringify(frame).includes("V01_RESTART_CHILD spawned"));
		await client.request("prompt", { sessionId, message: "V01_RESTART_PARENT_ROUTING: spawn V01_RESTART_CHILD via eval with offline/offline model, then remain blocked." });
		const spawned = await spawnEvent;
		if (!spawned) throw new Error("restart child spawn was not observed");
		const listed = await client.request("list_sessions");
		const parentPath = listed?.data?.sessions?.find(session => session.sessionId === sessionId)?.sessionPath;
		if (!parentPath) throw new Error("parent sessionPath unavailable for restart ownership preservation");
		firstEvents = client.events;
		firstHost = host;
		firstHost.role = "first-host-killed-mid-task";
		try { process.kill(host.child.pid, "SIGKILL"); } catch {}
		await host.child.exited;
		firstHost.killedByScenario = true;
		host = null; client?.close(); client = null;
		host = await startHost(ctx, dirs, provider); client = host.client; ownedHosts.push(host);
		if (!client) throw new Error("restarted RPC host did not become ready");
		const requestsBeforeResume = provider.requests.length;
		const next = await client.request("open_session", { cwd: dirs.cwd, sessionPath: parentPath });
		if (!next?.data?.sessionId) throw new Error("restarted host session unavailable");
		const outputEvent = client.waitEvent((frame) => frame.type === "tool_execution_end" && JSON.stringify(frame).includes("V01_RESTART_TASK_OUTPUT="));
		await client.request("prompt", { sessionId: next.data.sessionId, message: "V01_RESTART_PARENT_ROUTING AFTER_RESTART: invoke eval to inspect persisted task_output." });
		const output = await outputEvent;
		const snapshots = [];
		const inspect = value => {
			if (typeof value === "string") {
				const marker = "V01_RESTART_TASK_OUTPUT=";
				const start = value.indexOf(marker);
				if (start >= 0) { try { inspect(JSON.parse(value.slice(start + marker.length).trim())); } catch {} }
			} else if (value && typeof value === "object") {
				if (value.task_id && value.name === CHILD_NAME && value.parent_session_id && value.status) snapshots.push(value);
				for (const nested of Object.values(value)) inspect(nested);
			}
		};
		inspect(output);
		const lost = snapshots.find(snapshot => snapshot.status === "lost" && snapshot.lost?.explanation && snapshot.lost?.session_dir);
		const childRequestedAfterResume = provider.requests.slice(requestsBeforeResume).some(request => (request.parsed?.messages ?? []).some(message => message.role === "user" && JSON.stringify(message).includes("Wait for host restart")) && !(request.parsed?.messages ?? []).some(message => message.role === "user" && JSON.stringify(message).includes("V01_RESTART_PARENT_ROUTING")));
		const resumed = snapshots.find(snapshot => snapshot.status === "running" && snapshot.residency_state === "resident" && childRequestedAfterResume && JSON.stringify(firstEvents).includes(snapshot.task_id) && JSON.stringify(firstEvents).includes(snapshot.parent_session_id));
		const taskDeliveries = [...firstEvents, ...client.events].filter(frame => frame.type === "tool_execution_end" && JSON.stringify(frame).includes("V01_RESTART_TASK_OUTPUT=") && JSON.stringify(frame).includes(REPLY));
		const restartEvalExit = client.events.some(frame => frame.type === "tool_execution_end" && JSON.stringify(frame).includes("V01_RESTART_TASK_OUTPUT="));
		observable = { attempted: true, first_host_pid: firstHost?.child.pid ?? null, first_host_killed: Boolean(firstHost?.killedByScenario), restarted_host_pid: host.child.pid, restarted_host_ready: host.ready, persisted_task_output: Boolean(lost || resumed), reconciliation_kind: resumed ? "scoped-revival" : lost ? "lost" : null, reconciled_snapshot: resumed ?? lost ?? null, task_output_event: output, delivery_occurrences_across_hosts: taskDeliveries.length, no_double_delivery: taskDeliveries.length === 0, eval_kernel_exit_event: restartEvalExit };
	} catch (error) { observable = { ...observable, error: error.message, connectionError: host?.connectionError ?? null, first_host_pid: firstHost?.child.pid ?? null, first_host_killed: Boolean(firstHost && firstHost.child.exitCode !== null) }; }
	if (host) host.ownedProcesses = ownedHosts.filter(owned => owned !== host).map(owned => ({ role: owned.role ?? "first-host-killed-mid-task", pid: owned.child.pid, proc: owned.child }));
	const cleanup = await cleanupHost(host, client, dirs, provider);
	const artifacts = writeScenarioArtifacts(ctx, "restart-reconcile", { "loopback-requests.json": provider.requests, "first-host-events.json": firstEvents, "restarted-host-events.json": client?.events ?? [], "first-host-stdout.log": firstHost?.stdout ? await firstHost.stdout : "", "first-host-stderr.log": firstHost?.stderr ? await firstHost.stderr : "", "restarted-host-stdout.log": host?.stdout ? await host.stdout : "", "restarted-host-stderr.log": host?.stderr ? await host.stderr : "", "owned-hosts.json": ownedHosts.map(owned => ({ pid: owned.child.pid, role: owned.role ?? "host", exitCode: owned.child.exitCode, killedByScenario: Boolean(owned.killedByScenario), gone: !pidAlive(owned.child.pid) })), "cleanup.json": cleanup });
	return { status: observable.persisted_task_output && observable.no_double_delivery && cleanup.gone.host_process && cleanup.gone.socket && cleanup.gone.temp_dir && cleanup.processes.every(process => process.gone) ? "pass" : "blocked", invocation, observable: { ...observable, blocked_reason: observable.persisted_task_output ? null : "restart did not expose the owned child as lost with explanation and session-directory breadcrumbs" }, artifacts, cleanup };
}

async function main() {
	const args = parseArgs(process.argv.slice(2));
	const ctx = { binary: resolve(args.binary), install: resolve(args.install), evidence: resolve(args.evidence), artifactRoot: join(resolve(args.evidence), "qa", "hg-v-task-artifacts") };
	const qa = join(ctx.evidence, "qa"); mkdirSync(qa, { recursive: true });
	const binaryBefore = fingerprint(ctx.binary);
	const observedSha = binaryBefore?.sha256 ?? null;
	const stagedShaValid = observedSha === args.expectedSha;
	const names = args.scenario === "all" ? ["task-complete", "task-cancel", "restart-reconcile"] : [args.scenario];
	for (const name of names) if (!["task-complete", "task-cancel", "restart-reconcile"].includes(name)) usage(`unknown scenario ${name}`);
	const scenarios = {};
	for (const name of names) {
		console.log(`hg-v-task: scenario ${name}`);
		if (name === "task-complete") scenarios[name] = await taskComplete(ctx);
		else if (name === "task-cancel") scenarios[name] = await taskCancel(ctx);
		else scenarios[name] = await restartReconcile(ctx);
	}
	const binaryAfter = fingerprint(ctx.binary);
	const binaryStable = JSON.stringify(binaryBefore) === JSON.stringify(binaryAfter);
	const report = { schema: SCHEMA, binary: ctx.binary, install: ctx.install, expected_sha256: args.expectedSha, binary_before: binaryBefore, binary_after: binaryAfter, binary_stable: binaryStable, binary_sha_valid: stagedShaValid, scenarios, status: binaryStable && stagedShaValid && Object.values(scenarios).every((scenario) => scenario.status === "pass") ? "pass" : "blocked" };
	writeFileSync(join(qa, "hg-v-task.json"), JSON.stringify(report, null, 2) + "\n");
	console.log(`hg-v-task: ${report.status} -> ${join(qa, "hg-v-task.json")}`);
	process.exitCode = report.status === "pass" ? 0 : 1;
}

await main();
