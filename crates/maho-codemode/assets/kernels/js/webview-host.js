// The main-thread Bun.WebView service for one eval kernel, run by the bun relay process that hosts the kernel's
// worker thread (see src/kernels/js/worker_host.rs). Port of senpi packages/senpi-codemode/src/kernels/js/webview-host.ts
// (KernelWebViewClients) and packages/coding-agent/src/core/webview/{webview-wire,webview-readiness,webview-client,
// webview-service,native-webview,bun-chrome}.ts. The kernel worker's end is worker-webview.js / worker-webview-client.js;
// keep the message shapes in sync with them.
//
// mhc runs every kernel in its own bun process, so that process's main thread plays the part of the senpi host main
// thread: the RPC session-worker broker (webview-broker.ts) has no counterpart, and connectWebViewService only knows
// the main-thread route.

import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { appendFileSync } from "node:fs";
import { isMainThread, MessageChannel } from "node:worker_threads";

// ---- webview-wire.ts ----

export const WEBVIEW_METHODS = [
	"navigate",
	"evaluate",
	"screenshot",
	"click",
	"type",
	"press",
	"scroll",
	"scrollTo",
	"resize",
	"back",
	"forward",
	"reload",
	"cdp",
];

function isRecord(value) {
	return typeof value === "object" && value !== null;
}

/** Parses one message from a kernel worker; anything else is dropped at this boundary. */
export function parseClientMessage(value) {
	if (!isRecord(value)) return undefined;
	const { kind, id, viewId } = value;
	if (kind === "close-all") return typeof id === "number" ? { kind, id } : undefined;
	if (typeof viewId !== "string") return undefined;
	if (kind === "close") return { kind, viewId };
	if (kind === "listen") return typeof value.event === "string" ? { kind, viewId, event: value.event } : undefined;
	if (typeof id !== "number") return undefined;
	if (kind === "create") {
		const options = isRecord(value.options) ? value.options : {};
		return { kind, id, viewId, options, captureConsole: value.captureConsole === true };
	}
	if (kind === "call" && WEBVIEW_METHODS.includes(value.method) && Array.isArray(value.args)) {
		return { kind, id, viewId, method: value.method, args: value.args };
	}
	return undefined;
}

export function wireError(error) {
	if (!(error instanceof Error)) return { name: "Error", message: String(error) };
	const code = Reflect.get(error, "code");
	return { name: error.name, message: error.message, ...(typeof code === "string" ? { code } : {}) };
}

// ---- webview-readiness.ts ----

// Healthy first navigations on windows-latest measured p95 3.4 s and at most 18 s (the first Chrome
// of a cold runner); two bounded launches fit inside the eval cell's 60 s default budget.
export const DEFAULT_READINESS = { attachBoundMs: 20_000, launchAttempts: 2 };

export const READINESS_URL = "about:blank";

/** Appends one line per launch outcome to the file `SENPI_WEBVIEW_READINESS_LOG` names (CI diagnostics). */
export function readinessLogFromEnvironment() {
	const path = process.env.SENPI_WEBVIEW_READINESS_LOG;
	if (!path) return undefined;
	return (event) => {
		const outcome =
			event.type === "ready"
				? `ready launch=${event.launch} attachMs=${event.attachMs}`
				: `stalled phase=${event.phase} launch=${event.launch} (Chrome retired)`;
		appendFileSync(path, `${new Date().toISOString()} pid=${process.pid} webview ${outcome}\n`);
	};
}

/** Appends one line per failed Chrome retirement to the same CI diagnostics file (senpi#2993). */
export function retireFailureLogFromEnvironment() {
	const path = process.env.SENPI_WEBVIEW_READINESS_LOG;
	if (!path) return undefined;
	return (message) => {
		appendFileSync(path, `${new Date().toISOString()} pid=${process.pid} webview retire-failed ${JSON.stringify(message)}\n`);
	};
}

