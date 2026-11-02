#!/usr/bin/env bun
// SPDX-License-Identifier: MIT
// tui-capture.mjs - run a TUI in a real PTY (Bun.Terminal), parse it with
// @xterm/headless, and write terminal.txt / terminal.ansi / cells.json /
// metadata.json / terminal.png (cells rendered in Bun.WebView).
//
// usage: bun tui-capture.mjs (--command "<cmd>" | --from-file <capture.ansi>)
//          --evidence-dir <dir> [--cols 100] [--rows 30] [--input <text|{Enter}|{ArrowDown}|{Esc}>]...
//          [--wait-for <regex>] [--then-wait <regex>] [--timeout <ms>] [--offline-model]
//          [--no-png] [--title <text>]
// Exit codes: 0 ok, 1 capture failed (timeout, child error), 2 usage error or missing dependency.
// Set TUI_CAPTURE_RESOLVE=skill to only look in the skill directory for @xterm/headless.

import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const SKILL_DIR = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const KEYS = { "{Enter}": "\r", "{ArrowDown}": "\x1b[B", "{ArrowUp}": "\x1b[A", "{Esc}": "\x1b", "{Tab}": "\t", "{Backspace}": "\x7f" };
const DEFAULT_WAIT = "\u276f"; // the mhc editor's input marker

function fail(code, message) {
	console.error(`tui-capture: ${message}`);
	process.exit(code);
}

function parseArgs(argv) {
	const opts = { cols: 100, rows: 30, input: [], waitFor: DEFAULT_WAIT, thenWait: null, timeout: 30000, offlineModel: false, png: true, title: "TUI capture", command: null, fromFile: null, evidenceDir: null };
	const value = (i, flag) => {
		const v = argv[i];
		if (v === undefined) fail(2, `missing value for ${flag}`);
		return v;
	};
	for (let i = 0; i < argv.length; i++) {
		const a = argv[i];
		if (a === "--command") opts.command = value(++i, a);
		else if (a === "--from-file") opts.fromFile = resolve(value(++i, a));
		else if (a === "--evidence-dir") opts.evidenceDir = resolve(value(++i, a));
		else if (a === "--cols") opts.cols = Number(value(++i, a));
		else if (a === "--rows") opts.rows = Number(value(++i, a));
		else if (a === "--input") opts.input.push(value(++i, a));
		else if (a === "--wait-for") opts.waitFor = value(++i, a);
		else if (a === "--then-wait") opts.thenWait = value(++i, a);
		else if (a === "--timeout") opts.timeout = Number(value(++i, a));
		else if (a === "--title") opts.title = value(++i, a);
		else if (a === "--offline-model") opts.offlineModel = true;
		else if (a === "--no-png") opts.png = false;
		else fail(2, `unknown argument ${a}`);
	}
	if (!opts.evidenceDir) fail(2, "--evidence-dir is required");
	if (!opts.command === !opts.fromFile) fail(2, "pass exactly one of --command or --from-file");
	for (const n of ["cols", "rows", "timeout"]) if (!Number.isInteger(opts[n]) || opts[n] <= 0) fail(2, `--${n} must be a positive integer`);
	return opts;
}

// Resolution order: (1) the skill dir's node_modules, (2) tools/golden/node_modules
// found by walking up from cwd, (3) the bun install cache ($BUN_INSTALL/install/cache,
// else $HOME/.bun/install/cache), highest @xterm/headless@* version, (4) node_modules
// dirs above this script, then NODE_PATH. Only on-disk packages count, so Bun's
// auto-install never fetches anything.
function bunCacheCandidate() {
	const home = process.env.HOME;
	const cache = process.env.BUN_INSTALL ? join(process.env.BUN_INSTALL, "install/cache") : home ? join(home, ".bun/install/cache") : null;
	const dir = cache && join(cache, "@xterm");
	if (!dir || !existsSync(dir)) return null;
	const version = (name) => name.slice("headless@".length).split("@")[0].split(/[.-]/).map((n) => Number.parseInt(n, 10) || 0);
	const newer = (x, y) => { for (let i = 0; i < Math.max(x.length, y.length); i++) if ((x[i] ?? 0) !== (y[i] ?? 0)) return (x[i] ?? 0) > (y[i] ?? 0); return false; };
	let best = null;
	for (const name of readdirSync(dir)) {
		if (!name.startsWith("headless@") || !existsSync(join(dir, name, "package.json"))) continue;
		if (!best || newer(version(name), version(best))) best = name;
	}
	return best && join(dir, best, "package.json");
}

