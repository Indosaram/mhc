import { captureGlobalBaseline, largestGlobals } from "./worker-global-sizes.js";
import { createHeapProbe } from "./worker-heap.js";

const MIN_GROWTH_BYTES = 64 * 1024 * 1024;
const GROWTH_RATIO = 0.25;
const NOTICE_GROWTH_RATIO = 1.25;
const IDLE_COLLECT_DELAY_MS = 1_000;
const COLLECT_RATE_FLOOR = 20;

export function createWorkerMemory(thresholds, onIdleCollected, { heap = createHeapProbe(), now = () => performance.now() } = {}) {
	let baseline = new Set();
	let lastLive = 0;
	let noticeRef = 0;
	let lastCollectEnd = Number.NEGATIVE_INFINITY;
	let lastCollectMs = 0;
	let idleTimer = null;
	function collect() {
		const started = now(); const live = heap.collect();
		lastCollectEnd = now(); lastCollectMs = lastCollectEnd - started; lastLive = live;
		if (live < thresholds.noticeBytes / 2) noticeRef = 0;
		return live;
	}
	function cancelIdle() { if (idleTimer !== null) clearTimeout(idleTimer); idleTimer = null; }
	function scheduleIdle() {
		if (!heap.idleCollection || thresholds.gcWatermarkBytes === 0 || lastLive < thresholds.gcWatermarkBytes) return;
		const floorEnd = lastCollectEnd + COLLECT_RATE_FLOOR * lastCollectMs;
		idleTimer = setTimeout(() => { idleTimer = null; onIdleCollected({ liveBytes: Math.round(collect()), measure: "heap" }); }, Math.max(IDLE_COLLECT_DELAY_MS, floorEnd - now()));
		idleTimer.unref?.();
	}
	return {
		captureBaseline() { baseline = captureGlobalBaseline(); },
		cancelIdle,
		afterCell() {
			cancelIdle();
			let liveBytes = heap.estimate();
			const gcRan = heap.canCollect && (thresholds.ceilingBytes > 0 && liveBytes >= thresholds.ceilingBytes || thresholds.gcWatermarkBytes > 0 && liveBytes >= thresholds.gcWatermarkBytes && liveBytes > lastLive + Math.max(MIN_GROWTH_BYTES, lastLive * GROWTH_RATIO) || thresholds.noticeBytes > 0 && liveBytes >= thresholds.noticeBytes && (lastLive < thresholds.noticeBytes || noticeRef === 0 || liveBytes >= noticeRef * NOTICE_GROWTH_RATIO));
			if (gcRan) liveBytes = collect();
			const noticeLevel = gcRan && thresholds.noticeBytes > 0 && liveBytes >= thresholds.noticeBytes;
			if (noticeLevel && (noticeRef === 0 || liveBytes >= noticeRef * NOTICE_GROWTH_RATIO)) noticeRef = liveBytes;
			const globals = gcRan && ((thresholds.noticeBytes > 0 && liveBytes >= thresholds.noticeBytes) || (thresholds.ceilingBytes > 0 && liveBytes >= thresholds.ceilingBytes)) ? largestGlobals(baseline, 5) : [];
			scheduleIdle();
			return { liveBytes: Math.round(liveBytes), measure: "heap", ...(gcRan ? { gcRan: true } : {}), ...(globals.length ? { globals } : {}) };
		},
	};
}
