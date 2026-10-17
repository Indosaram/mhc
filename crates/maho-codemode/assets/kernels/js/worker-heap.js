import { getHeapStatistics, setFlagsFromString } from "node:v8";
import { runInNewContext } from "node:vm";

const BUN_WITHOUT_IDLE_COLLECTIONS = "<1.4.3";

export function createHeapProbe() {
	const jsc = typeof globalThis.Bun?.gc === "function" ? process.getBuiltinModule?.("bun:jsc") : undefined;
	if (jsc !== undefined && typeof jsc.heapSize === "function" && typeof jsc.edenGC === "function") {
		return {
			canCollect: true,
			idleCollection: globalThis.Bun.semver.satisfies(globalThis.Bun.version, BUN_WITHOUT_IDLE_COLLECTIONS),
			estimate() { jsc.edenGC(); return jsc.heapSize(); },
			collect() { globalThis.Bun.gc(true); return jsc.heapSize(); },
		};
	}
	const gc = exposedGc();
	const read = () => { const stats = getHeapStatistics(); return stats.used_heap_size + (stats.external_memory ?? 0); };
	return { canCollect: gc !== undefined, idleCollection: gc !== undefined, estimate: read, collect() { gc?.(); gc?.(); return read(); } };
}

function exposedGc() {
	if (typeof globalThis.gc === "function") return globalThis.gc;
	setFlagsFromString("--expose-gc");
	try { const gc = runInNewContext("typeof gc === 'function' ? gc : undefined"); return typeof gc === "function" ? gc : undefined; }
	finally { setFlagsFromString("--no-expose-gc"); }
}
