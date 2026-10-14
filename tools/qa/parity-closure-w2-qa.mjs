#!/usr/bin/env bun
// Real-surface QA for the wave 2-5 parity fixes: drives the built `mhc` binary through CLI,
// print, json and RPC surfaces in an isolated HOME, records one JSON line per scenario, and
// writes summary.json. usage: bun tools/qa/parity-closure-w2-qa.mjs <mhc-binary> <evidence-dir>
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, existsSync, readFileSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const binary = resolve(process.argv[2] ?? "target/debug/mhc");
const evidence = resolve(process.argv[3] ?? ".omo/evidence/parity-closure-20261014/qa-w2");
mkdirSync(evidence, { recursive: true });
const root = mkdtempSync(join(tmpdir(), "pc-qa2-"));
const home = join(root, "home");
const agent = join(home, ".maho", "agent");
mkdirSync(agent, { recursive: true });
writeFileSync(join(agent, "settings.json"), "{}\n");
const results = [];

function env(extra = {}) {
	return { HOME: home, PATH: process.env.PATH, TERM: "xterm-256color", MAHO_OFFLINE: "1", ...extra };
}
async function run(name, args, { cwd = root, extra = {}, stdin, timeoutMs = 30000 } = {}) {
	const child = Bun.spawn([binary, ...args], { cwd, env: env(extra), stdin: stdin === undefined ? "ignore" : "pipe", stdout: "pipe", stderr: "pipe" });
	if (stdin !== undefined) { child.stdin.write(stdin); child.stdin.end(); }
	const timer = setTimeout(() => child.kill("SIGKILL"), timeoutMs);
	const [stdout, stderr, code] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited]);
	clearTimeout(timer);
	writeFileSync(join(evidence, `${name}.stdout.txt`), stdout);
	writeFileSync(join(evidence, `${name}.stderr.txt`), stderr);
	return { stdout, stderr, code, killed: child.signalCode === "SIGKILL" };
}
function record(name, pass, detail) {
	const row = { scenario: name, pass, ...detail };
	results.push(row);
	console.log(JSON.stringify(row));
}
function jsonLines(text) {
	return text.split("\n").filter(Boolean).map((line) => { try { return JSON.parse(line); } catch { return undefined; } }).filter(Boolean);
}

// An OpenAI-compatible loopback model so print/json/rpc turns complete without the network.
const server = Bun.serve({
	port: 0,
	async fetch(request) {
		const body = await request.json().catch(() => ({}));
		const encoder = new TextEncoder();
		const chunk = (delta, finish) => `data: ${JSON.stringify({ id: "c1", object: "chat.completion.chunk", created: 1, model: body.model ?? "qa", choices: [{ index: 0, delta, finish_reason: finish }] })}\n\n`;
		const stream = new ReadableStream({
			start(controller) {
				controller.enqueue(encoder.encode(chunk({ role: "assistant", content: "qa-reply" }, null)));
				controller.enqueue(encoder.encode(chunk({}, "stop")));
				controller.enqueue(encoder.encode(`data: ${JSON.stringify({ id: "c1", object: "chat.completion.chunk", created: 1, model: "qa", choices: [], usage: { prompt_tokens: 5, completion_tokens: 2, total_tokens: 7 } })}\n\n`));
				controller.enqueue(encoder.encode("data: [DONE]\n\n"));
				controller.close();
			},
		});
		return new Response(stream, { headers: { "content-type": "text/event-stream" } });
	},
});
writeFileSync(join(agent, "models.json"), JSON.stringify({
	providers: { qa: { baseUrl: `http://127.0.0.1:${server.port}/v1`, api: "openai-completions", apiKey: "qa-key", models: [{ id: "qa-model", name: "QA Model", contextWindow: 128000, maxTokens: 4096 }] } },
}, null, 2));
const model = ["--model", "qa/qa-model"];

