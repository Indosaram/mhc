#!/usr/bin/env bun
// Installed acceptance driver for V02 worker, memory recall, notices and quota classification.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, watch, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";

const SCHEMA = "hg-v-memory/v1";
const DEFAULT_EXPECTED_SHA = "b0088a1a4449e3b10d5747d60ecd64c77ea0fbede9538266f98243931e06c50a";
const IDENTITY = "qa-fixture";
const FACT = "HG_V_MEMORY_RECALL_CANARY canary rollback gate and release order";
const TIMEOUT = 30_000;

function parseArgs(argv) {
	const args = {};
	for (let i = 0; i < argv.length; i++) {
		const key = argv[i];
		if (key === "--binary") args.binary = argv[++i];
		else if (key === "--install") args.install = argv[++i];
		else if (key === "--evidence") args.evidence = argv[++i];
		else if (key === "--scenario") args.scenario = argv[++i];
		else if (key === "--expected-sha256") args.expectedSha = argv[++i];
		else throw new Error(`unknown argument ${key}`);
	}
	for (const key of ["binary", "install", "evidence"]) if (!args[key]) throw new Error(`--${key} is required`);
	args.expectedSha ??= DEFAULT_EXPECTED_SHA;
	return args;
}

const sha256 = (data) => createHash("sha256").update(data).digest("hex");
const fingerprint = (path) => existsSync(path) ? { sha256: sha256(readFileSync(path)), mtimeMs: statSync(path).mtimeMs } : null;

function waitForPath(path, timeoutMs = TIMEOUT) {
	if (existsSync(path)) return Promise.resolve(true);
	return new Promise((resolvePromise) => {
		const watcher = watch(dirname(path), () => {
			if (!existsSync(path)) return;
			clearTimeout(timer);
			watcher.close();
			resolvePromise(true);
		});
		const timer = setTimeout(() => { watcher.close(); resolvePromise(existsSync(path)); }, timeoutMs);
	});
}

function scriptedProvider(mode, delivery = null) {
	const state = { requests: [], waiters: [], count: 0 };
	const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
		const body = await request.text().catch(() => "");
		state.requests.push(body);
		state.count++;
		for (const waiter of [...state.waiters]) if (waiter.predicate(body)) {
			state.waiters.splice(state.waiters.indexOf(waiter), 1); clearTimeout(waiter.timer); waiter.resolve(body);
		}
		if (mode === "429") {
			const parsed = JSON.parse(body);
			if (parsed.model === "offline" && JSON.stringify(parsed.messages ?? []).includes("hg-v-memory-429-probe")) return new Response(JSON.stringify({ error: { message: "rate limit exceeded" } }), { status: 429, headers: { "retry-after": "1", "content-type": "application/json" } });
			return new Response(`data: ${JSON.stringify({ id: "qa", object: "chat.completion.chunk", model: "offline", choices: [{ index: 0, delta: { content: "Visible fallback retry notice: provider rate limit encountered." }, finish_reason: "stop" }] })}\n\ndata: [DONE]\n\n`, { headers: { "content-type": "text/event-stream" } });
		}
  const parsed = JSON.parse(body);
  const parentWrite = parsed.messages?.some(message => message.role === "user" && JSON.stringify(message).includes("hg-v-memory-write-probe"));
  const alreadyCalled = parsed.messages?.some(message => message.role === "tool");
		if (mode === "recall" && body.includes("notes/qa.md") && !alreadyCalled) {
   const delta = { tool_calls: [{ index: 0, id: "hg_recall_nudge", type: "function", function: { name: "nudge", arguments: JSON.stringify({ path: "notes/qa.md", hint: FACT }) } }] };
   return new Response(`data: ${JSON.stringify({ id: "qa", object: "chat.completion.chunk", model: "offline", choices: [{ index: 0, delta, finish_reason: "tool_calls" }] })}\n\ndata: [DONE]\n\n`, { headers: { "content-type": "text/event-stream" } });
  }
  if (mode === "recall" && body.includes("hg-v-memory-restart-probe") && !body.includes("notes/qa.md") && delivery) await delivery;
  const payload = mode === "write" && parentWrite && !alreadyCalled
   ? { tool_calls: [{ index: 0, id: "hg_memory_write", type: "function", function: { name: "eval", arguments: JSON.stringify({ language: "js", code: `display(await tool.memory({command:"create",file_path:"notes/written.md",description:"Installed memory write canary",file_text:${JSON.stringify(FACT)},reason:"Installed memory acceptance"}))`, summary: "Write the isolated memory canary", on_timeout: "error", timeout: 20 }) } }] }
   : { content: "ok" };
		const chunk = `data: ${JSON.stringify({ id: "qa", object: "chat.completion.chunk", created: 0, model: "offline", choices: [{ index: 0, delta: payload, finish_reason: payload.tool_calls ? "tool_calls" : "stop" }] })}\n\ndata: [DONE]\n\n`;
		return new Response(chunk, { headers: { "content-type": "text/event-stream" } });
	} });
	return { server, port: server.port, state, waitFor(predicate, ms = TIMEOUT) {
		const existing = state.requests.find(predicate);
		if (existing) return Promise.resolve(existing);
		return new Promise((resolvePromise) => {
			const waiter = { predicate, resolve: resolvePromise };
			waiter.timer = setTimeout(() => { state.waiters.splice(state.waiters.indexOf(waiter), 1); resolvePromise(null); }, ms);
			state.waiters.push(waiter);
		});
	} };
}

