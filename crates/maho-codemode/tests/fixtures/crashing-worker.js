import { parentPort } from "node:worker_threads";

parentPort.once("message", () => {
    throw new TypeError("worker crash fixture");
});
parentPort.postMessage({ type: "ready" });