export class WebViewNotReadyError extends Error {
	code = "ERR_WEBVIEW_NOT_READY";
	phase = "cdp-target-attach";

	constructor(launches, policy) {
		super(
			`Chrome-backed WebView never became ready: phase cdp-target-attach (Chrome did not attach a CDP session to the new view within ${policy.attachBoundMs} ms) on ${launches} launch(es); each stalled Chrome was retired`,
		);
		this.name = "WebViewNotReadyError";
	}
}

export function timedDeadline(boundMs) {
	let timer;
	const reached = new Promise((resolve) => {
		timer = setTimeout(resolve, boundMs);
	});
	return { reached, cancel: () => clearTimeout(timer) };
}

/** Resolves the readiness navigation's duration once it settled, undefined when the deadline came first. */
export async function attachedWithin(view, deadline) {
	const startedAt = performance.now();
	const bound = deadline.reached.then(() => undefined);
	// Bun throws synchronously for some invalid states; the async wrapper turns that into a rejection.
	const navigation = (async () => await view.navigate(READINESS_URL))().then(() => Math.round(performance.now() - startedAt));
	try {
		return await Promise.race([navigation, bound]);
	} finally {
		deadline.cancel();
		// A stalled navigation is abandoned; closing its view rejects it with nobody left waiting.
		navigation.catch(() => {});
	}
}

// ---- native-webview.ts ----

/** `Bun.WebView` when this thread may construct Chrome-backed views: Bun runtime, process main thread. */
export function mainThreadWebViewClass() {
	if (!isMainThread) return undefined;
	const bun = Reflect.get(globalThis, "Bun");
	if (typeof bun !== "object" || bun === null) return undefined;
	const webView = Reflect.get(bun, "WebView");
	return typeof webView === "function" && typeof Reflect.get(webView, "closeAll") === "function" ? webView : undefined;
}

// ---- bun-chrome.ts ----

// Bun launches its Chrome with exactly this default flag run (see `Bun.WebView` backend docs);
// together with the parent pid it tells Bun's browser apart from any other Chrome we spawned.
const BUN_CHROME_FLAGS = "--remote-debugging-pipe --headless --no-first-run --no-default-browser-check";
const BROWSER_IMAGE = /chrom|msedge|brave/iu;
const EXIT_DEADLINE_MS = 5_000;
const EXIT_POLL_MS = 25;

function run(file, args) {
	return new Promise((resolve) => {
		execFile(file, [...args], { maxBuffer: 16 * 1024 * 1024, windowsHide: true }, (_error, stdout) => resolve(String(stdout ?? "")));
	});
}

function positivePids(texts) {
	return texts.map(Number).filter((pid) => Number.isInteger(pid) && pid > 0);
}

async function bunChromePids() {
	const pids = [];
	for (const line of (await run("ps", ["-axo", "pid=,ppid=,command="])).split("\n")) {
		const [pidText = "", ppidText, ...command] = line.trim().split(/\s+/u);
		if (Number(ppidText) === process.pid && command.join(" ").includes(BUN_CHROME_FLAGS)) pids.push(pidText);
	}
	return positivePids(pids);
}

function alive(pid) {
	try {
		process.kill(pid, 0);
		return true;
	} catch (error) {
		return error instanceof Error && Reflect.get(error, "code") === "EPERM";
	}
}

async function waitForExit(pids) {
	const deadline = Date.now() + EXIT_DEADLINE_MS;
	let remaining = pids.filter(alive);
	while (remaining.length > 0 && Date.now() < deadline) {
		await new Promise((resolve) => setTimeout(resolve, EXIT_POLL_MS));
		remaining = remaining.filter(alive);
	}
}

