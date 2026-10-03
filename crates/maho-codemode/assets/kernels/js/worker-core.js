import { kernelToolCallContext } from "./kernel-tools-context.js";
import { kernelToolError } from "./kernel-tools-errors.js";
import { createKernelToolPump } from "./kernel-tools-pump.js";
import { hostDeniedError, hostToolRefusal } from "./kernel-tools-scope.js";
import { JsWorkerRuntime } from "./worker-runtime.js";

// Mirrors INTERRUPT_ACK_OP and CHILD_LIFECYCLE_OP in src/bridge/reserved.ts (this worker file cannot import TypeScript).
const INTERRUPT_ACK_OP = "interrupt-ack";
const CHILD_LIFECYCLE_OP = "child";

// Mirrors SESSION_ENVIRONMENT_KEYS in src/kernels/session-env.ts (this worker file
// cannot import TypeScript). Keys the active session does not set must be dropped so a
// value inherited from the host environment never leaks into a cell or its children.
const SESSION_ENVIRONMENT_KEYS = [
	"PI_SESSION_ID",
	"PI_SESSION_FILE",
	"PI_SESSION_CWD",
	"PI_GOAL_STORE_FILE",
	"PI_PROVIDER",
	"PI_MODEL",
	"PI_REASONING_LEVEL",
];

export function createWorkerCore(transport, options) {
	let runtime = null;
	let activeCell = null;
	const pendingTools = new Map();
	const nestedInvokes = new Map();
	const kernelTools = createKernelToolPump({
		getRuntime: () => runtime,
		emit: (message) => transport.send(message),
		nestedInvokes,
	});

	function emit(message) {
		transport.send(message);
	}

	async function runCell(message) {
		if (!runtime) {
			emit({ type: "result", cellId: message.cellId, ok: false, error: { message: "JS runtime not initialized" }, durationMs: 0 });
			return;
		}
		const startedAtMs = performance.now();
		activeCell = { cellId: message.cellId, interruption: null };
		try {
			const value = await runtime.run(message.code, message.cellId, {
				emit,
				callTool: async (toolName, args) => await callTool(toolName, args),
			});
			emit({ type: "result", cellId: message.cellId, ok: true, valueRepr: valueRepr(value), durationMs: durationMs(startedAtMs) });
		} catch (error) {
			emit({ type: "result", cellId: message.cellId, ok: false, error: bridgeError(error), durationMs: durationMs(startedAtMs) });
		} finally {
			activeCell = null;
		}
	}

	async function callTool(toolName, args) {
		const nested = kernelToolCallContext.getStore();
		if (!nested && activeCell?.interruption) throw activeCell.interruption;
		if (nested?.signal.aborted) throw nested.signal.reason;
		// A scoped kernel-tool call is refused here, before anything reaches the host bridge, so the
		// closure sees a rejected promise and the parent's own cells keep their full tool surface (#1731).
		if (nested) {
			const refusal = hostToolRefusal(nested.scope, toolName);
			if (refusal) throw hostDeniedError(toolName, nested.callId, refusal);
		}
		const bag = nested?.pendingTools ?? pendingTools;
		const callId = `js-${crypto.randomUUID()}`;
		const promise = new Promise((resolve, reject) => bag.set(callId, { resolve, reject }));
		emit({ type: "tool-call", callId, toolName, args });
		return await promise;
	}

	function interruptCell(reason) {
		if (!activeCell || !runtime) return;
		emit({ type: "status", event: { op: INTERRUPT_ACK_OP, cellId: activeCell.cellId } });
		const interruption = cellInterruptedError(reason);
		activeCell.interruption = interruption;
		for (const [callId, pending] of pendingTools) {
			pendingTools.delete(callId);
			pending.reject(interruption);
		}
		kernelTools.abortAll(kernelToolError("kernel_tool_stale", interruption.message));
		runtime.interrupt();
	}

	function onMessage(message) {
		if (kernelTools.handle(message)) return;
		if (message.type === "kernel-tools-names") {
			runtime?.kernelTools.setCollisionNames(message.hostToolNames ?? [], message.foreignLanguageNames ?? []);
			return;
		}
		if (message.type === "init") {
			applySessionEnvironment(message.sessionEnv);
			runtime = new JsWorkerRuntime({
				cwd: options.cwd,
				parallelPoolWidth: options.parallelPoolWidth,
				localRoots: message.connection.localRoots,
				artifactsDir: message.connection.artifactsDir,
				kernelGeneration: message.kernelGeneration ?? 1,
				hostToolNames: message.hostToolNames ?? [],
				foreignLanguageNames: message.foreignLanguageNames ?? [],
				onChildEvent: (event) => emit({ type: "status", event: { op: CHILD_LIFECYCLE_OP, ...event } }),
			});
			emit({ type: "ready" });
			return;
		}
		if (message.type === "run") {
			void runCell(message);
			return;
		}
		if (message.type === "tool-reply") {
			if (kernelTools.settleToolReply(message)) return;
			const pending = pendingTools.get(message.callId);
			if (!pending) return;
			pendingTools.delete(message.callId);
			if (message.ok) pending.resolve(message.value);
			else pending.reject(errorFromBridge(message.error));
			return;
		}
		if (message.type === "interrupt") {
			interruptCell(message.reason ?? "interrupted");
			return;
		}
		if (message.type === "close") {
			emit({ type: "closed" });
			transport.close();
		}
	}

	const unsubscribe = transport.onMessage(onMessage);
	return {
		dispose() {
			unsubscribe();
			globalThis.__senpi_restore_console__?.();
		},
	};
}

function durationMs(startedAtMs) {
	return Math.max(0, Math.round(performance.now() - startedAtMs));
}

function applySessionEnvironment(sessionEnv) {
	const provided = new Set(Object.keys(sessionEnv ?? {}));
	const deleted = [];
	for (const key of SESSION_ENVIRONMENT_KEYS) {
		if (key in process.env && !provided.has(key)) deleted.push(key);
		delete process.env[key];
	}
	const applied = Object.entries(sessionEnv ?? {});
	for (const [key, value] of applied) process.env[key] = value;
	// A worker's process.env is its own view: Bun.$ and node:child_process read it, but Bun.spawn
	// without an explicit env inherits the OS environ, which also still holds deleted keys because
	// `delete process.env.X` does not unsetenv under Bun. installShellCapture reads these flags and
	// pins the worker's environment view for such children (see worker-shell-capture.js).
	globalThis.__senpi_session_env_deletions__ = deleted;
	globalThis.__senpi_session_env_applied__ = applied.length > 0 || deleted.length > 0;
}

function valueRepr(value) {
	if (value === undefined) return undefined;
	return JSON.stringify(value);
}

function cellInterruptedError(reason) {
	const error = new Error(`JS cell interrupted: ${reason}`);
	error.name = "CellInterruptedError";
	return error;
}

function bridgeError(error) {
	if (error instanceof Error) {
		return { name: error.name, message: error.message, stack: error.stack, ...(typeof error.code === "string" ? { code: error.code } : {}) };
	}
	return { message: String(error) };
}

function errorFromBridge(error) {
	const result = new Error(error.message);
	if (error.name) result.name = error.name;
	if (error.stack) result.stack = error.stack;
	if (typeof error.code === "string") result.code = error.code;
	return result;
}
