#!/usr/bin/env bun
// Real-surface QA for the 2026-10-14 non-DAG parity closure: drives the built `mhc` binary
// through CLI/print/RPC surfaces in an isolated HOME and records one JSON line per scenario.
// usage: bun tools/qa/parity-closure-qa.mjs <mhc-binary> <evidence-dir>
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, existsSync, readFileSync, cpSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const binary = resolve(process.argv[2] ?? "target/debug/mhc");
const evidence = resolve(process.argv[3] ?? ".omo/evidence/parity-closure-20261014/qa");
mkdirSync(evidence, { recursive: true });
const root = mkdtempSync(join(tmpdir(), "pc-qa-"));
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

try {
	// cli-main-migrations-08: blank --name is rejected with the upstream message and exit 1.
	{
		const r = await run("blank-name", ["--name", "   ", "-p", "hi"]);
		record("blank-name", r.code === 1 && r.stderr.includes("--name requires a non-empty value"), { code: r.code, stderr: r.stderr.trim().slice(0, 200) });
	}
	// core-session-compaction-export-26 / cli-main-migrations-02: --export renders a JSONL session to HTML.
	{
		const session = join(root, "s.jsonl");
		const now = new Date().toISOString();
		writeFileSync(session, [
			JSON.stringify({ type: "session", version: 3, id: "qa-session", timestamp: now, cwd: root }),
			JSON.stringify({ type: "message", id: "m1", parentId: null, timestamp: now, message: { role: "user", content: [{ type: "text", text: "hello <script>x</script>" }], timestamp: Date.now() } }),
		].join("\n") + "\n");
		const out = join(root, "out.html");
		const r = await run("export-html", ["--export", session, out]);
		const html = existsSync(out) ? readFileSync(out, "utf8") : "";
		if (html) cpSync(out, join(evidence, "export.html"));
		const embedded = (html.match(/[A-Za-z0-9+/=]{200,}/g) ?? []).map((b) => Buffer.from(b, "base64").toString("utf8")).find((s) => s.includes('"qa-session"')) ?? "";
		record("export-html", r.code === 0 && html.includes("<html") && embedded.includes("hello") && !html.includes("<script>x</script>"), { code: r.code, bytes: html.length, stderr: r.stderr.trim().slice(0, 300) });
	}
	// cli-main-migrations-19 / builtin-extensions-10: --help lists extension flags.
	{
		const r = await run("help-flags", ["--help"]);
		record("help-flags", r.code === 0 && /--no-model-fallback|--no-ask-user/.test(r.stdout), { code: r.code, lines: r.stdout.split("\n").length });
	}
	// cli-main-migrations-20: `auth check` with bad args exits 2; --json carries accounts.
	{
		const bad = await run("auth-check-badargs", ["auth", "check", "--json"]);
		const ok = await run("auth-check-json", ["auth", "check", "--provider", "xai", "--json", "--no-refresh"], { extra: { XAI_API_KEY: "offline-fixture" } });
		let parsed = null; try { parsed = JSON.parse(ok.stdout); } catch {}
		record("auth-check", bad.code === 2 && ok.code === 0 && parsed && "accounts" in parsed && !ok.stdout.includes("offline-fixture"), { badCode: bad.code, okCode: ok.code, keys: parsed ? Object.keys(parsed) : null });
	}
	// senpi-ai-auth-16: standalone pi-ai login CLI lists providers including radius (senpi-ai-auth-12).
	{
		const piAi = resolve(binary, "..", "pi-ai");
		if (existsSync(piAi)) {
			const child = Bun.spawn([piAi, "list"], { env: env(), stdout: "pipe", stderr: "pipe" });
			const [stdout, , code] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited]);
			writeFileSync(join(evidence, "pi-ai-list.stdout.txt"), stdout);
			record("pi-ai-list", code === 0 && /radius/i.test(stdout), { code, lines: stdout.trim().split("\n").length });
		} else record("pi-ai-list", false, { missing: piAi });
	}
	// core-auth-model-01: print mode never blocks on a project-trust prompt (no TTY), and exits.
	{
		const project = join(root, "proj");
		mkdirSync(join(project, ".maho"), { recursive: true });
		writeFileSync(join(project, ".maho", "settings.json"), JSON.stringify({ theme: "light" }));
		const r = await run("trust-print-no-hang", ["-p", "hi", "--no-session"], { cwd: project, timeoutMs: 20000 });
		record("trust-print-no-hang", !r.killed, { code: r.code, killed: r.killed, stderr: r.stderr.trim().slice(0, 200) });
	}
	// modes-rpc-print-json-12: classic RPC get_protocol_info succeeds with mode "classic".
	{
		const r = await run("rpc-protocol-info", ["--mode", "rpc", "--no-session", "--model", "xai/grok-4.20-0309-non-reasoning"], { extra: { XAI_API_KEY: "offline-fixture" }, stdin: JSON.stringify({ id: "1", type: "get_protocol_info" }) + "\n", timeoutMs: 20000 });
		const line = r.stdout.split("\n").map((l) => { try { return JSON.parse(l); } catch { return null; } }).find((m) => m && m.id === "1");
		record("rpc-protocol-info", !!line && line.success === true && line.data?.mode === "classic", { response: line });
	}
	// modes-rpc-print-json-11: malformed command gets an error response instead of killing RPC.
	{
		const r = await run("rpc-malformed", ["--mode", "rpc", "--no-session", "--model", "xai/grok-4.20-0309-non-reasoning"], { extra: { XAI_API_KEY: "offline-fixture" }, stdin: JSON.stringify({ id: "2", type: "set_model" }) + "\n" + JSON.stringify({ id: "3", type: "get_protocol_info" }) + "\n", timeoutMs: 20000 });
		const msgs = r.stdout.split("\n").map((l) => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
		const bad = msgs.find((m) => m.id === "2"); const after = msgs.find((m) => m.id === "3");
		record("rpc-malformed", !!bad && bad.success === false && !!after && after.success === true, { bad, afterOk: after?.success });
	}
} finally {
	writeFileSync(join(evidence, "summary.json"), JSON.stringify({ binary, results }, null, 2));
	rmSync(root, { recursive: true, force: true });
	console.log(JSON.stringify({ cleanup: root, removed: !existsSync(root), passed: results.filter((r) => r.pass).length, total: results.length }));
}
