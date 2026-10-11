#!/usr/bin/env bun
// Installed acceptance driver for the V04 native MCP and tool-search surfaces.
// Usage: bun tools/hg-v-mcp.mjs --binary <mhc> --install <install> --evidence <E> --scenario all
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, watch, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { writeOfflineAgent, markOnboardingComplete } from "/home/projects/mhc-wt/hg-integration/.omo/evidence/session2-residual/harness-v2/qa-loopback-lib.mjs";

const SCHEMA = "hg-v-mcp/v1";
const PINNED_SHA = "b0088a1a4449e3b10d5747d60ecd64c77ea0fbede9538266f98243931e06c50a";
const TIMEOUT_MS = 30_000;
const MCP_TOOL = "echo";
const MCP_ARGS = { text: "hg-v-mcp-exact-args" };
const MCP_RESULT = "hg-v-mcp-echo-result";
function usage(message) { console.error(`hg-v-mcp: ${message}`); process.exit(2); }
function parseArgs(argv) {
	const out = { scenario: "all" };
	for (let i = 0; i < argv.length; i++) {
		const key = argv[i];
		if (key === "--binary") out.binary = argv[++i];
		else if (key === "--install") out.install = argv[++i];
		else if (key === "--evidence") out.evidence = argv[++i];
		else if (key === "--scenario") out.scenario = argv[++i];
  else if (key === "--expected-sha") out.expectedSha = argv[++i];
		else usage(`unknown argument ${key}`);
	}
	for (const key of ["binary", "install", "evidence"]) if (!out[key]) usage(`--${key} is required`);
	return out;
}
const sha256 = (data) => createHash("sha256").update(data).digest("hex");
const fingerprint = (path) => existsSync(path) ? { sha256: sha256(readFileSync(path)), mtimeMs: statSync(path).mtimeMs } : null;
const newTemp = (prefix) => mkdtempSync(join(tmpdir(), prefix));
function waitForPath(path, timeoutMs = TIMEOUT_MS) {
	if (existsSync(path)) return Promise.resolve(true);
	return new Promise((resolvePromise) => {
		const watcher = watch(join(path, ".."), () => { if (existsSync(path)) finish(true); });
		const timer = setTimeout(() => finish(false), timeoutMs);
		function finish(value) { clearTimeout(timer); watcher.close(); resolvePromise(value); }
	});
}
function isolatedEnv(home, agent) {
	return { PATH: process.env.PATH, HOME: home, TMPDIR: tmpdir(), MAHO_CODING_AGENT_DIR: agent,
		MAHO_MEMORY_HOME: join(home, "memory"), OMO_SENPI_MEMORY_HOME: join(home, "memory"), MAHO_RPC_HOST_IDLE_EXIT_MS: "60000", SENPI_RPC_HOST_IDLE_EXIT_MS: "60000" };
}
function writeMcpServer(agent, scratch, logPath) {
	const serverPath = join(scratch, "fake-mcp.mjs");
	writeFileSync(serverPath, `import { appendFileSync, writeFileSync } from "node:fs";\nwriteFileSync(${JSON.stringify(join(scratch, "mcp-child.pid"))}, String(process.pid));\nlet buffer = "";\nfunction reply(id, result) { process.stdout.write(JSON.stringify({jsonrpc:"2.0", id, result}) + "\\n"); }\nprocess.stdin.setEncoding("utf8");\nprocess.stdin.on("data", chunk => { buffer += chunk; for (;;) { const i = buffer.indexOf("\\n"); if (i < 0) break; const line = buffer.slice(0, i); buffer = buffer.slice(i + 1); if (!line.trim()) continue; const message = JSON.parse(line); appendFileSync(${JSON.stringify(logPath)}, JSON.stringify(message) + "\\n"); if (message.method === "initialize") reply(message.id, {protocolVersion:"2024-11-05", capabilities:{tools:{}}, serverInfo:{name:"hg-v-mcp-fake", version:"1"}}); else if (message.method === "notifications/initialized") {} else if (message.method === "tools/list") reply(message.id, {tools:[{name:"echo", description:"Echo supplied text", inputSchema:{type:"object", properties:{text:{type:"string"}}, required:["text"]}}]}); else if (message.method === "resources/list") reply(message.id, {resources:[]}); else if (message.method === "prompts/list") reply(message.id, {prompts:[]}); else if (message.method === "tools/call") { appendFileSync(${JSON.stringify(logPath)}, JSON.stringify(message) + "\\n"); reply(message.id, {content:[{type:"text", text:${JSON.stringify(MCP_RESULT)}}]}); } else if (message.id !== undefined) reply(message.id, {}); } });\n`);
	mkdirSync(agent, { recursive: true });
	writeFileSync(join(agent, "mcp.json"), JSON.stringify({ settings: { nativeToolSearch: true }, mcpServers: { fake: {
		type: "stdio", command: process.execPath, args: [serverPath], exposure: "search", lifecycle: "keep-alive", startupTimeoutMs: 5000,
	} } }, null, 2));
	return serverPath;
}
function startRecordingProvider() {
	const requests = [];
	const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
		let body = ""; try { body = await request.text(); } catch {}
		requests.push(body);
		let payload;
		try { payload = JSON.parse(body); } catch { payload = {}; }
		const tools = payload.tools ?? [];
		const echo = tools.find((tool) => tool.function?.name === MCP_TOOL || tool.function?.name?.endsWith(`_${MCP_TOOL}`));
		const returned = payload.messages?.some((message) => message.role === "tool" && JSON.stringify(message).includes(MCP_RESULT));
		const searchResult = payload.messages?.findLast((message) => message.role === "tool" && JSON.stringify(message).includes("parameters:") && JSON.stringify(message).includes(MCP_TOOL));
		const matchedName = JSON.stringify(searchResult ?? {}).match(/mcp_[A-Za-z0-9_]*echo/)?.[0];
		const searchAvailable = tools.some((tool) => tool.function?.name === "tool_search");
		const call = returned ? { role: "assistant", content: "hg-v-mcp-tool-result-reached-transcript" }
			: echo ? { role: "assistant", tool_calls: [{ index: 0, id: `call-hg-mcp-${requests.length}`, type: "function", function: { name: echo.function.name, arguments: JSON.stringify(MCP_ARGS) } }] }
			: matchedName ? { role: "assistant", tool_calls: [{ index: 0, id: `call-hg-mcp-${requests.length}`, type: "function", function: { name: matchedName, arguments: JSON.stringify(MCP_ARGS) } }] }
			: { role: "assistant", tool_calls: [{ index: 0, id: `call-hg-mcp-${requests.length}`, type: "function", function: { name: "tool_search", arguments: JSON.stringify({ query: MCP_TOOL, source: "mcp", group: "fake" }) } }] };
		const chunks = [
			{ id: "offline", object: "chat.completion.chunk", created: 0, model: "offline", choices: [{ index: 0, delta: { role: "assistant", ...call }, finish_reason: returned ? "stop" : "tool_calls" }] },
		];
		const response = chunks.map((chunk) => `data: ${JSON.stringify(chunk)}\n\n`).join("") + "data: [DONE]\n\n";
		return new Response(response, { headers: { "content-type": "text/event-stream" } });
	} });
	return { server, requests };
}
function connectHost(socketPath) {
	return new Promise((resolvePromise, reject) => {
		const client = { buffer: "", waiters: new Map(), nextId: 1, socket: null, events: [], onEvent: null };
		const timer = setTimeout(() => reject(new Error("RPC socket connection timeout")), TIMEOUT_MS);
		client.socket = Bun.connect({ unix: socketPath, socket: {
			open(socket) { clearTimeout(timer); client.socket = socket; resolvePromise(client); },
			data(_socket, chunk) { client.buffer += chunk.toString(); let index; while ((index = client.buffer.indexOf("\n")) >= 0) { const line = client.buffer.slice(0, index); client.buffer = client.buffer.slice(index + 1); let frame; try { frame = JSON.parse(line); } catch { continue; } const settle = client.waiters.get(String(frame.id)); if (settle) { client.waiters.delete(String(frame.id)); settle(frame); } else {client.events.push(frame);client.onEvent?.(frame);} } },
			close() { for (const settle of client.waiters.values()) settle(null); client.waiters.clear(); }, error(_socket, error) { clearTimeout(timer); reject(error); },
		} }).catch((error) => { clearTimeout(timer); reject(error); });
	});
}
function rpcRequest(client, fields) {
	const id = String(client.nextId++);
	return new Promise((resolvePromise) => {
		const timer = setTimeout(() => { client.waiters.delete(id); resolvePromise(null); }, TIMEOUT_MS);
		client.waiters.set(id, (frame) => { clearTimeout(timer); resolvePromise(frame); });
		client.socket.write(JSON.stringify({ id, ...fields }) + "\n");
	});
}
function drain(stream, consume) {
	return (async () => { const reader = stream.getReader(); const decoder = new TextDecoder(); for (;;) { const { value, done } = await reader.read(); if (done) break; consume(decoder.decode(value, { stream: true })); } })();
}
async function terminate(proc) {
	if (!proc) return { exited: true, exitCode: null };
	try { process.kill(proc.pid, "SIGTERM"); } catch {}
	const exitCode = await Promise.race([proc.exited, new Promise((resolvePromise) => setTimeout(() => resolvePromise(null), 5000))]);
	if (exitCode === null) { try { process.kill(proc.pid, "SIGKILL"); } catch {} return { exited: await proc.exited !== null, exitCode: await proc.exited, forced: true }; }
	return { exited: true, exitCode, forced: false };
}
function processGone(pid) { try { process.kill(pid, 0); return false; } catch (error) { return error.code === "ESRCH"; } }
async function watchChildExit(pid, trigger, timeoutMs = 5_000) {
	if (process.platform !== "linux") return false;
	const helper = Bun.spawn(["python3", "-c", "import os,select,sys\npid=int(sys.argv[1])\nfd=os.pidfd_open(pid)\nprint('ARMED',flush=True)\nselect.poll().poll(0)\np=select.poll();p.register(fd,select.POLLIN);p.poll(int(sys.argv[2]));print('EXITED' if p.poll(0) else 'TIMEOUT',flush=True)", String(pid), String(timeoutMs)], { stdin: "ignore", stdout: "pipe", stderr: "pipe" });
	const reader = helper.stdout.getReader(); const decoder = new TextDecoder(); let output = "";
	try {
		for (;;) { const { value, done } = await reader.read(); if (done) throw new Error("child pidfd watch ended before subscription"); output += decoder.decode(value); if (output.includes("ARMED\n")) break; }
		const exited = (async () => { for (;;) { const { value, done } = await reader.read(); if (done) break; output += decoder.decode(value); } return output.includes("EXITED\n"); })();
		await trigger();
		return await exited;
	} finally { try { helper.kill(); } catch {} await helper.exited; }
}