try {
	// print mode (tf-core-cli repair + cli-main-migrations-w2-03): a piped prompt completes and exits 0.
	{
		const r = await run("print-loopback", ["-p", ...model, "--no-session"], { stdin: "  say hi  \n" });
		record("print-loopback", r.code === 0 && r.stdout.includes("qa-reply") && !r.killed, { code: r.code, killed: r.killed, stdout: r.stdout.trim().slice(0, 120), stderr: r.stderr.trim().slice(0, 200) });
	}
	// modes-rpc-print-json-w4-01: json mode agent_end is the plain upstream record.
	{
		const r = await run("json-agent-end", ["--mode", "json", ...model, "--no-session", "hello"]);
		const end = jsonLines(r.stdout).find((line) => line.type === "agent_end");
		record("json-agent-end", r.code === 0 && !!end && !("abortSource" in end), { code: r.code, agentEndKeys: end ? Object.keys(end) : null });
	}
	// modes-rpc-print-json-w5-03: login_api_key / logout responses carry no `data` key.
	{
		const stdin = [
			{ id: "l1", type: "login_api_key", provider: "qa", key: "qa-key-2" },
			{ id: "l2", type: "logout", provider: "qa" },
		].map((c) => JSON.stringify(c)).join("\n") + "\n";
		const r = await run("rpc-login-logout", ["--mode", "rpc", ...model, "--no-session"], { stdin, timeoutMs: 20000 });
		const lines = jsonLines(r.stdout).filter((line) => line.type === "response");
		const login = lines.find((line) => line.id === "l1");
		const logout = lines.find((line) => line.id === "l2");
		record("rpc-login-logout", login?.success === true && logout?.success === true && !("data" in login) && !("data" in logout), { login, logout });
	}
	// modes-rpc-print-json-w5-01: navigate_tree to a missing target reports errorCode not_found.
	{
		const stdin = JSON.stringify({ id: "n1", type: "navigate_tree", targetId: "does-not-exist" }) + "\n";
		const r = await run("rpc-navigate-missing", ["--mode", "rpc", ...model, "--no-session"], { stdin, timeoutMs: 20000 });
		const resp = jsonLines(r.stdout).find((line) => line.id === "n1");
		record("rpc-navigate-missing", !!resp && resp.success === false && resp.errorCode === "not_found", { response: resp });
	}
	// cli-main-migrations-w3-04: host parse errors exit 2 with usage on stderr.
	{
		const r = await run("host-bad-args", ["host", "bogus"]);
		record("host-bad-args", r.code === 2 && /usage/i.test(r.stderr), { code: r.code, stderr: r.stderr.trim().slice(0, 200) });
	}
	// cli-main-migrations-w3-02: --list-models takes a tokenized provider/id query.
	{
		const r = await run("list-models-token", ["--list-models", "qa/qa-model"]);
		record("list-models-token", r.code === 0 && r.stdout.includes("qa-model"), { code: r.code, stdout: r.stdout.trim().slice(0, 200) });
	}
	// core-session-compaction-export-w5-04: a corrupt --session file is an Error, not a panic (exit 101).
	{
		const bad = join(root, "corrupt.jsonl");
		writeFileSync(bad, "this is not json\n");
		const r = await run("corrupt-session", ["-p", ...model, "--session", bad, "hi"]);
		record("corrupt-session", r.code !== 101 && r.code !== 0 && !/panicked/.test(r.stderr), { code: r.code, stderr: r.stderr.trim().slice(0, 200) });
	}
	// core-session-compaction-export-w5-08: --export of a non-.jsonl name keeps the full basename.
	{
		const session = join(root, "session.json");
		const now = new Date().toISOString();
		writeFileSync(session, [
			JSON.stringify({ type: "session", version: 3, id: "qa-export", timestamp: now, cwd: root }),
			JSON.stringify({ type: "message", id: "m1", parentId: null, timestamp: now, message: { role: "user", content: [{ type: "text", text: "hi" }], timestamp: Date.now() } }),
		].join("\n") + "\n");
		const r = await run("export-name", ["--export", session], { cwd: root });
		const html = readdirSync(root).filter((name) => name.endsWith(".html"));
		record("export-name", r.code === 0 && html.some((name) => name.includes("session.json")), { code: r.code, html });
	}
	// cli-main-migrations-18 (session-runtime worker): a multi-session stdio RPC host opens each session in
	// its own `rpc-session-worker` child process. Commands are sent one at a time, awaiting each response.
	{
		const child = Bun.spawn([binary, "--mode", "rpc", "--multi-session", ...model], { cwd: root, env: env(), stdin: "pipe", stdout: "pipe", stderr: "pipe" });
		const reader = child.stdout.getReader();
		const decoder = new TextDecoder();
		let buffer = "";
		const transcript = [];
		async function call(command) {
			child.stdin.write(JSON.stringify(command) + "\n");
			child.stdin.flush();
			const deadline = Date.now() + 20000;
			while (Date.now() < deadline) {
				const newline = buffer.indexOf("\n");
				if (newline >= 0) {
					const line = buffer.slice(0, newline); buffer = buffer.slice(newline + 1);
					transcript.push(line);
					const parsed = (() => { try { return JSON.parse(line); } catch { return undefined; } })();
					if (parsed?.type === "response" && parsed.id === command.id) return parsed;
					continue;
				}
				const { value, done } = await Promise.race([reader.read(), Bun.sleep(deadline - Date.now()).then(() => ({ done: true }))]);
				if (done) break;
				buffer += decoder.decode(value);
			}
			return undefined;
		}
		const open = await call({ id: "o1", type: "open_session", cwd: root });
		const children = (() => {
			const listed = [];
			for (const task of readdirSync(`/proc/${child.pid}/task`)) listed.push(...readFileSync(`/proc/${child.pid}/task/${task}/children`, "utf8").split(/\s+/).filter(Boolean));
			return listed.filter((pid) => { try { return readFileSync(`/proc/${pid}/environ`, "utf8").split("\0").includes("__PI_INTERNAL_SPAWN=rpc-session-worker"); } catch { return false; } });
		})();
		const handle = open?.data?.sessionId;
		const prompt = handle ? await call({ id: "o2", type: "get_state", sessionId: handle }) : undefined;
		child.stdin.end();
		child.kill("SIGTERM");
		await child.exited;
		writeFileSync(join(evidence, "rpc-session-worker.stdout.txt"), transcript.join("\n") + "\n");
		record("rpc-session-worker", open?.success === true && children.length === 1 && prompt?.success === true, { openSuccess: open?.success, workers: children.length, stateCwd: prompt?.data?.cwd });
	}
} finally {
	server.stop(true);
	writeFileSync(join(evidence, "summary.json"), JSON.stringify({ binary, results }, null, 2));
	rmSync(root, { recursive: true, force: true });
	console.log(`cleanup: removed ${root}, stopped loopback model on port ${server.port}`);
}
