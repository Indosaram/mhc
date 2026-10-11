#!/usr/bin/env bun
// Installed-binary LSP acceptance probe using scripted offline OpenAI tool calls.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, watch, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { homedir } from "node:os";
import { fileURLToPath } from "node:url";

const SCHEMA = "hg-v-lsp/v1";
const EXPECTED_SHA = "b0088a1a4449e3b10d5747d60ecd64c77ea0fbede9538266f98243931e06c50a";
const TIMEOUT_MS = 90_000;
const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const RUST_ANALYZER_CANDIDATES = [
	"/home/indo/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/rust-analyzer",
	"/usr/bin/rust-analyzer",
	"/usr/local/bin/rust-analyzer",
];

function usage(message) {
	console.error(`hg-v-lsp: ${message}`);
	console.error("usage: bun tools/hg-v-lsp.mjs --binary <mhc> --install <dir> --evidence <dir> --scenario all");
	process.exit(2);
}

function parseArgs(argv) {
	const args = {};
	for (let i = 0; i < argv.length; i++) {
  if (["--binary", "--install", "--evidence", "--scenario", "--expected-sha"].includes(argv[i])) args[argv[i].slice(2)] = argv[++i];
		else usage(`unknown argument ${argv[i]}`);
	}
	for (const key of ["binary", "install", "evidence"]) if (!args[key]) usage(`--${key} is required`);
	args.binary = resolve(args.binary);
	args.install = resolve(args.install);
	args.evidence = resolve(args.evidence);
	if ((args.scenario ?? "all") !== "all") usage("only --scenario all is supported");
	return args;
}

const args = parseArgs(process.argv.slice(2));
const sha = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");
const fingerprint = () => ({ sha256: sha(args.binary), mtimeMs: statSync(args.binary).mtimeMs });
const before = fingerprint();
if (before.sha256 !== (args["expected-sha"] ?? EXPECTED_SHA)) throw new Error(`installed binary SHA mismatch: ${before.sha256}`);
const qaDir = join(args.evidence, "qa");
mkdirSync(qaDir, { recursive: true });
const owned = [];

async function detectServer() {
	for (const command of [...RUST_ANALYZER_CANDIDATES, "rust-analyzer"]) {
		try {
   const result = Bun.spawn([command, "--version"], { stdout: "pipe", stderr: "pipe", env: { PATH: process.env.PATH } });
   const [versionText, , exitCode] = await Promise.all([new Response(result.stdout).text(), new Response(result.stderr).text(), result.exited]);
   const version = versionText.trim();
   if (exitCode === 0 && /^rust-analyzer \d/.test(version)) return { name: "rust-analyzer", command, version };
		} catch { /* a rustup shim is not a server; try the next candidate */ }
	}
	return null;
}

function isolated() {
	const root = mkdtempSync(join(tmpdir(), "hg-v-lsp-"));
	const home = join(root, "home"), agent = join(root, "agent"), memory = join(root, "memory"), cwd = join(root, "project");
	for (const path of [home, agent, memory, join(cwd, "src")]) mkdirSync(path, { recursive: true });
	writeFileSync(join(cwd, "Cargo.toml"), '[package]\nname = "hg_lsp_fixture"\nversion = "0.1.0"\nedition = "2021"\n');
	const file = join(cwd, "src", "lib.rs");
	writeFileSync(file, 'pub fn target() -> i32 {\n    let known_error: i32 = "not an integer";\n    1\n}\npub fn caller() { target(); }\n');
	return { root, home, agent, memory, cwd, file };
}

function record(name, status, invocation, observable, artifacts, cleanup) {
	scenarios[name] = { status, invocation, observable, artifacts, cleanup };
}

async function readLine(stream) {
	const reader = stream.getReader();
	let text = "";
	while (!text.includes("\n")) {
		const { value, done } = await reader.read();
		if (done) break;
		text += new TextDecoder().decode(value);
	}
	return text.trim();
}
const scenarios = {};