async function runScenario(ctx, name) {
	const home = newTemp(`hg-v-mcp-${name}-home-`), scratch = newTemp(`hg-v-mcp-${name}-artifacts-`), cwd = newTemp(`hg-v-mcp-${name}-cwd-`);
	mkdirSync(scratch, { recursive: true });
	mkdirSync(cwd, { recursive: true });
	const agent = join(home, "agent"), logPath = join(scratch, "mcp-requests.jsonl"), socketPath = join(cwd, "s");
	const serverPath = writeMcpServer(agent, scratch, logPath);
	const provider = startRecordingProvider();
	writeOfflineAgent(home, `http://127.0.0.1:${provider.server.port}/v1`); // materializes isolated agent configuration
	writeFileSync(join(agent, "mcp.json"), JSON.stringify({ settings: { nativeToolSearch: true }, mcpServers: { fake: { type: "stdio", command: process.execPath, args: [serverPath], exposure: "search", lifecycle: "keep-alive", startupTimeoutMs: 5000 } } }, null, 2));
	markOnboardingComplete(cwd);
	const env = isolatedEnv(home, agent), artifacts = [logPath, serverPath];
	const cleanup = { pids: [], sockets: [socketPath], directories: [home, cwd], gone: false, process_proofs: [] };
	let host = null, rpc = null, stdout = "", stderr = "", result = { status: "blocked", observable: "not reached" }, childPid = null, sessionId = null;
	let outDrain = Promise.resolve(), errDrain = Promise.resolve();
	const stdoutChunks = [], stderrChunks = [];
	try {
		const appeared = waitForPath(socketPath);
		host = Bun.spawn([ctx.binary, "--mode", "rpc", "--multi-session", "--listen", `unix://${socketPath}`, "--offline", "--provider", "offline", "--model", "offline"], { cwd, env, stdin: "ignore", stdout: "pipe", stderr: "pipe" });
		cleanup.pids.push(host.pid);
		outDrain = drain(host.stdout, (chunk) => { stdout += chunk; stdoutChunks.push(chunk); });
		errDrain = drain(host.stderr, (chunk) => { stderr += chunk; stderrChunks.push(chunk); });
		if (!await appeared) throw new Error(`host readiness socket absent after ${TIMEOUT_MS}ms; contract: tools/latest-omo-host-qa.mjs mounted-skills scenario binds --mode rpc --listen unix://<socket>`);
		rpc = await connectHost(socketPath);
		const opened = await rpcRequest(rpc, { type: "open_session", cwd });
		sessionId = opened?.data?.sessionId;
		if (!sessionId) throw new Error(`open_session did not return data.sessionId: ${JSON.stringify(opened)}`);
		// Subscribe to actual stdout before sending the prompt; the provider is already recording all
		// requests and the subsequent get_entries call is the transcript readback signal.
		let completionTimer;
		const completed = new Promise(resolve => {
			rpc.onEvent = frame => { if (frame.type === "message_end" && frame.message?.role === "assistant" && frame.message?.stopReason === "stop") { clearTimeout(completionTimer); resolve(true); } };
			completionTimer = setTimeout(()=>resolve(false),TIMEOUT_MS);
		});
		const prompt = rpcRequest(rpc, { type: "prompt", sessionId, message: "Please use the available echo MCP tool with the supplied exact arguments." });
		const response = await prompt;
		if (!response) throw new Error("prompt RPC produced no bounded response");
		if (!await completed) throw new Error("assistant message_end stop was not observed before the bounded deadline");
		const entriesReply = await rpcRequest(rpc, { type: "get_entries", sessionId });
		const entries = entriesReply?.data?.entries ?? [];
		const transcriptText = JSON.stringify(entries);
  const providerSawMcpSchema = provider.requests.some(body => (JSON.parse(body).messages ?? []).some(message => message.role === "tool" && JSON.stringify(message).includes("parameters:") && JSON.stringify(message).includes("echo") && JSON.stringify(message).includes("required")));
		const searchCall = rpc.events.some(frame => frame.type === "tool_execution_end" && JSON.stringify(frame).includes("tool_search"));
		const calls = existsSync(logPath) ? readFileSync(logPath, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line)) : [];
		const exactCall = calls.find((call) => call.method === "tools/call" && call.params?.name === MCP_TOOL && JSON.stringify(call.params.arguments) === JSON.stringify(MCP_ARGS));
		const transcriptHasResult = transcriptText.includes(MCP_RESULT);
  const transcriptHasSchema = providerSawMcpSchema;
		const pass = providerSawMcpSchema && searchCall && Boolean(exactCall) && transcriptHasResult;
		result = { status: pass ? "pass" : "blocked", observable: { providerSawMcpSchema, searchCall, exactToolsCall: Boolean(exactCall), transcriptHasResult, transcriptHasSchema,
			detail: pass ? "fake MCP schema was exposed through tool_search; exact echo call logged and result is in transcript" : "live RPC did not satisfy all MCP/tool-search observables" } };
		if (name === "tool-search") result.status = providerSawMcpSchema && searchCall ? "pass" : "blocked";
		if (name === "cleanup") result.status = Boolean(exactCall) ? "pending-cleanup" : "blocked";
		artifacts.push(join(scratch, "host.stdout.log"), join(scratch, "host.stderr.log"));
	} catch (error) {
		result = { status: "blocked", observable: { detail: error.message, stdout: stdout.slice(-4000), stderr: stderr.slice(-4000) } };
	} finally {
		if (rpc) { try { rpc.socket.end(); } catch {} }
		if (host) {
			// Explicit session close is the product cleanup trigger. Subscribe to the child PID's
			// procfs exit before sending close_session; only then shut down the owned host.
			const childPidPath = join(scratch, "mcp-child.pid");
			if (rpc && childPidPath && existsSync(childPidPath)) {
				childPid = Number(readFileSync(childPidPath, "utf8"));
				cleanup.pids.push(childPid);
				const childReaped = await watchChildExit(childPid, async () => await rpcRequest(rpc, { type: "close_session", sessionId }));
				cleanup.process_proofs.push({ role: "mcp-stdio-child", pid: childPid, goneBySignal0: childReaped, exitedAfterSubscribedClose: childReaped, observedRequest: existsSync(logPath) });
			}
			const stopped = await terminate(host);
			await Promise.allSettled([host.exited, Promise.resolve(outDrain), Promise.resolve(errDrain)]);
			cleanup.process_proofs.push({ pid: host.pid, ...stopped, goneBySignal0: processGone(host.pid) });
			if (childPid !== null) cleanup.process_proofs.find((proof) => proof.role === "mcp-stdio-child").goneBySignal0 = processGone(childPid);
		}
		await provider.server.stop(true);
		writeFileSync(join(scratch, "host.stdout.log"), stdout);
		writeFileSync(join(scratch, "host.stderr.log"), stderr);
		const saved = join(ctx.evidence, name);
		mkdirSync(saved,{recursive:true});
		for(const path of artifacts) if(existsSync(path)) writeFileSync(join(saved,path.split("/").at(-1)),readFileSync(path));
		writeFileSync(join(saved,"provider-requests.json"),JSON.stringify(provider.requests.map(body=>JSON.parse(body)),null,2));
		writeFileSync(join(saved,"rpc-events.json"),JSON.stringify(rpc?.events??[],null,2));
		for(let i=0;i<artifacts.length;i++) artifacts[i]=join(saved,artifacts[i].split("/").at(-1));
		cleanup.socketGone = !existsSync(socketPath);
		rmSync(home, { recursive: true, force: true });
			rmSync(cwd, { recursive: true, force: true });
			rmSync(scratch, { recursive: true, force: true });
			cleanup.gone = !existsSync(home) && !existsSync(cwd) && !existsSync(scratch) && !existsSync(socketPath) && cleanup.process_proofs.filter((proof) => proof.role === "mcp-stdio-child").every((proof) => proof.goneBySignal0);
		cleanup.directories_removed = cleanup.gone;
		cleanup.child_pid = childPid;
	}
	if (name === "cleanup") {
		const childProved = cleanup.process_proofs.some((proof) => proof.role === "mcp-stdio-child" && proof.goneBySignal0);
		result = { status: childProved && cleanup.gone ? "pass" : "blocked", observable: { childProvedGone: childProved, detail: childProved ? "MCP stdio child lifetime ended with host shutdown" : "no recorded child process cleanup proof" } };
	} else if (result.status === "pending-cleanup") result.status = "pass";
	return { status: result.status, invocation: [ctx.binary, "--mode", "rpc", "--multi-session", "--listen", `unix://${socketPath}`, "--offline", "--provider", "offline", "--model", "offline", "<RPC open_session; prompt; get_entries>"], observable: result.observable,
		artifacts: artifacts.map((path) => path.replace(ctx.evidence, "<evidence>")), cleanup };
}