function loadHeadless() {
	const rel = "node_modules/@xterm/headless/package.json";
	const candidates = [join(SKILL_DIR, rel)];
	const walkUp = (start, sub) => {
		for (let dir = start; ; dir = dirname(dir)) {
			candidates.push(join(dir, sub));
			if (dirname(dir) === dir) break;
		}
	};
	if (process.env.TUI_CAPTURE_RESOLVE !== "skill") {
		walkUp(process.cwd(), join("tools/golden", rel));
		const cached = bunCacheCandidate();
		if (cached) candidates.push(cached);
		walkUp(dirname(fileURLToPath(import.meta.url)), rel);
		for (const dir of (process.env.NODE_PATH ?? "").split(":").filter(Boolean)) candidates.push(join(dir, "@xterm/headless/package.json"));
	}
	for (const pkg of candidates) {
		if (existsSync(pkg)) return createRequire(pkg)(dirname(pkg)).Terminal;
	}
	fail(2, "missing module @xterm/headless (looked in: skill dir node_modules, tools/golden/node_modules above cwd, bun install cache @xterm/headless@*, node_modules above the script, NODE_PATH)");
}

// xterm default palette: 16 ANSI colors, 6x6x6 cube, 24 grays.
const BASE16 = ["#000000", "#cd0000", "#00cd00", "#cdcd00", "#0000ee", "#cd00cd", "#00cdcd", "#e5e5e5", "#7f7f7f", "#ff0000", "#00ff00", "#ffff00", "#5c5cff", "#ff00ff", "#00ffff", "#ffffff"];
const hex = (r, g, b) => "#" + [r, g, b].map((n) => n.toString(16).padStart(2, "0")).join("");
function paletteHex(i) {
	if (i < 16) return BASE16[i];
	if (i < 232) {
		const n = i - 16, step = (v) => (v === 0 ? 0 : 55 + v * 40);
		return hex(step(Math.floor(n / 36)), step(Math.floor(n / 6) % 6), step(n % 6));
	}
	const g = 8 + (i - 232) * 10;
	return hex(g, g, g);
}
function color(cell, which) {
	const v = which === "fg" ? cell.getFgColor() : cell.getBgColor();
	if (which === "fg" ? cell.isFgRGB() : cell.isBgRGB()) return hex((v >> 16) & 255, (v >> 8) & 255, v & 255);
	if (which === "fg" ? cell.isFgPalette() : cell.isBgPalette()) return paletteHex(v);
	return null; // terminal default
}

function viewportCells(vt, cols, rows) {
	const b = vt.buffer.active;
	const out = [];
	for (let y = 0; y < rows; y++) {
		const line = b.getLine(b.viewportY + y);
		const row = [];
		for (let x = 0; x < cols; x++) {
			const c = line?.getCell(x);
			if (!c) { row.push({ char: " ", width: 1, fg: null, bg: null }); continue; }
			let fg = color(c, "fg"), bg = color(c, "bg");
			if (c.isInverse()) [fg, bg] = [bg ?? "#000000", fg ?? "#e5e5e5"];
			row.push({ char: c.getChars() || (c.getWidth() === 0 ? "" : " "), width: c.getWidth(), fg, bg, bold: !!c.isBold() });
		}
		out.push(row);
	}
	return out;
}

const viewportText = (vt, rows) => {
	const b = vt.buffer.active;
	return Array.from({ length: rows }, (_, y) => b.getLine(b.viewportY + y)?.translateToString(true) ?? "").join("\n");
};

const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
function cellsHtml(cells, title) {
	const rows = cells.map((row) => {
		let html = "";
		for (const c of row) {
			if (c.width === 0) continue;
			const style = `${c.fg ? `color:${c.fg};` : ""}${c.bg ? `background:${c.bg};` : ""}${c.bold ? "font-weight:bold;" : ""}width:${c.width}ch`;
			html += `<span style="${style}">${esc(c.char === "" ? " " : c.char)}</span>`;
		}
		return `<div class="r">${html}</div>`;
	});
	return `<!doctype html><meta charset="utf-8"><title>${esc(title)}</title><style>
html,body{margin:0;background:#1e1e1e;color:#e5e5e5}
#t{display:inline-block;padding:8px;font:14px/1.25 "DejaVu Sans Mono","Noto Sans Mono",monospace;white-space:pre}
.r{height:1.25em}.r span{display:inline-block;height:1.25em;overflow:visible}
</style><div id="t">${rows.join("")}</div>`;
}