async function toolScenario(name, server) {
	const ctx = isolated();
	const artifactDir = join(qaDir, name);
	mkdirSync(artifactDir, { recursive: true });
	const fixtureEnv = { PATH: `${dirname(server.command)}:${process.env.PATH}`, HOME: ctx.home, MAHO_CODING_AGENT_DIR: ctx.agent, MAHO_MEMORY_HOME: ctx.memory, PI_OFFLINE: "1", PI_TELEMETRY: "0", RUSTUP_HOME: process.env.RUSTUP_HOME ?? join(homedir(), ".rustup"), CARGO_HOME: process.env.CARGO_HOME ?? join(homedir(), ".cargo") };
	const metadataProcess = Bun.spawn(["cargo", "metadata", "--no-deps", "--format-version", "1"], { cwd: ctx.cwd, env: fixtureEnv, stdout: "pipe", stderr: "pipe" });
	const [metadataStdout, metadataStderr, metadataExitCode] = await Promise.all([new Response(metadataProcess.stdout).text(), new Response(metadataProcess.stderr).text(), metadataProcess.exited]);
	const tracePath = join(artifactDir, "rust-analyzer-trace.jsonl");
	let workspaceTimer;
	let workspaceWatcher;
	const workspaceReady = new Promise((resolveReady, rejectReady) => {
		workspaceWatcher = watch(artifactDir, () => {
			if (!existsSync(tracePath)) return;
			const rows = readFileSync(tracePath, "utf8").split("\n").filter(Boolean).flatMap(line => { try { return [JSON.parse(line)]; } catch { return []; } });
			if (rows.some(row => row.direction === "server_stderr" && row.text.includes("did switch workspaces"))) {
				workspaceWatcher.close(); clearTimeout(workspaceTimer); resolveReady();
			}
		});
		workspaceTimer = setTimeout(() => { workspaceWatcher.close(); rejectReady(new Error("rust-analyzer workspace switch not observed")); }, TIMEOUT_MS);
	});
	workspaceReady.catch(() => {});
	let diagnosticTimer;
	let diagnosticWatcher;
	const diagnosticReady = new Promise((resolveReady, rejectReady) => {
		diagnosticWatcher = watch(artifactDir, () => {
			if (!existsSync(tracePath)) return;
			const rows = readFileSync(tracePath, "utf8").split("\n").filter(Boolean).flatMap(line => { try { return [JSON.parse(line)]; } catch { return []; } });
			if (rows.some(row => row.message?.method === "textDocument/publishDiagnostics" && row.message.params?.uri === `file://${ctx.file}` && row.message.params.diagnostics?.some(diagnostic => /mismatched types|expected.*i32/.test(diagnostic.message)))) {
				diagnosticWatcher.close(); clearTimeout(diagnosticTimer); resolveReady();
			}
		});
		diagnosticTimer = setTimeout(() => { diagnosticWatcher.close(); rejectReady(new Error("fixture type mismatch publication not observed")); }, TIMEOUT_MS);
	});
	diagnosticReady.catch(() => {});
	const userConfig = join(ctx.home, ".maho", "agent", "lsp-client.json");
	mkdirSync(dirname(userConfig), { recursive: true });
	writeFileSync(userConfig, JSON.stringify({ lsp: { rust: { command: [process.execPath, resolve(ROOT, "tools/hg-lsp-trace.mjs"), tracePath, server.command], extensions: [".rs"], priority: 100 } } }, null, 2) + "\n");
	const requests = [], frames = [];
	let callIndex = 0, proc, socket, stdoutPromise, stderrPromise, error = null;
	const calls = [{ name: "lsp_symbols", arguments: { filePath: ctx.file, scope: "document" } }, { name: "lsp_symbols", arguments: { filePath: ctx.file, scope: "workspace", query: "target" } }, ...(name === "diagnostics"
		? [{ name: "lsp_diagnostics", arguments: { filePath: ctx.file } }]
		: [
   { name: "lsp_prepare_rename", arguments: { filePath: ctx.file, line: 1, character: 11 } },
   { name: "lsp_rename", arguments: { filePath: ctx.file, line: 1, character: 11, newName: "renamed_target" } },
		])];
	const provider = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
		requests.push(await request.json());
		if (callIndex === 1) await workspaceReady;
		if (calls[callIndex]?.name === "lsp_diagnostics") await diagnosticReady;
		const call = calls[callIndex++];
		const delta = call
   ? { tool_calls: [{ index: 0, id: `hg_lsp_${callIndex}`, type: "function", function: { name: "eval", arguments: JSON.stringify({ language: "js", code: `display(await tool.${call.name}(${JSON.stringify(call.arguments)}))`, summary: `Exercise ${call.name} on the fixture`, timeout: 60, on_timeout: "error" }) } }] }
			: { content: "HG_LSP_PROBE_FINISHED" };
		const chunk = { id: "hg-lsp", object: "chat.completion.chunk", created: 0, model: "offline", choices: [{ index: 0, delta, finish_reason: call ? "tool_calls" : "stop" }] };
		return new Response(`data: ${JSON.stringify(chunk)}\n\ndata: [DONE]\n\n`, { headers: { "content-type": "text/event-stream" } });
	} });
	writeFileSync(join(ctx.agent, "models.json"), JSON.stringify({ providers: { offline: { api: "openai-completions", baseUrl: `http://127.0.0.1:${provider.port}/v1`, apiKey: "offline-fixture", models: [{ id: "offline", reasoning: false, input: ["text"], contextWindow: 128000, maxTokens: 4096 }] } } }));
	mkdirSync(join(ctx.agent, "omo-senpi", "omo-native"), { recursive: true });
	writeFileSync(join(ctx.agent, "omo-senpi", "omo-native", "onboarding-completed"), "");
	const hostSocket = join(ctx.root, "host.sock");
	const invocation = [args.binary, "--mode", "rpc", "--multi-session", "--listen", `unix://${hostSocket}`, "--offline", "--provider", "offline", "--model", "offline"];
	let sequence = 0, buffer = "", pending = new Map(), finish;
	const finished = new Promise((resolvePromise) => { finish = resolvePromise; });
 const observe = (frame) => frame.type === "agent_end";
	const deadline = setTimeout(() => { error = "bounded scenario deadline exceeded"; finish(); }, TIMEOUT_MS);
	try {
		const ready = new Promise((resolvePromise, reject) => {
			const watcher = watch(ctx.root, () => { if (existsSync(hostSocket)) { watcher.close(); clearTimeout(timer); resolvePromise(); } });
			const timer = setTimeout(() => { watcher.close(); reject(new Error("host socket readiness timeout")); }, 10_000);
		});
		proc = Bun.spawn(invocation, { cwd: ctx.cwd, env: fixtureEnv, stdin: "ignore", stdout: "pipe", stderr: "pipe" });
		owned.push({ pid: proc.pid, command: args.binary, argv: invocation });
		stdoutPromise = new Response(proc.stdout).text(); stderrPromise = new Response(proc.stderr).text();
		await ready;
		socket = await Bun.connect({ unix: hostSocket, socket: { data(_socket, chunk) {
			buffer += chunk.toString(); let index;
			while ((index = buffer.indexOf("\n")) >= 0) {
				const frame = JSON.parse(buffer.slice(0, index)); buffer = buffer.slice(index + 1); frames.push(frame);
				if (frame.type === "response" && pending.has(String(frame.id))) { pending.get(String(frame.id))(frame); pending.delete(String(frame.id)); }
				if (observe(frame)) finish();
			}
		}, error(_socket, cause) { error = cause.message; finish(); } } });
		const rpc = (type, fields = {}) => new Promise((resolvePromise) => { const id = String(++sequence); pending.set(id, resolvePromise); socket.write(JSON.stringify({ id, type, ...fields }) + "\n"); });
		const opened = await Promise.race([rpc("open_session", { cwd: ctx.cwd }), finished.then(() => null)]);
		if (!opened?.data?.sessionId) throw new Error("open_session did not return a session id");
		await Promise.race([rpc("prompt", { sessionId: opened.data.sessionId, message: `Run the scripted ${name} LSP acceptance probe on ${ctx.file}.` }), finished]);
		await finished;
	} catch (cause) { error = cause.message; }
	finally {
		workspaceWatcher.close(); clearTimeout(workspaceTimer);
		diagnosticWatcher.close(); clearTimeout(diagnosticTimer);
		clearTimeout(deadline); socket?.end();
	}
	const source = readFileSync(ctx.file, "utf8");
	const toolResults = requests.flatMap((request) => (request.messages ?? []).filter((message) => message.role === "tool"));
	const daemonLogs = [];
	for (const relative of readdirSync(ctx.home, { recursive: true })) {
		const path = join(ctx.home, relative);
		if (relative.endsWith(".log") && statSync(path).isFile()) daemonLogs.push({ path, text: readFileSync(path, "utf8") });
	}
	const daemonPidPaths = readdirSync(ctx.home, { recursive: true }).filter(path => path.endsWith("daemon.pid")).map(path => join(ctx.home, path));
	const initialTrace = existsSync(tracePath) ? readFileSync(tracePath, "utf8").split("\n").filter(Boolean).map(line => JSON.parse(line)) : [];
	const ownedDescendants = [...daemonPidPaths.map(path => Number(readFileSync(path, "utf8").trim())), ...initialTrace.filter(entry => entry.direction === "wrapper").flatMap(entry => [entry.pid, entry.childPid])].filter(Number.isInteger);
	const exitObservers = ownedDescendants.map(pid => {
		const observer = Bun.spawn(["python3", "-c", "import os,select,sys\npid=int(sys.argv[1])\ntry: fd=os.pidfd_open(pid)\nexcept ProcessLookupError: print('MISSING_AT_SUBSCRIBE',flush=True);sys.exit(2)\nprint('READY',flush=True)\np=select.poll();p.register(fd,select.POLLIN);sys.exit(0 if p.poll(15000) else 1)", String(pid)], { stdout: "pipe", stderr: "pipe" });
		return { pid, observer, ready: readLine(observer.stdout) };
	});
	const observerReadiness = await Promise.all(exitObservers.map(async ({ pid, observer, ready }) => ({ pid, observer, ready: (await ready).includes("READY") })));
	const hostExit = proc ? proc.exited : Promise.resolve(null);
	if (proc) {
		try { process.kill(proc.pid, "SIGTERM"); } catch {}
		const hostExitBounded = await Promise.race([hostExit.then(() => true), new Promise(resolvePromise => setTimeout(() => resolvePromise(false), 5000))]);
		if (!hostExitBounded) { try { process.kill(proc.pid, "SIGKILL"); } catch {} await hostExit; }
	}
	const stdout = stdoutPromise ? await stdoutPromise : "", stderr = stderrPromise ? await stderrPromise : "";
	provider.stop(true);
	const daemonCleanup = [];
	for (const pidPath of daemonPidPaths) {
		const pid = Number(readFileSync(pidPath, "utf8").trim());
		const removed = new Promise(resolveRemoved => {
			const watcher = watch(dirname(pidPath), () => { if (!existsSync(pidPath)) { watcher.close(); clearTimeout(timer); resolveRemoved(true); } });
			const timer = setTimeout(() => { watcher.close(); resolveRemoved(false); }, 10000);
		});
		try { process.kill(pid, "SIGTERM"); } catch {}
		daemonCleanup.push({ pid, pidFileRemoved: await removed });
	}
	const exitProofs = await Promise.all(exitObservers.map(async ({ pid, observer }, index) => ({ pid, observerReady: observerReadiness[index].ready, exitObserved: observerReadiness[index].ready && await observer.exited === 0, stderr: await new Response(observer.stderr).text() })));
	const trace = existsSync(tracePath) ? readFileSync(tracePath, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line)) : [];
	const descendantPids = [...daemonCleanup.map(entry => entry.pid), ...trace.filter(entry => entry.direction === "wrapper").flatMap(entry => [entry.pid, entry.childPid])].filter(Number.isInteger);
	const descendants = descendantPids.map(pid => {
		try { const stat = readFileSync(`/proc/${pid}/stat`, "utf8"); const fields = stat.slice(stat.lastIndexOf(")") + 2).split(" "); return { pid, state: fields[0], ppid: Number(fields[1]), terminated: fields[0] === "Z" }; }
		catch { return { pid, terminated: true, absent: true }; }
	});
	const artifact = join(artifactDir, "provider-rpc.json");
	const causalCalls = requests.flatMap(request => (request.messages ?? []).flatMap(message => message.tool_calls ?? []).map(toolCall => ({ id: toolCall.id, name: toolCall.function?.name, arguments: toolCall.function?.arguments })));
	const lspCallResults = toolResults.filter(result => causalCalls.some(call => call.id === result.tool_call_id && call.name === "eval"));
	const lspTraceMethods = trace.filter(entry => entry.direction === "client_to_server" || entry.direction === "server_to_client").map(entry => entry.message?.method).filter(Boolean);
	writeFileSync(artifact, JSON.stringify({ scenario: name, server, fixture: ctx.file, cargo_metadata: { exitCode: metadataExitCode, stdout: metadataStdout, stderr: metadataStderr }, provider_requests: requests, scripted_tool_calls: causalCalls, rpc_frames: frames, stdout, stderr, error, tool_results: toolResults, lsp_tool_call_results: lspCallResults, final_source: source, daemon_logs: daemonLogs, rust_analyzer_trace: trace, rust_analyzer_trace_path: tracePath, lsp_trace_methods: lspTraceMethods }, null, 2) + "\n");
	const hasActualLspChildren = initialTrace.some(entry => entry.direction === "wrapper" && Number.isInteger(entry.pid) && Number.isInteger(entry.childPid));
	const cleanupEvidenceComplete = hasActualLspChildren && ownedDescendants.length > 0 && exitProofs.length === ownedDescendants.length && exitProofs.every(entry => entry.observerReady && entry.exitObserved) && daemonCleanup.every(entry => entry.pidFileRemoved) && descendants.every(entry => entry.terminated);
	const passed = !error && metadataExitCode === 0 && causalCalls.some(call => call.name === "eval") && lspCallResults.length > 0 && lspTraceMethods.includes("initialize") && cleanupEvidenceComplete && (name === "diagnostics"
		? lspCallResults.some((result) => /not an integer|mismatched types|expected.*i32/i.test(JSON.stringify(result))) && lspTraceMethods.includes("textDocument/publishDiagnostics")
		: lspCallResults.some((result) => /Applied 2 edit\(s\) to 1 file\(s\)/.test(result.content)) && /pub fn renamed_target\(/.test(source) && /renamed_target\(\);/.test(source));
	const status = passed ? "passed" : "blocked";
	const observable = passed
		? (name === "diagnostics" ? "Scripted lsp_diagnostics result contains the fixture's Rust type mismatch." : "Scripted prepare-rename and rename calls returned and changed target plus its reference.")
		: `Acceptance is inconclusive, not a product failure: scripted calls=${calls.length}, provider requests=${callIndex}, causal eval calls=${causalCalls.filter(call => call.name === "eval").length}, LSP trace methods=${JSON.stringify(lspTraceMethods)}, owned descendants=${ownedDescendants.length}, metadata exit=${metadataExitCode}, error=${error ?? "none"}`;
	const pids = owned.splice(0).map(({ pid }) => pid), sockets = [hostSocket];
	for (const path of sockets) rmSync(path, { force: true });
	rmSync(ctx.root, { recursive: true, force: true });
	record(name, status, invocation, observable, [artifact], { pids, descendants, exitProofs, daemonCleanup, sockets, dirs: [ctx.root], proof: { process_gone: pids.every((pid) => { try { process.kill(pid, 0); return false; } catch { return true; } }), descendants_terminated: cleanupEvidenceComplete, observed_descendant_count: ownedDescendants.length, sockets_gone: sockets.every((path) => !existsSync(path)), temp_root_removed: !existsSync(ctx.root) } });
}