// A zombie keeps no command line, so a dead browser child is recognized by its name alone.
async function deadBrowserChildren() {
	if (process.platform === "win32") return [];
	const pids = [];
	for (const line of (await run("ps", ["-axo", "pid=,ppid=,stat=,comm="])).split("\n")) {
		const [pidText = "", ppidText, stat = "", ...command] = line.trim().split(/\s+/u);
		if (Number(ppidText) === process.pid && stat.startsWith("Z") && BROWSER_IMAGE.test(command.join(" "))) pids.push(pidText);
	}
	return positivePids(pids);
}

/**
 * Resolves once no killed-but-unreaped Chrome child is left (bounded). A Chrome that died (crash,
 * kill) stays a zombie until Bun reaps it, and Chrome's profile lock still names that pid, so a
 * Chrome Bun launches in that window exits at once ("Chrome process closed the pipe").
 */
export async function settleDeadBunChrome() {
	await waitForExit(await deadBrowserChildren());
}

/**
 * Ends the Chrome Bun spawned once no proxied view needs it; the next Chrome-backed view respawns it.
 * `WebView.closeAll()` would do this, but on macOS it also kills the shared WebKit host that native
 * worker views (the macOS default backend) run on, so there only Bun's own Chrome child is killed.
 * Resolves once those processes are gone (bounded), so a released kernel leaves no Chrome behind.
 */
export async function retireBunChrome(webViewClass) {
	// Port gap: upstream lists the Windows process table (windows-chrome-tree.ts), kills Bun's whole Chrome tree with
	// taskkill and waits for its removal; this port only calls closeAll() there. The relay process that owns this
	// Chrome is itself retired by the Rust host's process-tree termination.
	if (process.platform === "win32") {
		webViewClass.closeAll();
		return;
	}
	const pids = await bunChromePids();
	if (process.platform === "darwin") {
		for (const pid of pids) {
			try {
				process.kill(pid, "SIGKILL");
			} catch {
				// Already gone between the listing and the kill.
			}
		}
	} else {
		webViewClass.closeAll();
	}
	await waitForExit(pids);
}

// ---- webview-client.ts ----

function stateOf(view) {
	return { url: view.url, title: view.title, loading: view.loading };
}

export function closeQuietly(view) {
	try {
		view.close();
	} catch {
		// A view whose browser already died has nothing left to close.
	}
}

/**
 * One kernel's capability: the views it created and the only port that can reach them. Nothing is
 * addressable across clients, so one client cannot see, drive, or close another's views.
 */
export class WebViewServiceClient {
	id;
	owner;
	#port;
	#host;
	#views = new Map();
	#launches = new Set();
	#released = false;

