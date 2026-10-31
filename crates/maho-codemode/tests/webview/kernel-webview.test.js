// Ports senpi test/js-kernel-webview.test.ts, js-kernel-webview-lifecycle.test.ts and
// js-kernel-webview-resilience.test.ts onto the relay process that hosts an mhc kernel worker.
import { afterAll, afterEach, beforeAll, describe, expect, it } from "bun:test";
import { connectWebViewService, KernelWebViewClients, mainThreadWebViewService } from "../../assets/kernels/js/webview-host.js";
import { alive, bunChromeChildren, bunWebViewAvailable, RelayKernel, serveFixturePage } from "./fixtures.js";

const BUDGET = 60_000;
let page;
const kernels = [];

function openKernel() {
	const kernel = new RelayKernel();
	kernels.push(kernel);
	return kernel;
}

const openView = () =>
	[
		`globalThis.view = new Bun.WebView({ backend: "chrome", width: 320, height: 240 });`,
		`await view.navigate(${JSON.stringify(page.url)});`,
		`return await view.evaluate("document.getElementById('greeting').textContent");`,
	].join("\n");

function request(port, message) {
	return new Promise((resolve) => {
		const onMessage = (reply) => {
			if (reply.kind !== "reply" || reply.id !== message.id) return;
			port.off("message", onMessage);
			resolve(reply);
		};
		port.on("message", onMessage);
		port.postMessage(message);
	});
}

describe("KernelWebViewClients grant/release loop", () => {
	function fakeConnection() {
		const released = Promise.withResolvers();
		return { connection: { clientId: crypto.randomUUID(), port: { tag: "port" }, release: async () => released.resolve() }, released: released.promise };
	}

	it("grants a port for webview-connect and passes it in the transfer list", async () => {
		const { connection } = fakeConnection();
		const posted = Promise.withResolvers();
		const clients = new KernelWebViewClients((message, transfer) => posted.resolve({ message, transfer }), async () => connection);
		expect(clients.consume({ type: "result", cellId: "c" })).toBe(false);
		expect(clients.consume({ type: "webview-connect", requestId: "r1" })).toBe(true);
		const { message, transfer } = await posted.promise;
		expect(message).toEqual({ type: "webview-port", requestId: "r1", ok: true, port: connection.port });
		expect(transfer).toEqual([connection.port]);
	});

	it("releases every granted connection when the generation retires", async () => {
		const { connection, released } = fakeConnection();
		const posted = Promise.withResolvers();
		const clients = new KernelWebViewClients(() => posted.resolve(), async () => connection);
		clients.consume({ type: "webview-connect", requestId: "r1" });
		await posted.promise;
		await clients.release();
		await released;
	});

	it("releases a connection granted after retirement instead of handing it to the dead worker", async () => {
		const { connection, released } = fakeConnection();
		const connecting = Promise.withResolvers();
		const posts = [];
		const clients = new KernelWebViewClients((message) => posts.push(message), () => connecting.promise);
		clients.consume({ type: "webview-connect", requestId: "late" });
		await clients.release();
		connecting.resolve(connection);
		await released;
		expect(posts).toEqual([]);
	});

	it("answers a failed connect with the error, and stays silent once retired", async () => {
		const posted = Promise.withResolvers();
		const clients = new KernelWebViewClients((message) => posted.resolve(message), async () => {
			throw new Error("Bun.WebView is not available in this runtime");
		});
		clients.consume({ type: "webview-connect", requestId: "r1" });
		expect(await posted.promise).toMatchObject({ type: "webview-port", requestId: "r1", ok: false, error: { message: "Bun.WebView is not available in this runtime" } });
		const retired = [];
		const quiet = new KernelWebViewClients((message) => retired.push(message), async () => {
			throw new Error("gone");
		});
		await quiet.release();
		quiet.consume({ type: "webview-connect", requestId: "r2" });
		await quiet.release();
		expect(retired).toEqual([]);
	});
});