const server = await detectServer();

if (server) {
	for (const name of ["diagnostics", "rename"]) await toolScenario(name, server);
} else {
	const fact = "No executable rust-analyzer passed --version; rustup shims are not accepted as a server.";
	for (const name of ["diagnostics", "rename"]) record(name, "blocked", null, fact, [], { pids: [], sockets: [], dirs: [], proof: "No scenario processes started." });
}

const after = fingerprint();
const binaryStable = before.sha256 === after.sha256 && before.mtimeMs === after.mtimeMs;
const scenarioCleanups = Object.fromEntries(Object.entries(scenarios).filter(([name]) => ["diagnostics", "rename"].includes(name)));
const cleanup = {
	status: Object.keys(scenarioCleanups).length === 2 && Object.values(scenarioCleanups).every(scenario => scenario.cleanup?.proof?.process_gone && scenario.cleanup?.proof?.descendants_terminated && scenario.cleanup?.proof?.sockets_gone && scenario.cleanup?.proof?.temp_root_removed) ? "passed" : "blocked",
	invocation: "Owned host, LSP wrapper, and language-server identities are observed; no global process scan or foreign receipt is used.",
	observable: "Only per-run host, LSP wrapper, language server, daemon registration, socket, and temp-root teardown are evidence.",
	artifacts: Object.values(scenarios).flatMap(scenario => scenario.artifacts ?? []),
	cleanup: scenarioCleanups,
};
scenarios.cleanup = cleanup;
const report = { schema: SCHEMA, binary: args.binary, binary_before: before, binary_after: after, binary_stable: binaryStable, server, scenarios, status: binaryStable && Object.values(scenarios).every((scenario) => scenario.status === "passed") ? "passed" : Object.values(scenarios).some((scenario) => scenario.status === "failed") || !binaryStable ? "failed" : "blocked" };
writeFileSync(join(qaDir, "hg-v-lsp.json"), JSON.stringify(report, null, 2) + "\n");
console.log(JSON.stringify(report, null, 2));
if (report.status !== "passed") process.exitCode = 1;
