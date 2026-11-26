import { Worker as RealWorker } from "node:worker_threads";
import { pathToFileURL } from "node:url";

const realBuiltin = process.getBuiltinModule.bind(process);
class ControlledWorker extends RealWorker {
	constructor(_script, options) {
		super(`
			const { parentPort, workerData } = require("node:worker_threads");
			const ctrl = new Int32Array(workerData.sab, 0, 4);
			const body = new Uint8Array(workerData.sab, 16);
			let firstPending = false;
			parentPort.on("message", (payload) => {
				if (payload.command === "first") {
					firstPending = true;
					return;
				}
				const verdict = firstPending
					? { action: "allow", source: "stale" }
					: { action: "refuse", source: "current" };
				const encoded = new TextEncoder().encode(JSON.stringify(verdict));
				body.set(encoded);
				Atomics.store(ctrl, 1, encoded.length);
				Atomics.store(ctrl, 0, 2);
				Atomics.notify(ctrl, 0);
			});
		`, options);
	}
}
process.getBuiltinModule = (name) => name === "node:worker_threads"
	? { ...realBuiltin(name), Worker: ControlledWorker }
	: realBuiltin(name);
process.env.SENPI_KILL_GUARD_BRIDGE_PORT = "1";
const { judgeCommand } = await import(pathToFileURL(process.argv[2]).href);
const first = judgeCommand("first", {});
const second = judgeCommand("second", {});
if (first.action !== "refuse" || second.action !== "refuse" || second.source !== "current") {
	throw new Error(`unexpected verdicts: ${JSON.stringify({ first, second })}`);
}