describe.skipIf(!bunWebViewAvailable)("Bun.WebView from an eval cell", () => {
	beforeAll(async () => {
		page = await serveFixturePage();
	});

	afterEach(async () => {
		await Promise.allSettled(kernels.splice(0).map((kernel) => kernel.close()));
	}, BUDGET);

	afterAll(async () => {
		await page?.stop();
		page = undefined;
	});

	it(
		"drives a chrome-backed view: navigate, evaluate, click, screenshot",
		async () => {
			const value = await openKernel().value(
				[
					`const view = new Bun.WebView({ backend: "chrome", width: 640, height: 480 });`,
					`await view.navigate(${JSON.stringify(page.url)});`,
					`const greeting = await view.evaluate("document.getElementById('greeting').textContent");`,
					`await view.click("#go");`,
					`const title = await view.evaluate("document.title");`,
					`const shot = await view.screenshot();`,
					`const bytes = new Uint8Array(await shot.arrayBuffer());`,
					`const result = { greeting, title, url: view.url, isBlob: shot instanceof Blob, type: shot.type, png: bytes[0] === 0x89 && bytes[1] === 0x50 };`,
					`view.close();`,
					`return result;`,
				].join("\n"),
			);
			expect(value).toMatchObject({ greeting: "hello from the fixture", title: "clicked", url: page.url, isBlob: true, type: "image/png", png: true });
		},
		BUDGET,
	);

	it(
		"serves the default backend: native WebKit in the worker on macOS, the main-thread Chrome elsewhere",
		async () => {
			const value = await openKernel().value(
				[
					`await using view = new Bun.WebView({ width: 320, height: 240 });`,
					`await view.navigate(${JSON.stringify(page.url)});`,
					`const greeting = await view.evaluate("document.getElementById('greeting').textContent");`,
					`return { greeting, proxied: Object.getPrototypeOf(view) === Bun.WebView.prototype, isWebView: view instanceof Bun.WebView };`,
				].join("\n"),
			);
			expect(value).toEqual({ greeting: "hello from the fixture", proxied: process.platform !== "darwin", isWebView: true });
		},
		BUDGET,
	);

	it(
		"keeps one kernel's views out of another kernel's reach",
		async () => {
			const owner = openKernel();
			const other = openKernel();
			expect(await owner.value(openView())).toBe("hello from the fixture");
			expect(await other.value(openView())).toBe("hello from the fixture");
			await other.value("Bun.WebView.closeAll(); return true;");
			expect(await owner.value(`return await view.evaluate("document.title")`)).toBe("kernel-webview");
		},
		BUDGET,
	);

	it(
		"closes the views a kernel left open when its worker exits, leaving no Chrome",
		async () => {
			const kernel = openKernel();
			expect(await kernel.value(openView())).toBe("hello from the fixture");
			const chrome = await bunChromeChildren(kernel.pid);
			expect(chrome.length).toBeGreaterThan(0);
			kernels.splice(kernels.indexOf(kernel), 1);
			expect(await kernel.close()).toBe(0);
			expect(chrome.filter(alive)).toEqual([]);
		},
		BUDGET,
	);

	it(
		"rejects instead of hanging when Chrome dies, and serves a new view afterwards",
		async () => {
			const kernel = openKernel();
			expect(await kernel.value(openView())).toBe("hello from the fixture");
			const [chrome] = await bunChromeChildren(kernel.pid);
			process.kill(chrome, "SIGKILL");
			const afterCrash = await kernel.value(`try { await view.evaluate("1 + 1"); return "resolved"; } catch (error) { return "rejected: " + error.message; }`);
			expect(afterCrash).toMatch(/^rejected: /u);
			expect(await kernel.value(`view.close();\n${openView()}`)).toBe("hello from the fixture");
		},
		BUDGET,
	);
});

describe.skipIf(!bunWebViewAvailable)("the main-thread WebView service", () => {
	beforeAll(async () => {
		page = await serveFixturePage();
	});

	afterAll(async () => {
		await page?.stop();
		page = undefined;
	});

	it(
		"answers a raw port that names another client's view as unknown, and leaves that view alive",
		async () => {
			const owner = await connectWebViewService();
			const intruder = await connectWebViewService();
			try {
				const viewId = "owner-view";
				const created = await request(owner.port, { kind: "create", id: 1, viewId, options: { backend: "chrome" }, captureConsole: false });
				expect(created.ok).toBe(true);
				const drive = await request(intruder.port, { kind: "call", id: 1, viewId, method: "navigate", args: [page.url] });
				expect(drive).toMatchObject({ ok: false, error: { message: expect.stringContaining("Unknown WebView") } });
				intruder.port.postMessage({ kind: "close", viewId });
				await intruder.release();
				const still = await request(owner.port, { kind: "call", id: 2, viewId, method: "navigate", args: [page.url] });
				expect(still.ok).toBe(true);
			} finally {
				await owner.release();
				await intruder.release();
				owner.port.close();
				intruder.port.close();
			}
			expect(mainThreadWebViewService().viewCount).toBe(0);
			expect(await bunChromeChildren()).toEqual([]);
		},
		BUDGET,
	);
});
