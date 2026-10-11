#!/usr/bin/env bun
// QA-only, byte-transparent stdio tracer for a single LSP child process.
import { appendFileSync } from "node:fs";

const [tracePath, command, ...args] = process.argv.slice(2);
if (!tracePath || !command) {
	console.error("usage: bun tools/hg-lsp-trace.mjs <trace.jsonl> <server> [args...]");
	process.exit(2);
}

const child = Bun.spawn([command, ...args], {
	cwd: process.cwd(),
	env: { ...process.env, RA_LOG: "rust_analyzer=info,project_model=debug" },
	stdin: "pipe",
	stdout: "pipe",
	stderr: "pipe",
});
let sequence = 0;
appendFileSync(tracePath, `${JSON.stringify({ sequence: ++sequence, direction: "wrapper", pid: process.pid, childPid: child.pid, observedAt: new Date().toISOString(), cwd: process.cwd(), command, args, env: Object.fromEntries(["HOME", "PATH", "RUSTUP_HOME", "CARGO_HOME"].map((key) => [key, process.env[key] ?? null])) })}\n`);

function observe(direction, chunk) {
	const key = direction === "client_to_server" ? "clientBuffer" : "serverBuffer";
	const state = observe[key] ?? { bytes: Buffer.alloc(0), expected: null };
	state.bytes = Buffer.concat([state.bytes, Buffer.from(chunk)]);
	while (true) {
		if (state.expected === null) {
			const boundary = state.bytes.indexOf("\r\n\r\n");
			if (boundary < 0) break;
			const header = state.bytes.subarray(0, boundary).toString("ascii");
			const match = /(?:^|\r\n)Content-Length:\s*(\d+)/i.exec(header);
			if (!match) {
				state.bytes = state.bytes.subarray(boundary + 4);
				continue;
			}
			state.expected = { headerBytes: boundary + 4, bodyBytes: Number(match[1]) };
		}
		const frameBytes = state.expected.headerBytes + state.expected.bodyBytes;
		if (state.bytes.length < frameBytes) break;
		const body = state.bytes.subarray(state.expected.headerBytes, frameBytes).toString("utf8");
		state.bytes = state.bytes.subarray(frameBytes);
		state.expected = null;
		let message;
		try { message = JSON.parse(body); } catch { continue; }
		const method = message.method;
		if (
			(direction === "client_to_server" && (method === "initialize" || message.id !== undefined || method === "textDocument/didOpen")) ||
			(direction === "server_to_client" && (
				message.id !== undefined ||
				["$/status", "$/progress", "window/logMessage", "window/showMessage", "textDocument/publishDiagnostics"].includes(method)
			))
		) {
			appendFileSync(tracePath, `${JSON.stringify({ sequence: ++sequence, direction, observedAt: new Date().toISOString(), message })}\n`);
		}
	}
	observe[key] = state;
}

const toServer = (async () => {
	for await (const chunk of Bun.stdin.stream()) {
		const bytes = Buffer.from(chunk);
		observe("client_to_server", bytes);
		await child.stdin.write(bytes);
	}
	child.stdin.end();
})();
const toClient = (async () => {
	for await (const chunk of child.stdout) {
		const bytes = Buffer.from(chunk);
		observe("server_to_client", bytes);
		await Bun.stdout.write(bytes);
	}
})();
const stderr = (async () => {
	for await (const chunk of child.stderr) {
		appendFileSync(tracePath, `${JSON.stringify({ sequence: ++sequence, direction: "server_stderr", text: Buffer.from(chunk).toString("utf8") })}\n`);
	}
})();

const [, exitCode] = await Promise.all([toServer, child.exited, toClient, stderr]).then(([, code]) => [undefined, code]);
appendFileSync(tracePath, `${JSON.stringify({ sequence: ++sequence, direction: "wrapper_exit", observedAt: new Date().toISOString(), exitCode })}\n`);
process.exitCode = exitCode ?? 1;