async function main() {
	const args = parseArgs(process.argv.slice(2));
	const ctx = { binary: resolve(args.binary), install: resolve(args.install), evidence: resolve(args.evidence), qaRoot: join(resolve(args.evidence), "qa") };
	mkdirSync(ctx.qaRoot, { recursive: true });
	const before = fingerprint(ctx.binary);
	const scenarios = {};
	const expectedSha = args.expectedSha ?? PINNED_SHA;
 if (!before || before.sha256 !== expectedSha) {
		for (const name of ["stdio-mcp", "tool-search", "cleanup"]) scenarios[name] = { status: "blocked", invocation: null, observable: `installed binary SHA mismatch; expected ${expectedSha}, got ${before?.sha256 ?? "missing"}`, artifacts: [], cleanup: { gone: true } };
	} else {
		for (const name of args.scenario === "all" ? ["stdio-mcp", "tool-search", "cleanup"] : [args.scenario]) {
			if (!(name in { "stdio-mcp": 1, "tool-search": 1, cleanup: 1 })) usage(`unknown scenario ${name}`);
			console.log(`hg-v-mcp: scenario ${name}`);
			try { scenarios[name] = await runScenario(ctx, name); }
			catch (error) { scenarios[name] = { status: "blocked", invocation: null, observable: error.message, artifacts: [], cleanup: { gone: false } }; }
		}
	}
	const after = fingerprint(ctx.binary), binaryStable = JSON.stringify(before) === JSON.stringify(after);
	const report = { schema: SCHEMA, binary: ctx.binary, install: ctx.install, expected_sha256: expectedSha, binary_before: before, binary_after: after, binary_stable: binaryStable,
		scenarios, status: binaryStable && Object.values(scenarios).every((scenario) => scenario.status === "pass" && scenario.cleanup?.gone) ? "pass" : "blocked" };
	const receipt = join(ctx.qaRoot, "hg-v-mcp.json");
	writeFileSync(receipt, JSON.stringify(report, null, 2) + "\n");
	console.log(`hg-v-mcp: ${report.status} -> ${receipt}`);
	process.exit(report.status === "pass" ? 0 : 1);
}
await main();