function sse(text) {
	return `data: ${JSON.stringify({ id: "qa", object: "chat.completion.chunk", created: 0, model: "offline", choices: [{ index: 0, delta: { role: "assistant", content: text }, finish_reason: "stop" }] })}\n\ndata: [DONE]\n\n`;
}

async function connectRpc(socketPath) {
	const client = { socket: null, buffer: "", pending: new Map(), events: [], waiters: [], next: 1 };
	const socket = await Bun.connect({ unix: socketPath, socket: {
		data(_socket, chunk) {
			client.buffer += chunk.toString();
			let index;
			while ((index = client.buffer.indexOf("\n")) >= 0) {
				const line = client.buffer.slice(0, index); client.buffer = client.buffer.slice(index + 1);
				let frame; try { frame = JSON.parse(line); } catch { continue; }
				if (frame.id && client.pending.has(frame.id)) { client.pending.get(frame.id)(frame); client.pending.delete(frame.id); }
				else { client.events.push(frame); for (const waiter of [...client.waiters]) if (waiter.predicate(frame)) { client.waiters.splice(client.waiters.indexOf(waiter), 1); clearTimeout(waiter.timer); waiter.resolve(frame); } }
			}
		}, close() { for (const resolvePromise of client.pending.values()) resolvePromise(null); client.pending.clear(); }, error() {},
	} });
	client.socket = socket;
	client.request = (fields, timeout = TIMEOUT) => new Promise((resolvePromise) => {
		const id = String(client.next++); const timer = setTimeout(() => { client.pending.delete(id); resolvePromise(null); }, timeout);
		client.pending.set(id, (frame) => { clearTimeout(timer); resolvePromise(frame); });
		socket.write(JSON.stringify({ id, ...fields }) + "\n");
	});
	client.waitEvent = (predicate, timeout = TIMEOUT) => new Promise((resolvePromise) => {
		const existing = client.events.find(predicate);
		if (existing) return resolvePromise(existing);
		const waiter = { predicate, resolve: resolvePromise };
		waiter.timer = setTimeout(() => { client.waiters.splice(client.waiters.indexOf(waiter), 1); resolvePromise(null); }, timeout);
		client.waiters.push(waiter);
	});
	client.close = () => socket.end();
	return client;
}