async function renderPng(cells, title, file) {
	if (typeof Bun.WebView !== "function") fail(2, "Bun.WebView is unavailable; pass --no-png");
	const scratch = mkdtempSync(join(tmpdir(), "tui-capture-png-"));
	const page = join(scratch, "screen.html");
	writeFileSync(page, cellsHtml(cells, title));
	const view = new Bun.WebView({ width: 1600, height: 1000 });
	try {
		await view.navigate(`file://${page}`);
		const [w, h] = await view.evaluate("(() => { const r = document.getElementById('t').getBoundingClientRect(); return [Math.ceil(r.width), Math.ceil(r.height)]; })()");
		await view.resize(w, h);
		await Bun.write(file, await view.screenshot({ format: "png" }));
		return { width: w, height: h };
	} finally {
		view.close();
		rmSync(scratch, { recursive: true, force: true });
	}
}

// Disposable HOME + agent dir with an `offline` provider backed by a local SSE stub.
function startOfflineModel() {
	const server = Bun.serve({
		hostname: "127.0.0.1", port: 0,
		fetch: () => new Response(`data: ${JSON.stringify({ id: "c", object: "chat.completion.chunk", choices: [{ index: 0, delta: { content: "offline reply" }, finish_reason: "stop" }] })}\n\ndata: [DONE]\n\n`, { headers: { "content-type": "text/event-stream" } }),
	});
	const home = mkdtempSync(join(tmpdir(), "tui-capture-home-"));
	const agent = join(home, ".omo/agent");
	const cwd = join(home, "work");
	for (const dir of [agent, cwd, join(agent, "omo-senpi/omo-native"), join(cwd, ".maho")]) mkdirSync(dir, { recursive: true });
	writeFileSync(join(agent, "models.json"), JSON.stringify({ providers: { offline: { api: "openai-completions", baseUrl: `http://127.0.0.1:${server.port}/v1`, apiKey: "offline-fixture", models: [{ id: "offline-model", reasoning: false, input: ["text"], contextWindow: 128000, maxTokens: 4096 }] } } }));
	writeFileSync(join(agent, "settings.json"), JSON.stringify({ defaultProvider: "offline", defaultModel: "offline-model", quietStartup: true, tipsEnabled: false, theme: "dark" }));
	writeFileSync(join(agent, "omo-senpi/omo-native/onboarding-completed"), JSON.stringify({ completedAt: new Date().toISOString(), version: 1 }));
	writeFileSync(join(cwd, ".maho/onboarding-completed"), "");
	return {
		server, home, cwd,
		env: { HOME: home, MAHO_CODING_AGENT_DIR: agent, XDG_CONFIG_HOME: join(home, ".config"), XDG_CACHE_HOME: join(home, ".cache"), XDG_DATA_HOME: join(home, ".local/share"), PI_OFFLINE: "1", DO_NOT_TRACK: "1" },
	};
}

function sha256Of(command) {
	const first = command.trim().split(/\s+/)[0];
	const path = first && (first.includes("/") ? resolve(first) : Bun.which(first));
	if (!path || !existsSync(path)) return null;
	return { path, sha256: createHash("sha256").update(readFileSync(path)).digest("hex") };
}

