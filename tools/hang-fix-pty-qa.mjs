#!/usr/bin/env bun
// Interactive hang regression QA. The harness uses a real PTY, a loopback OpenAI-compatible
// provider, and event subscriptions installed before each input. It does not build mhc.
//
//   bun tools/hang-fix-pty-qa.mjs --binary <mhc> --evidence <directory>
import { mkdirSync, mkdtempSync, readFileSync, rmSync, watch, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const TIMEOUT_MS = 30_000;
const SENTINEL = "HANG_FIX_FINAL_SENTINEL_7f1a";
const SECOND_SENTINEL = "HANG_FIX_SECOND_TURN_7f1a";

function args(argv) {
	const result = {};
	for (let index = 0; index < argv.length; index += 1) {
		if (argv[index] === "--binary") result.binary = argv[++index];
		else if (argv[index] === "--evidence") result.evidence = argv[++index];
		else if (argv[index] === "--startup-only") result.startupOnly = true;
		else throw new Error(`Unknown argument: ${argv[index]}`);
	}
	if (!result.binary || !result.evidence) throw new Error("Usage: bun tools/hang-fix-pty-qa.mjs --binary <mhc> --evidence <directory>");
	return { binary: resolve(result.binary), evidence: resolve(result.evidence), startupOnly: result.startupOnly };
}

function sse(value) {
	return `data: ${JSON.stringify(value)}\n\ndata: [DONE]\n\n`;
}

function completion(text, finish = "stop") {
	return sse({ id: "hang-fix", object: "chat.completion.chunk", created: 0, model: "hang-fix-model", choices: [{ index: 0, delta: { role: "assistant", content: text }, finish_reason: finish }] });
}

function toolCall() {
	return sse({ id: "hang-fix", object: "chat.completion.chunk", created: 0, model: "hang-fix-model", choices: [{ index: 0, delta: { tool_calls: [{ index: 0, id: "read-hang-fix", type: "function", function: { name: "read", arguments: JSON.stringify({ path: "README.md" }) } }] }, finish_reason: "tool_calls" }] });
}

function eventWaiter(pty, predicate, label) {
	return new Promise((resolvePromise, reject) => {
		const timer = setTimeout(() => {
			pty.listeners.delete(onData);
			reject(new Error(`Timed out waiting for PTY ${label}`));
		}, TIMEOUT_MS);
		const onData = () => {
			if (!predicate(pty.raw)) return;
			clearTimeout(timer);
			pty.listeners.delete(onData);
			resolvePromise(pty.raw);
		};
		pty.listeners.add(onData);
		onData();
	});
}

async function main() {
	const options = args(process.argv.slice(2));
	mkdirSync(options.evidence, { recursive: true });
	const home = mkdtempSync(join(tmpdir(), "mhc-hang-fix-"));
	const agent = join(home, "agent");
	const project = join(home, "project");
	mkdirSync(agent, { recursive: true });
	mkdirSync(project, { recursive: true });
	const receipt = { schema: "hang-fix-pty-qa/v1", binary: options.binary, startedAt: new Date().toISOString(), assertions: {}, cleanup: {} };
	let provider;
	let pty;
	const capture = { raw: "", listeners: new Set() };
	let providerRequests = 0;
	let readResultObserved = false;
	let providerClosed = false;
	try {
		provider = Bun.serve({
			port: 0,
			async fetch(request) {
				if (request.method !== "POST") return new Response("not found", { status: 404 });
				providerRequests += 1;
				const body = await request.json();
				if (providerRequests === 1) {
					if (!body.tools?.some((tool) => tool.function?.name === "read")) throw new Error("Provider request did not advertise read tool");
					return new Response(toolCall(), { headers: { "content-type": "text/event-stream" } });
				}
				if (providerRequests === 2) {
					readResultObserved = body.messages?.some((message) => message.role === "tool" && message.tool_call_id === "read-hang-fix" && JSON.stringify(message.content).includes("PTY tool fixture"));
					if (!readResultObserved) return new Response("Expected actual read result", { status: 400 });
					return new Response(completion(SENTINEL), { headers: { "content-type": "text/event-stream" } });
				}
				if (providerRequests === 3) return new Response(completion(SECOND_SENTINEL), { headers: { "content-type": "text/event-stream" } });
				return new Response(completion("Unexpected extra provider request"), { headers: { "content-type": "text/event-stream" } });
			},
		});
		writeFileSync(join(agent, "models.json"), JSON.stringify({ providers: { local: {
			api: "openai-completions", baseUrl: `http://127.0.0.1:${provider.port}/v1`, apiKey: "qa-only",
			models: [{ id: "hang-fix-model", reasoning: false, input: ["text"], contextWindow: 200000, maxTokens: 4096 }],
		} } }, null, 2));
		writeFileSync(join(agent, "settings.json"), JSON.stringify({ defaultProvider: "local", defaultModel: "hang-fix-model" }));
		writeFileSync(join(project, "README.md"), "PTY tool fixture\n");
		// Ordinary initialization mounts OMO. HOME/.maho is deliberately absent, making the real
		// config-watch component emit its reload-required discovery warning through SinkLogger.

		pty = Bun.spawn([options.binary, "--provider", "local", "--model", "hang-fix-model", "--no-approve"], {
			cwd: project,
			env: { ...process.env, HOME: home, MAHO_CODING_AGENT_DIR: agent, TERM: "xterm-256color", NO_COLOR: "1" },
			terminal: { cols: 100, rows: 32, data(_terminal, data) {
				capture.raw += Buffer.from(data).toString("utf8");
				for (const listener of capture.listeners) listener();
			} },
		});
		const promptReady = eventWaiter(capture, (raw) => /›|❯|\>\s/.test(raw), "interactive prompt");
		const hiddenLogPath = join(agent, "maho-debug.log");
		const warningLogged = new Promise((resolvePromise, reject) => {
			let watcher;
			const finish = (error, contents) => {
				clearTimeout(timer);
				watcher?.close();
				if (error) reject(error);
				else resolvePromise(contents);
			};
			const inspect = () => {
				try {
					const contents = readFileSync(hiddenLogPath, "utf8");
					if (/maho-omo warn:.*(?:config-watch user config discovery requires reload|ulw-loop status ignored)/.test(contents)) finish(null, contents);
				} catch { /* The subscribed file has not been created yet. */ }
			};
			const timer = setTimeout(() => finish(new Error("Timed out awaiting config-watch warning in hidden debug log")), TIMEOUT_MS);
			watcher = watch(agent, { recursive: true }, inspect);
			inspect();
		});
		await promptReady;
		const logContents = await warningLogged;
		receipt.assertions.componentWarningInHiddenDebugLog = /maho-omo warn:.*(?:config-watch user config discovery requires reload|ulw-loop status ignored)/.test(logContents);
		if (!receipt.assertions.componentWarningInHiddenDebugLog) throw new Error("Component warning was not found in the hidden debug log");
		if (capture.raw.includes("config-watch user config discovery requires reload")) throw new Error("Expected config-watch warning to be redirected from the PTY");
		if (capture.raw.includes("maho-omo warn:")) throw new Error("maho-omo warning leaked to raw PTY output");
		receipt.assertions.mahoOmoWarningAbsentFromRawTerminal = true;
		if (options.startupOnly) {
			receipt.result = "passed";
			return;
		}

		const firstTurn = eventWaiter(capture, (raw) => providerRequests >= 2 && raw.includes(SENTINEL), "first-turn final sentinel");
		pty.terminal.write("Please inspect README.md and report the result.\r");
		const firstRaw = await firstTurn;
		receipt.assertions.realReadToolCall = readResultObserved && providerRequests >= 2 && firstRaw.includes(SENTINEL);
		if (!receipt.assertions.realReadToolCall) throw new Error("The scripted provider did not complete the real read-tool turn");

		const secondTurn = eventWaiter(capture, (raw) => providerRequests >= 3 && raw.includes(SECOND_SENTINEL), "responsive second-turn sentinel");
		pty.terminal.write("Reply once more.\r");
		const secondRaw = await secondTurn;
		if (secondRaw.includes("maho-omo warn:")) throw new Error("Component warning leaked during a turn");
		receipt.assertions.responsiveSecondTurn = providerRequests >= 3 && secondRaw.includes(SECOND_SENTINEL);
		if (!receipt.assertions.responsiveSecondTurn) throw new Error("The second interactive turn did not complete");
		receipt.providerRequests = providerRequests;
		receipt.result = "passed";
	} catch (error) {
		receipt.result = "failed";
		receipt.error = String(error?.stack ?? error);
	} finally {
		if (pty) {
			try { pty.terminal.write("\u0004"); } catch { /* terminal already stopped */ }
			const exited = await Promise.race([pty.exited.then(() => true).catch(() => true), new Promise((resolvePromise) => setTimeout(() => resolvePromise(false), 5000))]);
			if (!exited) {
				try { pty.kill("SIGTERM"); } catch { /* already stopped */ }
				receipt.cleanup.forcedTermination = true;
			}
			receipt.cleanup.ptyExited = await Promise.race([pty.exited.then(() => true).catch(() => true), new Promise((resolvePromise) => setTimeout(() => resolvePromise(false), 5000))]);
		} else receipt.cleanup.ptyExited = true;
		if (provider) { provider.stop(true); providerClosed = true; }
		writeFileSync(join(options.evidence, "terminal-ansi.txt"), capture.raw);
		try { writeFileSync(join(options.evidence, "hidden-debug.log"), readFileSync(join(agent, "maho-debug.log"))); } catch (error) { receipt.hiddenLogCaptureError = String(error); }
		receipt.cleanup.providerClosed = providerClosed;
		rmSync(home, { recursive: true, force: true });
		receipt.cleanup.tempHomeRemoved = true;
		receipt.finishedAt = new Date().toISOString();
		writeFileSync(join(options.evidence, "hang-fix-pty-qa-receipt.json"), `${JSON.stringify(receipt, null, 2)}\n`);
	}
	if (receipt.result !== "passed") throw new Error(`PTY QA failed; see ${join(options.evidence, "hang-fix-pty-qa-receipt.json")}`);
	console.log(`PTY QA passed; receipt: ${join(options.evidence, "hang-fix-pty-qa-receipt.json")}`);
}

await main();
