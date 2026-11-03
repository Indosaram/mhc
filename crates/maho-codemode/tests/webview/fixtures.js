// Shared fixtures for the Bun.WebView bridge tests (port of senpi test/eval/webview-fixtures.ts), plus a driver for
// the relay process that hosts a kernel worker (src/kernels/js/worker_host.rs spawns the same command line).
import { execFile } from "node:child_process";
import { createServer } from "node:http";
import { join, resolve } from "node:path";

export const ASSETS = resolve(import.meta.dir, "../../assets/kernels/js");

const FIXTURE_HTML =
	"<!doctype html><title>kernel-webview</title><h1 id='greeting'>hello from the fixture</h1><button id='go' onclick=\"document.title='clicked'\">go</button>";

export const bunWebViewAvailable = typeof globalThis.Bun?.WebView === "function";

export async function serveFixturePage() {
	const server = createServer((_request, response) => {
		response.writeHead(200, { "content-type": "text/html" });
		response.end(FIXTURE_HTML);
	});
	await new Promise((done) => server.listen(0, "127.0.0.1", done));
	const address = server.address();
	return {
		url: `http://127.0.0.1:${address.port}/`,
		stop: () =>
			new Promise((done) => {
				server.closeAllConnections();
				server.close(() => done());
			}),
	};
}

function run(file, args) {
	return new Promise((done) => {
		execFile(file, [...args], { maxBuffer: 16 * 1024 * 1024 }, (_error, stdout) => done(String(stdout ?? "")));
	});
}

/** Bun-spawned Chrome = a direct child of `parent` launched with `--remote-debugging-pipe`. */
export async function bunChromeChildren(parent = process.pid) {
	const pids = [];
	for (const line of (await run("ps", ["-axo", "pid=,ppid=,command="])).split("\n")) {
		const [pidText, ppidText, ...command] = line.trim().split(/\s+/u);
		if (Number(ppidText) !== parent) continue;
		if (!command.join(" ").includes("--remote-debugging-pipe")) continue;
		pids.push(Number(pidText));
	}
	return pids;
}

export function alive(pid) {
	try {
		process.kill(pid, 0);
		return true;
	} catch {
		return false;
	}
}

/** One kernel worker behind the relay process, driven over the same JSON-lines bridge the Rust host speaks. */
export class RelayKernel {
	#process;
	#waiters = new Map();
	#ready;
	#exited;
	#buffer = "";

	constructor(entry = "worker-entry.js") {
		this.#process = Bun.spawn(["bun", join(ASSETS, "worker-relay.js"), join(ASSETS, entry), process.cwd(), "1"], {
			stdin: "pipe",
			stdout: "pipe",
			stderr: "inherit",
		});
		this.#exited = this.#process.exited;
		const ready = Promise.withResolvers();
		this.#ready = ready.promise;
		this.#waiters.set("ready", ready.resolve);
		void this.#read();
		this.#send({ type: "init", sessionId: "webview-test", connection: { port: 1, token: "t" }, kernelGeneration: 1, hostToolNames: [], foreignLanguageNames: [] });
	}

	get pid() {
		return this.#process.pid;
	}

	async run(code, cellId = crypto.randomUUID()) {
		await this.#ready;
		const result = Promise.withResolvers();
		this.#waiters.set(cellId, result.resolve);
		this.#send({ type: "run", cellId, code });
		return await result.promise;
	}

	async value(code) {
		const result = await this.run(code);
		if (!result.ok) throw new Error(`cell failed: ${result.error?.message}`);
		return result.valueRepr === undefined ? undefined : JSON.parse(result.valueRepr);
	}

	/** Sends `close` and resolves with the relay's exit code once it is gone. */
	async close() {
		this.#send({ type: "close" });
		return await this.#exited;
	}

	#send(message) {
		this.#process.stdin.write(`${JSON.stringify(message)}\n`);
		this.#process.stdin.flush();
	}

	async #read() {
		const decoder = new TextDecoder();
		for await (const chunk of this.#process.stdout) {
			this.#buffer += decoder.decode(chunk, { stream: true });
			let newline;
			while ((newline = this.#buffer.indexOf("\n")) >= 0) {
				const line = this.#buffer.slice(0, newline);
				this.#buffer = this.#buffer.slice(newline + 1);
				if (!line) continue;
				const message = JSON.parse(line);
				const key = message.type === "result" ? message.cellId : message.type;
				const waiter = this.#waiters.get(key);
				this.#waiters.delete(key);
				waiter?.(message);
			}
		}
	}
}
