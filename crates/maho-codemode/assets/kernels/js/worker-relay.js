// The bun process that hosts one JS kernel worker thread (worker or inline mode) and relays its messages to the Rust
// host as JSON lines on stdout/stdin. Its main thread is also where Chrome-backed Bun.WebViews may be constructed, so
// it answers the worker's `webview-connect` with a private port to webview-host.js's service and releases every grant
// when the worker exits (senpi kernels/js/worker-slot.ts #publish / retire with KernelWebViewClients).
//
// usage: bun worker-relay.js <worker entry> <cwd> <parallel pool width>
import { createInterface } from "node:readline";
import { Worker } from "node:worker_threads";
import { encodeFrameValues, reviveFrameValues } from "./frame-values.js";

const [entry, cwd, poolWidth] = process.argv.slice(2);
const worker = new Worker(entry, { workerData: { cwd, parallelPoolWidth: Number(poolWidth) } });
const write = (message) => process.stdout.write(`${JSON.stringify(encodeFrameValues(message))}\n`);

// Loaded on the first WebView a cell asks for, so starting a kernel never pays for the service.
let webViews;
function clients() {
	webViews ??= import("./webview-host.js").then(({ KernelWebViewClients }) => new KernelWebViewClients((message, transfer) => worker.postMessage(message, [...transfer])));
	return webViews;
}

worker.on("message", (message) => {
	if (message?.type === "webview-connect") {
		clients().then(
			(granted) => granted.consume(message),
			(error) => worker.postMessage({ type: "webview-port", requestId: message.requestId, ok: false, error: { message: error.message, name: error.name } }),
		);
		return;
	}
	write(message);
});
worker.on("error", (error) => {
	write({ type: "init-failed", error: { message: error.message, name: error.name, stack: error.stack } });
	process.exitCode = 1;
});
worker.on("exit", async (code) => {
	// Closes every view the generation left open and retires the Chrome they used before the host sees the exit.
	await webViews?.then((granted) => granted.release(), () => {});
	process.stdout.write("", () => process.exit(code));
});
const input = createInterface({ input: process.stdin });
input.on("line", (line) => worker.postMessage(reviveFrameValues(JSON.parse(line))));
input.on("close", () => worker.terminate());
