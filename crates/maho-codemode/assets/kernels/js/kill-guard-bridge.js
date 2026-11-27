// Local bridge adapter for maho-codemode (senpi#5349): exposes the kill-guard judge API expected
// by worker-kill-guard.js and forwards synchronous checks to the Rust bridge /kill_guard_check route
// over a dedicated Worker + SharedArrayBuffer + Atomics.wait channel.

const workerThreads = process.getBuiltinModule("node:worker_threads");
const Worker = workerThreads?.Worker ?? globalThis.Worker;

const SAB_BYTES = 65_536;
const CTRL_WORDS = 4;
const HEADER_BYTES = CTRL_WORDS * 4;
const STATUS_IDLE = 0;
const STATUS_PENDING = 1;
const STATUS_DONE = 2;
const CHECK_DEADLINE_MS = 5_000;

const decoder = new TextDecoder();
const bridgePort = (() => {
	try {
		const raw =
			new URL(import.meta.url).searchParams.get("port") ??
			process.env.SENPI_KILL_GUARD_BRIDGE_PORT ??
			null;
		const parsed = raw === null ? Number.NaN : Number(raw);
		return Number.isInteger(parsed) && parsed > 0 ? parsed : undefined;
	} catch {
		return undefined;
	}
})();

let client;

function ensureClient(port) {
	if (client !== undefined) return client;
	const sab = new SharedArrayBuffer(SAB_BYTES);
	const ctrl = new Int32Array(sab, 0, CTRL_WORDS);
	const body = new Uint8Array(sab, HEADER_BYTES);
	const workerScript = `
const { parentPort, workerData } = require("node:worker_threads");
const ctrl = new Int32Array(workerData.sab, 0, 4);
const body = new Uint8Array(workerData.sab, 16);
const encoder = new TextEncoder();
const url = "http://127.0.0.1:" + workerData.port + "/kill_guard_check";
parentPort.on("message", async (payload) => {
	let reply;
	try {
		const res = await fetch(url, {
			method: "POST",
			headers: { "content-type": "application/json" },
			body: JSON.stringify(payload),
		});
		const json = await res.json();
		if (json && json.ok === true && json.value) {
			reply = json.value;
		} else {
			const detail = json && json.error && json.error.message ? json.error.message : "bridge status " + res.status;
			reply = { action: "refuse", rule: "R5", message: "[senpi:kill-guard] refused (R5 UNKNOWN): " + detail };
		}
	} catch (error) {
		const detail = error && error.message ? error.message : String(error);
		reply = { action: "refuse", rule: "R5", message: "[senpi:kill-guard] refused (R5 UNKNOWN): " + detail };
	}
	const encoded = encoder.encode(JSON.stringify(reply));
	if (encoded.length > body.length) {
		const truncated = encoder.encode(JSON.stringify({
			action: "refuse",
			rule: "R5",
			message: "[senpi:kill-guard] refused (R5 UNKNOWN): kill-guard verdict exceeded bridge buffer",
		}));
		const len = Math.min(truncated.length, body.length);
		body.set(truncated.subarray(0, len));
		Atomics.store(ctrl, 1, len);
	} else {
		body.set(encoded);
		Atomics.store(ctrl, 1, encoded.length);
	}
	Atomics.store(ctrl, 0, 2);
	Atomics.notify(ctrl, 0);
});
`;
	const worker = new Worker(workerScript, { eval: true, workerData: { port, sab } });
	worker.unref();
	client = { worker, ctrl, body };
	return client;
}

function sendCheck(payload) {
	if (bridgePort === undefined || typeof Worker !== "function") {
		return {
			action: "refuse",
			rule: "R5",
			message: "[senpi:kill-guard] refused (R5 UNKNOWN): kill-guard bridge is unavailable",
		};
	}
	const { worker, ctrl, body } = ensureClient(bridgePort);
	Atomics.store(ctrl, 0, STATUS_PENDING);
	worker.postMessage(payload);
	const deadline = Date.now() + CHECK_DEADLINE_MS;
	while (Atomics.load(ctrl, 0) === STATUS_PENDING) {
		const remaining = deadline - Date.now();
		if (remaining <= 0) break;
		if (Atomics.wait(ctrl, 0, STATUS_PENDING, remaining) === "timed-out") break;
	}
	if (Atomics.load(ctrl, 0) !== STATUS_DONE) {
		Atomics.store(ctrl, 0, STATUS_IDLE);
		client = undefined;
		void worker.terminate();
		return {
			action: "refuse",
			rule: "R5",
			message: "[senpi:kill-guard] refused (R5 UNKNOWN): kill-guard check timed out",
		};
	}
	const len = Atomics.load(ctrl, 1);
	const jsonText = decoder.decode(body.subarray(0, len));
	Atomics.store(ctrl, 0, STATUS_IDLE);
	try {
		return JSON.parse(jsonText);
	} catch {
		return {
			action: "refuse",
			rule: "R5",
			message: "[senpi:kill-guard] refused (R5 UNKNOWN): kill-guard check returned invalid JSON",
		};
	}
}

function vantagePidOf(context) {
	if (!context || !context.vantage) return undefined;
	if (context.vantage.kind === "agent") return context.agentPid;
	if (context.vantage.kind === "process") return context.vantage.pid;
	return undefined;
}

export function spawnSyncFromNode(spawnSync) {
	return spawnSync;
}

export function spawnSyncFromBun(spawnSync) {
	return spawnSync;
}

export function killGuardContext(options) {
	return options;
}

export function judgeCommand(command, context) {
	const vantagePid = vantagePidOf(context);
	return sendCheck({
		kind: "command",
		command: String(command),
		...(context?.shell !== undefined ? { shell: context.shell } : {}),
		...(vantagePid !== undefined ? { vantagePid } : {}),
	});
}

export function judgeArgv(argv, context) {
	const vantagePid = vantagePidOf(context);
	return sendCheck({
		kind: "argv",
		argv: Array.isArray(argv) ? argv.map(String) : [],
		...(vantagePid !== undefined ? { vantagePid } : {}),
	});
}

export function judgeSignal(call, context) {
	const vantagePid = vantagePidOf(context);
	return sendCheck({
		kind: "signal",
		target: Number(call?.target),
		group: Boolean(call?.group),
		signal: call?.signal ?? null,
		...(vantagePid !== undefined ? { vantagePid } : {}),
	});
}