	constructor(id, owner, port, host) {
		this.id = id;
		this.owner = owner;
		this.#port = port;
		this.#host = host;
		port.on("message", (message) => void this.#handle(parseClientMessage(message)));
		port.on("close", () => this.#host.onClientClosed(this));
		port.unref?.();
	}

	get viewCount() {
		return this.#views.size;
	}

	/** Resolves once every launch this client started has been adopted or has retired its Chrome. */
	async settled() {
		await Promise.allSettled([...this.#launches]);
	}

	release() {
		if (this.#released) return;
		this.#released = true;
		for (const view of this.#views.values()) closeQuietly(view);
		this.#views.clear();
		this.#port.close();
	}

	async #handle(message) {
		if (!message || this.#released) return;
		switch (message.kind) {
			case "create":
				await this.#reply(message.id, async () => {
					const view = await this.#create(message.viewId, message.options, message.captureConsole);
					return { value: undefined, view };
				});
				return;
			case "call":
				await this.#reply(message.id, async () => {
					const view = this.#view(message.viewId);
					const method = Reflect.get(view, message.method);
					if (typeof method !== "function") throw new TypeError(`WebView.${message.method} is not available`);
					const value = await Reflect.apply(method, view, [...message.args]);
					return { value, view };
				});
				return;
			case "close": {
				const view = this.#views.get(message.viewId);
				this.#views.delete(message.viewId);
				if (view) closeQuietly(view);
				return;
			}
			case "listen": {
				const view = this.#views.get(message.viewId);
				view?.addEventListener(message.event, (event) => {
					this.#emit(message.viewId, { type: "dom", name: message.event, data: Reflect.get(event, "data") });
				});
				return;
			}
			case "close-all":
				for (const view of this.#views.values()) closeQuietly(view);
				this.#views.clear();
				this.#post({ kind: "reply", id: message.id, ok: true, value: undefined });
				return;
		}
	}

	async #create(viewId, options, captureConsole) {
		if (this.#views.has(viewId)) throw new Error(`WebView ${viewId} already exists`);
		const onConsole = captureConsole ? (...args) => this.#emit(viewId, { type: "console", args }) : undefined;
		const launch = this.#host.createView(
			options,
			onConsole,
			(view) => this.#adopt(viewId, view),
			() => !this.#released,
		);
		this.#launches.add(launch);
		try {
			return await launch;
		} finally {
			this.#launches.delete(launch);
		}
	}

	#adopt(viewId, view) {
		if (this.#released) return false;
		view.onNavigated = (url, title) => this.#emit(viewId, { type: "navigated", url, title });
		view.onNavigationFailed = (error) => this.#emit(viewId, { type: "navigation-failed", error: wireError(error) });
		this.#views.set(viewId, view);
		return true;
	}

	#view(viewId) {
		const view = this.#views.get(viewId);
		if (view) return view;
		const error = new Error(`Unknown WebView ${viewId}: it was closed or belongs to another kernel`);
		Reflect.set(error, "code", "ERR_INVALID_STATE");
		throw error;
	}

	async #reply(id, work) {
		try {
			const { value, view } = await work();
			this.#post({ kind: "reply", id, ok: true, value, state: stateOf(view) });
		} catch (error) {
			this.#post({ kind: "reply", id, ok: false, error: wireError(error) });
		}
	}

	#emit(viewId, event) {
		this.#post({ kind: "event", viewId, event });
	}

	#post(message) {
		if (this.#released) return;
		try {
			this.#port.postMessage(message);
		} catch (error) {
			if (message.kind !== "reply" || !message.ok) return;
			this.#port.postMessage({ kind: "reply", id: message.id, ok: false, error: wireError(error) });
		}
	}
}

// ---- webview-service.ts ----

// Right after Chrome dies, Windows refuses to relaunch it for about a second (Bun reports
// ERR_DLOPEN_FAILED "Failed to spawn Chrome"); a missing Chrome fails the same way, so the
// retry is bounded and the last error is surfaced.
const RELAUNCH_ATTEMPTS = 8;
const RELAUNCH_RETRY_MS = 250;

function isChromeRelaunchWindow(error) {
	return error instanceof Error && Reflect.get(error, "code") === "ERR_DLOPEN_FAILED";
}

/**
 * Serves Chrome-backed `Bun.WebView`s on the process main thread (Bun allows that backend nowhere
 * else) for the kernel worker thread. Every grant is its own client: a private MessagePort plus the
 * views created through it, released as a unit by the owner that asked.
 *
 * options: readiness (Partial<{attachBoundMs, launchAttempts}>), onReadiness(event), attachDeadline(launch),
 * retireChrome(webViewClass), onRetireFailure(message) - the same seams the upstream tests drive.
 */
export class WebViewService {
	#webViewClass;
	#readiness;
	#onReadiness;
	#attachDeadline;
	#retireChrome;
	#onRetireFailure;
	#clients = new Map();
	#retiring = Promise.resolve();
	#chromeInUse = false;
	// Views being constructed and not yet adopted by their client: Chrome must outlive them.
	#launching = 0;

	constructor(webViewClass, options = {}) {
		this.#webViewClass = webViewClass;
		this.#readiness = { ...DEFAULT_READINESS, ...options.readiness };
		this.#onReadiness = options.onReadiness ?? (() => {});
		const boundMs = this.#readiness.attachBoundMs;
		this.#attachDeadline = options.attachDeadline ?? (() => timedDeadline(boundMs));
		this.#retireChrome = options.retireChrome ?? retireBunChrome;
		this.#onRetireFailure = options.onRetireFailure;
	}