async function main() {
	if (typeof Bun === "undefined") fail(2, "run with bun (needs Bun.Terminal and Bun.WebView)");
	const opts = parseArgs(process.argv.slice(2));
	const Terminal = loadHeadless();
	mkdirSync(opts.evidenceDir, { recursive: true });
	const vt = new Terminal({ cols: opts.cols, rows: opts.rows, allowProposedApi: true, scrollback: 1000 });
	const meta = { title: opts.title, command: opts.command, fromFile: opts.fromFile, cols: opts.cols, rows: opts.rows, binary: null, offlineModel: opts.offlineModel, waits: [], teardown: null, startedAt: new Date().toISOString() };
	let raw = "";
	let status = 0;
	let shot = null; // { text, cells, raw } taken before teardown so exit output is not captured
	const snapshot = () => ({ text: viewportText(vt, opts.rows), cells: viewportCells(vt, opts.cols, opts.rows), raw });

	if (opts.fromFile) {
		raw = readFileSync(opts.fromFile, "utf8");
		await new Promise((done) => vt.write(raw, done));
	} else {
		meta.binary = sha256Of(opts.command);
		const offline = opts.offlineModel ? startOfflineModel() : null;
		const waiters = new Set();
		let ended = false;
		const notify = () => { for (const w of [...waiters]) w(); };
		const decoder = new TextDecoder();
		const waitFor = (label, pattern) => {
			const re = new RegExp(pattern, "u");
			const started = performance.now();
			return new Promise((res, rej) => {
				const timer = setTimeout(() => { waiters.delete(check); rej(new Error(`timeout after ${opts.timeout}ms waiting for ${label} /${pattern}/`)); }, opts.timeout);
				const check = () => {
					if (re.test(viewportText(vt, opts.rows))) {
						waiters.delete(check); clearTimeout(timer);
						meta.waits.push({ label, pattern, ms: Math.round(performance.now() - started) });
						res();
					} else if (ended) {
						waiters.delete(check); clearTimeout(timer);
						rej(new Error(`process output ended before ${label} /${pattern}/ appeared`));
					}
				};
				waiters.add(check);
				check();
			});
		};
		const env = { ...process.env, TERM: "xterm-256color", COLORTERM: "truecolor", LANG: process.env.LANG || "C.UTF-8", ...(offline?.env ?? {}) };
		// Simple commands are exec'd so signals reach the TUI itself, not a wrapper shell.
		const script = /[;&|<>()`$\n]/.test(opts.command) ? opts.command : `exec ${opts.command}`;
		const proc = Bun.spawn(["sh", "-c", script], {
			cwd: offline?.cwd ?? process.cwd(), env,
			terminal: {
				cols: opts.cols, rows: opts.rows,
				data(_t, bytes) { const s = decoder.decode(bytes, { stream: true }); raw += s; vt.write(s, notify); },
				exit() { ended = true; vt.write("", notify); },
			},
		});
		vt.onData((d) => { if (!ended) proc.terminal.write(d); }); // answer terminal queries (cursor position etc.)
		try {
			await waitFor("startup", opts.waitFor);
			for (const item of opts.input) {
				if (KEYS[item]) proc.terminal.write(KEYS[item]);
				else {
					proc.terminal.write(item);
					await waitFor(`echo of input ${JSON.stringify(item)}`, item.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
				}
			}
			if (opts.thenWait) await waitFor("after input", opts.thenWait);
		} catch (error) {
			console.error(`tui-capture: ${error.message}`);
			status = 1;
		}
		shot = snapshot();
		// Teardown: SIGTERM, bounded wait, SIGKILL fallback.
		const teardown = { signal: null, exitCode: null, errors: [] };
		if (proc.exitCode === null) {
			teardown.signal = "SIGTERM";
			proc.kill("SIGTERM");
			const killed = await Promise.race([proc.exited.then(() => true), new Promise((r) => setTimeout(() => r(false), 5000))]);
			if (!killed) { teardown.signal = "SIGKILL"; proc.kill("SIGKILL"); await proc.exited; }
		}
		teardown.exitCode = await proc.exited;
		try { proc.terminal?.close(); } catch (e) { teardown.errors.push(`terminal: ${e}`); }
		if (offline) {
			await offline.server.stop(true);
			rmSync(offline.home, { recursive: true, force: true });
			teardown.scratchRemoved = !existsSync(offline.home);
		}
		meta.teardown = teardown;
	}

	shot ??= snapshot();
	const cells = shot.cells;
	writeFileSync(join(opts.evidenceDir, "terminal.txt"), shot.text + "\n");
	writeFileSync(join(opts.evidenceDir, "terminal.ansi"), shot.raw);
	writeFileSync(join(opts.evidenceDir, "cells.json"), JSON.stringify({ cols: opts.cols, rows: opts.rows, cells }));
	if (opts.png) meta.png = await renderPng(cells, opts.title, join(opts.evidenceDir, "terminal.png"));
	meta.finishedAt = new Date().toISOString();
	meta.status = status === 0 ? "pass" : "fail";
	writeFileSync(join(opts.evidenceDir, "metadata.json"), JSON.stringify(meta, null, 2));
	vt.dispose();
	console.log(JSON.stringify({ evidenceDir: opts.evidenceDir, status: meta.status }));
	process.exit(status);
}

await main();
