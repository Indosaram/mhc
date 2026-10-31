// Ports senpi test/js-kernel-webview-readiness.test.ts and js-kernel-webview-launch.test.ts onto webview-host.js.
import { afterAll, afterEach, beforeAll, describe, expect, it } from "bun:test";
import { mainThreadWebViewClass, retireBunChrome, WebViewService } from "../../assets/kernels/js/webview-host.js";
import { bunChromeChildren, bunWebViewAvailable, serveFixturePage } from "./fixtures.js";

let page;
const owner = {};
const grants = [];

function nativeClass() {
	const native = mainThreadWebViewClass();
	if (!native) throw new Error("expected Bun.WebView on the main thread");
	return native;
}

/**
 * Real Chrome launches; the launches `stalls` picks never settle their first navigation. A launch's
 * readiness bound is reached the moment its stalled navigation starts, and never for a healthy one,
 * so no case depends on how fast Chrome attaches.
 */
function stallingLaunches(stalls) {
	let launches = 0;
	let current;
	const webViewClass = new Proxy(nativeClass(), {
		construct(target, args) {
			launches += 1;
			const view = Reflect.construct(target, args);
			if (stalls(launches)) {
				Object.defineProperty(view, "navigate", {
					value: () => {
						current?.();
						return new Promise(() => {});
					},
				});
			}
			return view;
		},
	});
	const attachDeadline = () => {
		const reached = Promise.withResolvers();
		current = reached.resolve;
		return { reached: reached.promise, cancel: () => {} };
	};
	return { webViewClass, attachDeadline, launches: () => launches };
}

function connect(service) {
	const grant = service.connect(owner);
	grants.push({ service, grant });
	return grant.port;
}

let nextId = 1;
function request(port, message) {
	const id = nextId++;
	return new Promise((resolve) => {
		const onMessage = (reply) => {
			if (reply.kind !== "reply" || reply.id !== id) return;
			port.off("message", onMessage);
			resolve(reply);
		};
		port.on("message", onMessage);
		port.postMessage({ ...message, id });
	});
}

const create = (port, viewId) => request(port, { kind: "create", viewId, options: { backend: "chrome" }, captureConsole: false });

async function greetingAfterNavigate(port, viewId) {
	expect(await request(port, { kind: "call", viewId, method: "navigate", args: [page.url] })).toMatchObject({ ok: true });
	const read = await request(port, { kind: "call", viewId, method: "evaluate", args: ["document.getElementById('greeting').textContent"] });
	return read.value;
}

describe.skipIf(!bunWebViewAvailable)("a Chrome launch that never becomes ready", () => {
	beforeAll(async () => {
		page = await serveFixturePage();
	});

	// Release resolves once the service's Chrome retirement ended; anything left is a leak.
	afterEach(async () => {
		await Promise.allSettled(grants.splice(0).map(({ service, grant }) => service.release(grant.clientId, owner)));
		const leaked = await bunChromeChildren();
		if (leaked.length > 0) await retireBunChrome(nativeClass());
		expect(leaked).toEqual([]);
	}, 60_000);

	afterAll(async () => {
		await page?.stop();
	});

	it(
		"retires the stalled launch and answers the create from a fresh Chrome",
		async () => {
			const events = [];
			const { webViewClass, attachDeadline, launches } = stallingLaunches((launch) => launch === 1);
			const service = new WebViewService(webViewClass, { attachDeadline, onReadiness: (event) => events.push(event) });
			const port = connect(service);
			expect(await create(port, "view")).toMatchObject({ ok: true });
			expect(await greetingAfterNavigate(port, "view")).toBe("hello from the fixture");
			expect(launches()).toBe(2);
			expect(events.map((event) => [event.type, event.launch])).toEqual([
				["stalled", 1],
				["ready", 2],
			]);
		},
		60_000,
	);

	it(
		"fails the create naming the phase, with no Chrome left, once every launch stalled",
		async () => {
			const { webViewClass, attachDeadline, launches } = stallingLaunches(() => true);
			const service = new WebViewService(webViewClass, { attachDeadline });
			const reply = await create(connect(service), "view");
			expect(reply).toMatchObject({ ok: false, error: { code: "ERR_WEBVIEW_NOT_READY" } });
			expect(reply.error.message).toContain("phase cdp-target-attach");
			expect(launches()).toBe(2);
			expect(await bunChromeChildren()).toEqual([]);
			expect(service.viewCount).toBe(0);
		},
		60_000,
	);

	it(
		"keeps another kernel's ready view on the shared Chrome while a stalled create fails",
		async () => {
			const { webViewClass, attachDeadline, launches } = stallingLaunches((launch) => launch > 1);
			const service = new WebViewService(webViewClass, { attachDeadline });
			const healthy = connect(service);
			const stalled = connect(service);
			expect(await create(healthy, "kept")).toMatchObject({ ok: true });
			expect(await create(stalled, "lost")).toMatchObject({ ok: false, error: { code: "ERR_WEBVIEW_NOT_READY" } });
			expect(launches()).toBe(3);
			expect(await greetingAfterNavigate(healthy, "kept")).toBe("hello from the fixture");
			expect(service.viewCount).toBe(1);
		},
		60_000,
	);

	it(
		"does not relaunch for a client released while its launch stalls, and leaves no Chrome behind",
		async () => {
			const { webViewClass, launches } = stallingLaunches(() => true);
			const stalled = Promise.withResolvers();
			const bound = Promise.withResolvers();
			const attachDeadline = () => {
				stalled.resolve();
				return { reached: bound.promise, cancel: () => {} };
			};
			const service = new WebViewService(webViewClass, { attachDeadline });
			const grant = service.connect(owner);
			grant.port.postMessage({ kind: "create", id: 1, viewId: "view", options: { backend: "chrome" }, captureConsole: false });
			await stalled.promise;
			// The kernel goes away mid-stall (cell timeout, reset); only then is the launch's bound reached.
			const released = service.release(grant.clientId, owner);
			bound.resolve();
			await released;
			expect(launches()).toBe(1);
			expect(await bunChromeChildren()).toEqual([]);
			expect(service.viewCount).toBe(0);
		},
		60_000,
	);

	it(
		"retires the Chrome a launch starts after its kernel was released mid-launch",
		async () => {
			const native = nativeClass();
			const launched = Promise.withResolvers();
			let grant;
			let released;
			let attempts = 0;
			// The kernel is released while Chrome refuses to relaunch; the retried launch then starts a Chrome no view uses.
			const launching = new Proxy(native, {
				construct(target, args) {
					attempts += 1;
					if (attempts === 1) {
						if (grant) released = service.release(grant.clientId, owner);
						throw Object.assign(new Error("Failed to spawn Chrome"), { code: "ERR_DLOPEN_FAILED" });
					}
					const view = Reflect.construct(target, args);
					launched.resolve();
					return view;
				},
			});
			const service = new WebViewService(launching);
			grant = service.connect(owner);
			try {
				grant.port.postMessage({ kind: "create", id: 1, viewId: "late", options: { backend: "chrome" }, captureConsole: false });
				await launched.promise;
				await released;
				expect(attempts).toBe(2);
				expect(await bunChromeChildren()).toEqual([]);
			} finally {
				grant.port.close();
				await retireBunChrome(native);
			}
		},
		60_000,
	);
});