	get viewCount() {
		let count = 0;
		for (const client of this.#clients.values()) count += client.viewCount;
		return count;
	}

	connect(owner) {
		const clientId = randomUUID();
		const channel = new MessageChannel();
		const client = new WebViewServiceClient(clientId, owner, channel.port1, {
			createView: (options, onConsole, adopt, wanted) => this.#createView(options, onConsole, adopt, wanted),
			// A failed retirement was already reported by the chain; this path has no caller to hand it to.
			onClientClosed: (closed) => void this.#drop(closed).catch(() => {}),
		});
		this.#clients.set(clientId, client);
		return { clientId, port: channel.port2 };
	}

	/**
	 * Only the owner that connected a client can release it. Resolves after the client's in-flight
	 * launches settled and any Chrome retirement in flight ended, including one a closed port already started.
	 */
	async release(clientId, owner) {
		const client = this.#clients.get(clientId);
		if (client?.owner === owner) await this.#drop(client);
		await this.#retiring;
	}

	async releaseOwner(owner) {
		const owned = [...this.#clients.values()].filter((client) => client.owner === owner);
		await Promise.all(owned.map((client) => this.#drop(client)));
		await this.#retiring;
	}

	/**
	 * Launches a view, waits until it is ready, and hands it to `adopt` in the same turn, so no
	 * retirement can slip between the launch and the client's bookkeeping. A launch that fails, never
	 * becomes ready, or whose client was released while it was in flight, retires the Chrome it started
	 * unless another view still needs it; a launch that never became ready is retried on a fresh
	 * Chrome before the create fails naming the phase it stalled in.
	 */
	async #createView(options, onConsole, adopt, wanted) {
		const viewOptions = onConsole ? { ...options, console: onConsole } : options;
		for (let launch = 1; ; launch++) {
			this.#launching++;
			let view;
			try {
				view = await this.#launchReady(viewOptions, launch);
			} catch (error) {
				this.#launching--;
				await this.#retireIfIdle();
				throw error;
			}
			this.#launching--;
			if (view) {
				if (adopt(view)) return view;
				closeQuietly(view);
				await this.#retireIfIdle();
				throw new Error("WebView client released");
			}
			await this.#retireIfIdle();
			if (!wanted()) throw new Error("WebView client released");
			if (launch >= this.#readiness.launchAttempts) throw new WebViewNotReadyError(launch, this.#readiness);
		}
	}

	async #launchReady(viewOptions, launch) {
		const view = await this.#launch(viewOptions);
		let attachMs;
		try {
			attachMs = await attachedWithin(view, this.#attachDeadline(launch));
		} catch (error) {
			closeQuietly(view);
			throw error;
		}
		if (attachMs !== undefined) {
			this.#onReadiness({ type: "ready", launch, attachMs });
			return view;
		}
		closeQuietly(view);
		this.#onReadiness({ type: "stalled", phase: "cdp-target-attach", launch });
		return undefined;
	}

	async #launch(viewOptions) {
		// No retirement is scheduled while a launch is pending, so the one awaited here is the last.
		await this.#retiring;
		await settleDeadBunChrome();
		this.#chromeInUse = true;
		for (let attempt = 1; ; attempt++) {
			try {
				return new this.#webViewClass(viewOptions);
			} catch (error) {
				if (!isChromeRelaunchWindow(error) || attempt >= RELAUNCH_ATTEMPTS) throw error;
				await new Promise((resolve) => setTimeout(resolve, RELAUNCH_RETRY_MS));
			}
		}
	}

	async #drop(client) {
		if (this.#clients.get(client.id) !== client) return await this.#retiring;
		this.#clients.delete(client.id);
		client.release();
		await client.settled();
		await this.#retireIfIdle();
	}

	/** Retires Chrome once no view uses it and no launch is pending; resolves when retirement ends. */
	#retireIfIdle() {
		if (!this.#chromeInUse || this.#busy()) return this.#retiring;
		this.#chromeInUse = false;
		const retirement = this.#retiring.then(() => (this.#busy() ? undefined : this.#retireChrome(this.#webViewClass)));
		// senpi#2993: the chain every later launch and retirement awaits must not keep a failure, or one
		// failed retirement would fail every launch after it. The caller of this one still gets the error.
		this.#retiring = retirement.catch((error) => this.#reportRetireFailure(error));
		return retirement;
	}

	#reportRetireFailure(error) {
		const message = error instanceof Error ? error.message : String(error);
		try {
			if (this.#onRetireFailure) {
				this.#onRetireFailure(message);
				return;
			}
		} catch {
			// The diagnostics log can be the thing that failed; fall through to the process warning.
		}
		process.emitWarning(`WebView Chrome retirement failed: ${message}`, { code: "SENPI_WEBVIEW_RETIRE_FAILED" });
	}

	#busy() {
		return this.viewCount > 0 || this.#launching > 0;
	}
}

const SERVICE_KEY = Symbol.for("senpi.webview.service");

/** The process-wide service, created on first use; undefined off the main thread or without `Bun.WebView`. */
export function mainThreadWebViewService() {
	const existing = Reflect.get(globalThis, SERVICE_KEY);
	if (existing instanceof WebViewService) return existing;
	const webViewClass = mainThreadWebViewClass();
	if (!webViewClass) return undefined;
	const onReadiness = readinessLogFromEnvironment();
	const onRetireFailure = retireFailureLogFromEnvironment();
	const service = new WebViewService(webViewClass, {
		...(onReadiness ? { onReadiness } : {}),
		...(onRetireFailure ? { onRetireFailure } : {}),
	});
	Reflect.set(globalThis, SERVICE_KEY, service);
	return service;
}

// ---- webview-broker.ts (main-thread route only) ----

/** Connects a kernel to this process's main-thread WebView service; rejects when the runtime has none. */
export async function connectWebViewService() {
	if (!isMainThread) throw new Error("no route from this worker thread to the main-thread WebView service");
	const service = mainThreadWebViewService();
	if (!service) throw new Error("Bun.WebView is not available in this runtime");
	const owner = {};
	const grant = service.connect(owner);
	return { ...grant, release: () => service.release(grant.clientId, owner) };
}

// ---- senpi-codemode kernels/js/webview-host.ts ----

function bridgeError(error) {
	return { message: error.message, name: error.name, stack: error.stack };
}

/**
 * The WebView clients one worker generation asked for. Chrome-backed `Bun.WebView`s only run on the
 * process main thread, so the worker gets a private port to the main-thread service; retiring the
 * generation (reset, close, crash, timeout) releases every view created through those ports.
 */
export class KernelWebViewClients {
	#post;
	#connect;
	#connections = new Set();
	#retired = false;

	constructor(post, connect = connectWebViewService) {
		this.#post = post;
		this.#connect = connect;
	}

	consume(message) {
		if (message?.type !== "webview-connect") return false;
		void this.#grant(message.requestId);
		return true;
	}

	async release() {
		this.#retired = true;
		const connections = [...this.#connections];
		this.#connections.clear();
		await Promise.allSettled(connections.map((connection) => connection.release()));
	}

	async #grant(requestId) {
		let connection;
		try {
			connection = await this.#connect();
		} catch (error) {
			const cause = error instanceof Error ? error : new Error(String(error));
			if (!this.#retired) this.#post({ type: "webview-port", requestId, ok: false, error: bridgeError(cause) }, []);
			return;
		}
		if (this.#retired) {
			await connection.release();
			return;
		}
		this.#connections.add(connection);
		this.#post({ type: "webview-port", requestId, ok: true, port: connection.port }, [connection.port]);
	}
}
