import { parentPort, workerData } from "node:worker_threads";
import { createWorkerCore } from "./worker-core.js";

if (!parentPort) throw new Error("JS kernel worker missing parentPort");
const exit = process.exit.bind(process);

const transport = {
	send(message) {
		parentPort.postMessage(message);
	},
	onMessage(handler) {
		parentPort.on("message", handler);
		return () => parentPort.off("message", handler);
	},
	close() {
		parentPort.close();
		setTimeout(() => exit(0), 0);
	},
};

createWorkerCore(transport, {
	cwd: workerData.cwd,
	parallelPoolWidth: workerData.parallelPoolWidth,
});