function setup(home, port) {
	const agent = join(home, "agent");
	mkdirSync(agent, { recursive: true }); mkdirSync(join(home, ".maho"), { recursive: true }); mkdirSync(join(home, "project"), { recursive: true });
	mkdirSync(join(agent, "omo-senpi", "omo-native"), { recursive: true });
	writeFileSync(join(agent, "omo-senpi", "omo-native", "onboarding-completed"), "");
	writeFileSync(join(home, ".maho", "omo.jsonc"), JSON.stringify({ categories: { quick: { model: "offline/offline" } }, memory: { enabled: true, agent: IDENTITY, recall: { max_items: 2 } } }, null, 2) + "\n");
	writeFileSync(join(home, ".gitconfig"), "[user]\n name = QA Fixture\n email = qa@example.invalid\n");
	writeFileSync(join(agent, "models.json"), JSON.stringify({ providers: { offline: { api: "openai-completions", baseUrl: `http://127.0.0.1:${port}/v1`, apiKey: "offline-fixture", models: ["offline", "fallback"].map(id => ({ id, reasoning: false, input: ["text"], contextWindow: 128000, maxTokens: 4096 })) } } }));
	writeFileSync(join(agent, "settings.json"), JSON.stringify({ defaultProvider: "offline", defaultModel: "offline", retry: { fallbackChains: { "offline/offline": ["offline/fallback"] } } }) + "\n");
	return { agent, env: { PATH: process.env.PATH, HOME: home, MAHO_CODING_AGENT_DIR: agent, MAHO_MEMORY_HOME: join(home, "memory"), OMO_SENPI_MEMORY_HOME: join(home, "memory"), GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null", GIT_AUTHOR_NAME: "QA Fixture", GIT_AUTHOR_EMAIL: "qa@example.invalid", GIT_COMMITTER_NAME: "QA Fixture", GIT_COMMITTER_EMAIL: "qa@example.invalid" } };
}

async function hostStart(ctx, mode, home, root, provider) {
	const config = setup(home, provider.port);
	const socket = join(root, `${mode}.sock`); const project = join(home, "project");
	// bind creates the pathname before listen; the host chmod occurs after UnixListener::bind.
	const appeared = new Promise(resolveReady => {
		const watcher = watch(root, (event, filename) => {
			if (event !== "change" || String(filename) !== `${mode}.sock` || !existsSync(socket)) return;
			watcher.close(); clearTimeout(timer); resolveReady(true);
		});
		const timer = setTimeout(() => { watcher.close(); resolveReady(false); }, TIMEOUT);
	});
	const proc = Bun.spawn([ctx.binary, "--mode", "rpc", "--multi-session", "--listen", `unix://${socket}`, "--offline"], { cwd: project, env: config.env, detached: true, stdin: "ignore", stdout: "pipe", stderr: "pipe" });
	const stdout = new Response(proc.stdout).text().catch(() => null); const stderr = new Response(proc.stderr).text().catch(() => null);
	const host = { proc, socket, stdout, stderr, config, client: null };
	try {
		const ready = await appeared;
		if (!ready) throw Object.assign(new Error("RPC host did not create its socket"), { contract: "RPC multi-session host socket readiness unavailable: crates/maho-cli/src/main.rs (installed surface)" });
		host.client = await connectRpc(socket);
		return host;
	} catch (error) {
		await stopHost(host, null, home, false);
		error.hostStderr = await stderr;
		throw error;
	}
}

function waitForProcessExit(proc, timeout = TIMEOUT) {
	return new Promise(resolvePromise => {
		const timer = setTimeout(() => resolvePromise(false), timeout);
		proc.exited.then(() => { clearTimeout(timer); resolvePromise(true); });
	});
}

async function stopHost(host, provider, home, removeHome = true) {
	if (host?.client) host.client.close();
	const socketRemoved = host ? waitForPathRemoval(host.socket) : Promise.resolve(true);
	if (host?.proc) {
		try { process.kill(host.proc.pid, "SIGTERM"); } catch {}
		const graceful = await waitForProcessExit(host.proc, 5_000);
		if (!graceful) { try { process.kill(host.proc.pid, "SIGKILL"); } catch {} await waitForProcessExit(host.proc, 5_000); }
	}
	const socketGone = await socketRemoved;
	const streams = host ? await Promise.all([host.stdout, host.stderr]) : ["", ""];
	provider?.server.stop(true);
 if (removeHome) rmSync(home, { recursive: true, force: true });
	return { pid: host?.proc.pid ?? null, socket: host?.socket ?? null, socketGone, streamsDrained: streams.every((s) => s !== null), providerStopped: Boolean(provider), home, homeRemoved: !existsSync(home), processTerminated: !host || host.proc.exitCode !== null };
}

function waitForPathRemoval(path, timeoutMs = TIMEOUT) {
	if (!existsSync(path)) return Promise.resolve(true);
	return new Promise((resolvePromise) => {
		const watcher = watch(dirname(path), () => {
			if (existsSync(path)) return;
			clearTimeout(timer); watcher.close(); resolvePromise(true);
		});
		const timer = setTimeout(() => { watcher.close(); resolvePromise(!existsSync(path)); }, timeoutMs);
	});
}

async function withHost(ctx, evidenceDir, name, mode, body) {
	const root = mkdtempSync(join(tmpdir(), "hg-v-memory-root-"));
	const home = mkdtempSync(join(tmpdir(), "hg-v-memory-home-"));
	mkdirSync(join(root, "artifacts"), { recursive: true });
	const provider = scriptedProvider(mode); let host = null; let result = { status: "fail", invocation: null, observable: null, artifacts: [], cleanup: null };
	try {
		host = await hostStart(ctx, mode, home, root, provider);
		const opened = await host.client.request({ type: "open_session", cwd: join(home, "project") });
		const sessionId = opened?.data?.sessionId;
		if (!sessionId) throw Object.assign(new Error("open_session returned no session id"), { contract: "RPC open_session response contract: crates/maho-cli/src/multi_session_host.rs (installed surface)" });
		const outcome = await body({ host, provider, sessionId, home, root });
		result = { ...result, ...outcome };
	} catch (error) {
		result.status = error.contract ? "blocked" : "fail";
		result.observable = { error: error.message, missing_contract: error.contract ?? null };
	} finally {
		result.artifacts ??= [];
		result.artifacts.push({ type: "provider_requests", requests: [...provider.state.requests] }, { type: "host_events", events: [...(host?.client.events ?? [])] });
		result.cleanup = await stopHost(host, provider, home);
		rmSync(root, { recursive: true, force: true });
		result.cleanup.root = root; result.cleanup.rootRemoved = !existsSync(root);
		result.cleanup.ok = result.cleanup.socketGone && result.cleanup.streamsDrained && result.cleanup.providerStopped && result.cleanup.homeRemoved && result.cleanup.rootRemoved && result.cleanup.processTerminated;
		if (!result.cleanup.ok && result.status === "pass") result.status = "fail";
		result.artifacts = result.artifacts ?? [];
		writeFileSync(join(evidenceDir, `${name}.json`), JSON.stringify(result, null, 2) + "\n");
	}
	return result;
}

async function memoryTool(ctx, evidenceDir) {
	return withHost(ctx, evidenceDir, "memory-tool", "write", async ({ host, provider, sessionId, home }) => {
		const seeded = await seedMemoryRepo(home, host.config.env);
  const agentStart = host.client.waitEvent((frame) => frame.type === "agent_end");
		const served = provider.waitFor((body) => body.includes("hg-v-memory-write-probe"));
  const response = await host.client.request({ type: "prompt", sessionId, message: "hg-v-memory-write-probe: invoke the supplied eval memory write.", sessionTitlePrompt: false });
		const idle = await agentStart; const body = await served;
		const repo = join(home, "memory", "agents", `${IDENTITY}-${sha256(Buffer.from(IDENTITY)).slice(0, 8)}`, "repo");
  const file = join(seeded.repo, "notes", "written.md");
		const git = Bun.spawn(["git", "-C", repo, "log", "-1", "--format=%H"], { env: { PATH: process.env.PATH, HOME: home }, stdout: "pipe", stderr: "pipe" });
		const [commit, gitCode] = await Promise.all([new Response(git.stdout).text(), git.exited]);
		const content = existsSync(file) ? readFileSync(file, "utf8") : "";
		const wrote = content.includes(FACT) && commit.trim().length === 40;
		const ok = response?.success === true && idle !== null && body !== null && wrote && gitCode === 0;
		const artifactPath = join(evidenceDir, "memory-tool-fact.md");
		writeFileSync(artifactPath, content);
		return { status: ok ? "pass" : "blocked", invocation: "mhc --mode rpc --multi-session; prompt eval -> native memory create in seeded identity repository", observable: { promptAccepted: response?.success === true, settled: idle !== null, providerSawPrompt: body !== null, factFile: file, factExists: content.includes(FACT), commit: commit.trim() || null, committed: gitCode === 0 && wrote }, artifacts: [{ type: "created_memory", path: artifactPath, content, commit: commit.trim() }] , blocker: ok ? null : "native memory tool did not persist the requested canary" };
	});
}

async function seedMemoryRepo(home, env) {
	const repo = join(home, "memory", "agents", `${IDENTITY}-${sha256(Buffer.from(IDENTITY)).slice(0, 8)}`, "repo");
	mkdirSync(join(repo, "notes"), { recursive: true });
	const file = join(repo, "notes", "qa.md");
	writeFileSync(file, `---\ndescription: Canary rollback gate and release order\n---\n# Deploy gate\n${FACT}\n`);
	for (const args of [["init", "-q"], ["add", "-A"], ["commit", "-q", "-m", "Seed native memory recall corpus"]]) {
		const process_ = Bun.spawn(["git", "-C", repo, ...args], { env, stdin: "ignore", stdout: "pipe", stderr: "pipe" });
		const [stderr, exitCode] = await Promise.all([new Response(process_.stderr).text(), process_.exited]);
		if (exitCode !== 0) throw new Error(`git ${args[0]} failed: ${stderr}`);
	}
	const head = Bun.spawn(["git", "-C", repo, "rev-parse", "HEAD"], { env, stdout: "pipe", stderr: "pipe" });
	const [commit, exitCode] = await Promise.all([new Response(head.stdout).text(), head.exited]);
	if (exitCode !== 0 || !/^[a-f0-9]{40}\s*$/.test(commit)) throw new Error("seed memory repository has no verifiable commit");
	return { repo, file, commit: commit.trim() };
}

async function restartRecall(ctx, evidenceDir) {
	const root = mkdtempSync(join(tmpdir(), "hg-v-memory-root-")); const home = mkdtempSync(join(tmpdir(), "hg-v-memory-home-"));
	const provider = scriptedProvider("recall"); let host = null; let secondProvider = null; let deliveryWatcher = null; let deliveryTimer = null;
	const result = { status: "fail", invocation: "Two separate installed RPC host processes with same isolated HOME and MAHO_MEMORY_HOME; second session prompt carries unique fact", observable: null, artifacts: [], cleanup: null };
	try {
		const seeded = await seedMemoryRepo(home, setup(home, provider.port).env);
		host = await hostStart(ctx, "recall-first", home, root, provider);
		let id = (await host.client.request({ type: "open_session", cwd: join(home, "project") }))?.data?.sessionId;
		if (!id) throw new Error("first session could not open");
		const firstReceipt = await stopHost(host, provider, home, false); host = null;
		const pending = join(seeded.repo, "..", "runtime", "recall", "pending");
		mkdirSync(pending, { recursive: true });
		let offered = null;
		const delivery = new Promise(resolveDelivery => {
			const inspect = () => {
				for (const name of readdirSync(pending)) {
					if (!name.endsWith(".json")) continue;
					let payload; try { payload = JSON.parse(readFileSync(join(pending, name), "utf8")); } catch { continue; }
					if (payload.nudges?.some(nudge => nudge.path === "notes/qa.md" && nudge.hint === FACT)) {
						offered = payload; clearTimeout(deliveryTimer); resolveDelivery(payload); return;
					}
				}
			};
			deliveryWatcher = watch(pending, inspect);
			deliveryTimer = setTimeout(() => resolveDelivery(null), TIMEOUT);
		});
		secondProvider = scriptedProvider("recall", delivery);
		const second = await hostStart(ctx, "recall-second", home, root, secondProvider); host = second;
		id = (await second.client.request({ type: "open_session", cwd: join(home, "project") }))?.data?.sessionId;
		const exactRequest = secondProvider.waitFor((body) => body.includes("hg-v-memory-restart-probe"));
  const idle = second.client.waitEvent((frame) => frame.type === "agent_end");
		const response = await second.client.request({ type: "prompt", sessionId: id, message: "hg-v-memory-restart-probe: what does my committed note record about the canary rollback gate and release order?", sessionTitlePrompt: false });
		const [body, settled] = await Promise.all([exactRequest, idle]);
		const delivered = second.client.waitEvent(frame => frame.type === "message_end" && JSON.stringify(frame.message?.Custom ?? frame.message).includes(FACT) && JSON.stringify(frame).includes("recall"));
		const nextIdle = second.client.waitEvent(frame => frame.type === "agent_end" && JSON.stringify(frame).includes("hg-v-memory-delivery-boundary"));
		await second.client.request({ type: "prompt", sessionId: id, message: "hg-v-memory-delivery-boundary: apply any queued committed memory hints.", sessionTitlePrompt: false });
		await Promise.all([delivered, nextIdle]);
		const parsed = body ? JSON.parse(body) : null;
		const injected = parsed?.messages?.filter((message) => message.role === "system" || message.role === "developer").map((message) => message.content).join("\n") ?? "";
		const userText = parsed?.messages?.filter((message) => message.role === "user").map((message) => message.content).join("\n") ?? "";
		const factInjected = Boolean(offered) && second.client.events.some(frame => frame.type === "message_end" && JSON.stringify(frame.message?.Custom ?? frame.message).includes(FACT) && JSON.stringify(frame).includes("recall")) && !userText.includes(FACT);
		const ok = factInjected && settled !== null && response?.success === true;
		result.status = ok ? "pass" : "blocked"; result.observable = { firstHostCleanup: firstReceipt, secondProcess: second.proc.pid, seedCommit: seeded.commit, factCommitted: Boolean(seeded.commit), factInjectedIntoContext: factInjected, factInUserPrompt: userText.includes(FACT), providerRequest: body, settled: settled !== null, promptAccepted: response?.success === true };
		result.artifacts = [{ type: "accepted_recall_offer", payload: offered }, { type: "seeded_memory", path: "restart-recall-seed.md", content: readFileSync(seeded.file, "utf8"), commit: seeded.commit }, { type: "provider_requests", requests: [...secondProvider.state.requests] }, { type: "host_events", events: [...second.client.events] }];
		if (!ok) result.blocker = "reopen recall did not offer and deliver the committed note through the registered recall nudge path; user prompt is excluded";
	} catch (error) { result.status = error.contract ? "blocked" : "fail"; result.observable = { error: error.message, missing_contract: error.contract ?? null, hostStderr: error.hostStderr ?? null }; }
	finally {
		deliveryWatcher?.close(); clearTimeout(deliveryTimer);
		if (host) { const receipt = await stopHost(host, secondProvider ?? provider, home); result.cleanup = receipt; if (secondProvider) provider.server.stop(true); }
		else { rmSync(home, { recursive: true, force: true }); result.cleanup = { home, homeRemoved: !existsSync(home), processTerminated: true, socketGone: true, streamsDrained: true, providerStopped: true }; }
		rmSync(root, { recursive: true, force: true }); result.cleanup.root = root; result.cleanup.rootRemoved = !existsSync(root); result.cleanup.ok = result.cleanup.homeRemoved && result.cleanup.rootRemoved && result.cleanup.processTerminated && result.cleanup.socketGone && result.cleanup.streamsDrained;
		writeFileSync(join(evidenceDir, "restart-recall.json"), JSON.stringify(result, null, 2) + "\n");
	}
	return result;
}

async function quota(ctx, evidenceDir) {
	return withHost(ctx, evidenceDir, "provider-429", "429", async ({ host, provider, sessionId }) => {
		const retryEvent = (frame) => frame.type === "auto_retry_start" || frame.type === "auto_retry_end";
		const noticeEvent = (frame) => frame.type === "retry_fallback_applied";
		const retryWait = host.client.waitEvent(retryEvent, 10_000);
		const noticeWait = host.client.waitEvent(noticeEvent, 10_000);
		const idle = host.client.waitEvent((frame) => frame.type === "agent_end" && frame.messages?.some(message => message.role === "assistant" && message.model === "fallback" && message.stopReason === "stop"), TIMEOUT);
		const served = provider.waitFor((body) => body.includes("hg-v-memory-429-probe"));
		const fallbackServed = provider.waitFor(body => {
			try { return JSON.parse(body).model === "fallback"; } catch { return false; }
		});
		const responsePromise = host.client.request({ type: "prompt", sessionId, message: "hg-v-memory-429-probe", sessionTitlePrompt: false }, TIMEOUT);
		const [settled, requestBody, response, retry, notice] = await Promise.all([idle, served, responsePromise, retryWait, noticeWait, fallbackServed]);
		const relevant = host.client.events.map((frame, index) => ({ index, frame })).filter(({ frame }) => retryEvent(frame));
		const errorEntries = host.client.events.map((frame, index) => ({ index, frame })).filter(({ frame }) => JSON.stringify(frame).includes('"errorMessage":"429: {\\"message\\":\\"rate limit exceeded\\"}"') || frame.entry?.message?.errorMessage?.includes("429: {\"message\":\"rate limit exceeded\"}"));
		const noticeRecords = host.client.events.map((frame, index) => ({ index, frame })).filter(({ frame }) => noticeEvent(frame));
		const fallbackRequests = provider.state.requests.map((body, index) => ({ index, body })).filter(({ body }) => {
			try { const parsed = JSON.parse(body); return parsed.model && parsed.model !== "offline"; } catch { return false; }
		});
		const fallbackModel = fallbackRequests.length ? JSON.parse(fallbackRequests.at(-1).body).model : null;
		const visibleNotice = noticeRecords.find(({ frame }) => typeof frame.from === "string" && typeof frame.to === "string" && frame.from !== frame.to && frame.to === `offline/${fallbackModel}`);
		const classified = errorEntries.length > 0 && relevant.length > 0 && relevant.some(({ frame }) => frame.errorMessage?.includes("429") || frame.errorMessage?.includes("rate limit"));
		const fallbackObserved = fallbackRequests.length > 0 && typeof fallbackModel === "string" && fallbackModel.length > 0;
		const noticeObserved = Boolean(visibleNotice);
		const ok = settled !== null && classified && fallbackObserved && noticeObserved;
		return { status: ok ? "pass" : "blocked", invocation: "mhc --mode rpc --multi-session --listen unix://<temp>/provider-429.sock --offline; open_session; prompt; loopback replies HTTP 429", observable: { requestObserved: requestBody !== null, responseStatus: 429, settled: settled !== null, promptResponse: response, retryEvents: relevant, retryEventCount: relevant.length, providerErrorEntries: errorEntries, classifierObserved: classified, fallbackRequestObserved: fallbackObserved, fallbackModel, fallbackProviderRequests: fallbackRequests, noticeObserved, noticeEvent: visibleNotice ?? null, noticeEventCount: noticeRecords.length, noCrash: host.proc.exitCode === null }, artifacts: [], blocker: ok ? null : `429 acceptance incomplete: retry events=${relevant.length}, provider 429 error entries=${errorEntries.length}, alternate-model requests=${fallbackRequests.length}, visible retry/fallback notices=${noticeRecords.length}; all four observations are required` };
	});
}

async function main() {
	const args = parseArgs(process.argv.slice(2));
	const binary = resolve(args.binary); const install = resolve(args.install); const evidence = resolve(args.evidence);
	const qa = join(evidence, "qa"); mkdirSync(qa, { recursive: true });
	const binaryBefore = fingerprint(binary); const installBinary = join(install, "mhc");
	if (!binaryBefore || binary !== installBinary) throw new Error("--binary must be --install/mhc");
	const scenarios = {};
	const available = new Map([["memory-tool", memoryTool], ["restart-recall", restartRecall], ["provider-429", quota]]);
	const names = args.scenario === "all" || !args.scenario ? [...available.keys()] : [args.scenario];
	for (const name of names) if (!available.has(name)) throw new Error(`unknown scenario ${name}`);
	for (const name of names) {
		const fn = available.get(name);
		console.log(`hg-v-memory: ${name}`);
		try { scenarios[name] = await fn({ binary }, qa); }
		catch (error) { scenarios[name] = { status: "fail", invocation: null, observable: { error: error.message }, artifacts: [], cleanup: { ok: false } }; }
		console.log(`hg-v-memory: ${name} -> ${scenarios[name].status}`);
	}
	const binaryAfter = fingerprint(binary); const binaryStable = Boolean(binaryBefore && binaryAfter && binaryBefore.sha256 === binaryAfter.sha256 && binaryBefore.mtimeMs === binaryAfter.mtimeMs);
	const report = { schema: SCHEMA, binary, binary_before: binaryBefore, binary_after: binaryAfter, binary_stable: binaryStable, expected_sha256: args.expectedSha, scenarios, status: binaryStable && binaryBefore.sha256 === args.expectedSha && Object.values(scenarios).every((scenario) => scenario.status === "pass" && scenario.cleanup?.ok) ? "pass" : "blocked" };
	writeFileSync(join(qa, "hg-v-memory.json"), JSON.stringify(report, null, 2) + "\n");
	console.log(`hg-v-memory: ${report.status} -> ${join(qa, "hg-v-memory.json")}`);
	process.exitCode = report.status === "pass" ? 0 : 1;
}

await main();
