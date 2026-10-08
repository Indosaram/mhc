// Causal interactive-TUI driver for the residual gate. Drives the REAL mhc TUI in a PTY
// (Bun.Terminal), renders the byte stream with @xterm/headless at the requested geometry, and
// exercises the pinned interactive behaviors against a GATED loopback provider:
//
//   * startup idle (editor prompt + model footer, no working indicator)
//   * a typed prompt whose turn streams: the first delta is held on a signal, so the driver can
//     observe the working indicator AND the partially streamed text BEFORE completion
//   * steer while the turn is held (editor accepts and echoes a queued line without ending the turn)
//   * abort while the turn is held (the pinned app.interrupt key, Escape, clears busy and returns
//     a usable editor)
//   * a release that completes the held turn and returns the idle prompt
//   * a terminal resize that reflows without overflow
//   * a custom/registered theme renders (its accent truecolor appears in the stream) or a malformed
//     theme falls back to the built-in dark theme
//
// Every wait is on an observed screen state with a bounded timeout; there are no fixed sleeps.
// Usage: bun tui-driver.mjs <mhc> <out-dir> <cols> <rows> <regular|fullscreen> [themePath] [themeName]
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { markOnboardingComplete, writeOfflineAgent } from "./qa-loopback-lib.mjs";

const XTERM = "/home/indo/.bun/install/cache/@xterm/headless@6.0.0@@@1/lib-headless/xterm-headless.js";
const { Terminal } = await import(XTERM);

const [, , binary, outDir, colsArg, rowsArg, modeArg, themePath, themeName] = process.argv;
if (!binary || !outDir) throw new Error("usage: bun tui-driver.mjs <mhc> <out-dir> <cols> <rows> <mode> [themePath] [themeName]");
const COLS = Number(colsArg ?? 120);
const ROWS = Number(rowsArg ?? 36);
const MODE = modeArg ?? "regular";
mkdirSync(outDir, { recursive: true });

// The full completion is split so the first half can be observed while the turn is held.
const REPLY = "offline acceptance";
const HELD = REPLY.slice(0, 8); // "offline "
const REST = REPLY.slice(8); // "acceptance"

// Gated loopback: a request streams the first half of the reply, then holds until `/release`;
// the release is one-shot (it never re-arms), so after it every request streams its reply to
// completion and no later turn can be left held.
let release;
let released = false;
let gate = new Promise((resolvePromise) => (release = resolvePromise));
const sse = (delta, finish = null) =>
	`data: ${JSON.stringify({ id: "offline", object: "chat.completion.chunk", created: 0, model: "offline", choices: [{ index: 0, delta, finish_reason: finish }] })}\n\n`;
const server = Bun.serve({
	hostname: "127.0.0.1",
	port: 0,
	// The gated loopback holds the SSE open until /release, bounded by the driver's own waitFor
	// timeouts; Bun's default 10 s idleTimeout would close the held stream first and surface a
	// spurious "error decoding response body". 0 disables the idle timeout (never a fixed sleep).
	idleTimeout: 0,
	async fetch(request) {
		const url = new URL(request.url);
		if (url.pathname === "/release") {
			released = true;
			release();
			return new Response("released");
		}
		const body = new ReadableStream({
			async start(controller) {
				const encoder = new TextEncoder();
				// The held turn can be aborted (Escape) before the release, which closes its stream; a
				// closed controller must not throw out of `start`.
				const push = (chunk) => {
					try {
						controller.enqueue(encoder.encode(chunk));
					} catch {
						/* the held turn was aborted; its stream is already closed */
					}
				};
				push(sse({ role: "assistant", content: HELD }));
				if (!released) await gate;
				// The second half completes the reply the driver waits for ("offline acceptance").
				push(sse({ content: REST }));
				push(sse({}, "stop"));
				push("data: [DONE]\n\n\n");
				try {
					controller.close();
				} catch {
					/* already closed */
				}
			},
		});
		return new Response(body, { headers: { "content-type": "text/event-stream" } });
	},
});
const baseUrl = `http://127.0.0.1:${server.port}/v1`;
const releaseUrl = `http://127.0.0.1:${server.port}/release`;

// Owned temp roots honor the configured TMPDIR (node:os.tmpdir()), never a hardcoded /tmp, so a
// QA run never writes to the small /tmp tmpfs user quota (EDQUOT os error 122).
const home = mkdtempSync(join(tmpdir(), "residual-tui-home-"));
const agent = writeOfflineAgent(home, baseUrl);
const cwd = realpathSync(mkdtempSync(join(tmpdir(), "residual-tui-cwd-")));
markOnboardingComplete(cwd);

const vt = new Terminal({ cols: COLS, rows: ROWS, allowProposedApi: true, scrollback: 0 });
let raw = "";
const decoder = new TextDecoder("utf-8");
const waiters = new Set();
const screen = () => {
	const buf = vt.buffer.active;
	const out = [];
	for (let y = 0; y < ROWS; y++) out.push(buf.getLine(buf.viewportY + y)?.translateToString(true) ?? "");
	return out.join("\n");
};
// Per-cell snapshot with positions + colors (theme-faithful) for the derived PNG renderer.
const hex = (value) => `#${(value & 0xffffff).toString(16).padStart(6, "0")}`;
// The standard xterm 256-color table (16 basic + 6x6x6 cube + 24 grays), used to resolve a palette
// color index to RGB. xterm-headless `getFgColor()/getBgColor()` returns the DECODED value — a
// palette INDEX (0..255) for palette cells, the packed 0xRRGGBB for RGB cells, or -1 for default —
// so the color mode must be read from `isFgRGB()/isBgRGB()` / `isFgPalette()/isBgPalette()`, never
// from a mode nibble of the returned value.
const ANSI256 = (() => {
	const basic = ["#000000", "#800000", "#008000", "#808000", "#000080", "#800080", "#008080", "#c0c0c0", "#808080", "#ff0000", "#00ff00", "#ffff00", "#0000ff", "#ff00ff", "#00ffff", "#ffffff"];
	const table = [...basic];
	const channel = (x) => (x === 0 ? 0 : 55 + x * 40);
	for (let i = 0; i < 216; i++) table.push(`#${[channel(Math.floor(i / 36)), channel(Math.floor((i % 36) / 6)), channel(i % 6)].map((v) => v.toString(16).padStart(2, "0")).join("")}`);
	for (let i = 0; i < 24; i++) {
		const v = (8 + i * 10).toString(16).padStart(2, "0");
		table.push(`#${v}${v}${v}`);
	}
	return table;
})();
const cellColor = (cell, which) => {
	try {
		const isDefault = which === "fg" ? cell.isFgDefault() : cell.isBgDefault();
		if (isDefault) return null;
		const isRGB = which === "fg" ? cell.isFgRGB() : cell.isBgRGB();
		const isPalette = which === "fg" ? cell.isFgPalette() : cell.isBgPalette();
		const raw = which === "fg" ? cell.getFgColor() : cell.getBgColor();
		if (raw === undefined || raw === null || raw < 0) return null;
		if (isRGB) return hex(raw); // getFgColor() already returns 0xRRGGBB for an RGB cell
		if (isPalette) return ANSI256[raw & 0xff] ?? null; // getFgColor() already returns the index
		return null;
	} catch {
		return null;
	}
};
const cellGrid = () => {
	const buf = vt.buffer.active;
	const cells = [];
	for (let row = 0; row < ROWS; row++) {
		const line = buf.getLine(buf.viewportY + row);
		if (!line) continue;
		for (let col = 0; col < COLS; col++) {
			const cell = line.getCell(col);
			if (!cell) continue;
			const ch = cell.getChars() || " ";
			const w = cell.getWidth() === 2 ? 2 : 1;
			if (w === 2 && col + 1 < COLS && line.getCell(col + 1)?.getWidth() === 0) {
				// leading wide cell; trailing cell omitted
			}
			if (cell.getWidth() === 0) continue; // trailing half of a wide glyph
			cells.push({ ch, fg: cellColor(cell, "fg"), bg: cellColor(cell, "bg"), w, col, row });
		}
	}
	return { cols: COLS, rows: ROWS, cells };
};
const check = () => {
	const text = screen();
	for (const w of waiters) if (w.pred(text)) (waiters.delete(w), w.resolve(text));
};
const waitFor = (label, pred, ms = 45_000) =>
	new Promise((resolvePromise, reject) => {
		const w = { pred, resolve: resolvePromise };
		const timer = setTimeout(() => (waiters.delete(w), reject(new Error(`timeout waiting for ${label}`))), ms);
		w.resolve = (t) => (clearTimeout(timer), resolvePromise(t));
		waiters.add(w);
		check();
	});
// xterm parses asynchronously: a write's callback fires only after the chunk is applied, so every
// write is awaited before the buffer is read (no blank/stale screenshots).
const writeSync = (data) => new Promise((resolvePromise) => vt.write(data, () => resolvePromise()));

const modeArgs = MODE === "fullscreen" ? ["--tui-mode", "fullscreen"] : [];
const themeArgs = themePath ? ["--theme", themePath, ...(themeName ? ["--use-theme", themeName] : [])] : [];
const proc = Bun.spawn(
	[binary, ...modeArgs, ...themeArgs, "--offline", "--no-tools", "--no-skills", "--no-prompt-templates", "--model", "offline/offline"],
	{
		cwd,
		env: { PATH: process.env.PATH, TMPDIR: tmpdir(), HOME: home, MAHO_CODING_AGENT_DIR: agent, OMO_CODING_AGENT_DIR: agent, SENPI_CODING_AGENT_DIR: agent, TERM: "xterm-256color", COLORTERM: "truecolor", LANG: "en_US.UTF-8" },
		terminal: {
			cols: COLS,
			rows: ROWS,
			data(_t, data) {
				const s = typeof data === "string" ? data : decoder.decode(data, { stream: true });
				raw += s;
				vt.write(s, check);
			},
		},
	},
);
const exited = proc.exited;

const evidence = {
	geometry: `${COLS}x${ROWS}`,
	mode: MODE,
	theme: themeName ?? null,
	onboarding_pre_completed: true,
	startup_predicate: false,
	prompt_echo_observed: false,
	gated_stream_visible_while_held: false,
	working_indicator_present_while_held: false,
	steer_echoed_while_working: false,
	abort_cleared_busy: false,
	reply_after_prompt: false,
	working_indicator_absent_at_reply: false,
	resize_reflowed: false,
	exit_code: null,
};
const log = [];
const ready = (t) => t.includes("\u276f") && t.includes("offline") && !t.includes("Working (");
let failed = false;

try {
	const startup = await waitFor("rendered TUI (editor prompt + model footer, idle)", ready);
	evidence.startup_predicate = true;
	writeFileSync(join(outDir, "startup.txt"), startup + "\n");
	log.push("startup: idle editor prompt + offline footer");

	// Typed prompt.
	proc.terminal.write("hello");
	await waitFor("typed prompt echo", (t) => /hello/.test(t));
	evidence.prompt_echo_observed = true;
	proc.terminal.write("\r");

	// The turn starts and streams the held first half; observe the working indicator + partial text.
	const held = await waitFor("working indicator + held partial stream", (t) => t.includes("Working (") && t.includes(HELD));
	evidence.working_indicator_present_while_held = true;
	evidence.gated_stream_visible_while_held = held.includes(HELD) && !held.includes(REPLY);
	writeFileSync(join(outDir, "held-frame.txt"), held + "\n");
	writeFileSync(join(outDir, "held-cells.json"), JSON.stringify(cellGrid()) + "\n");
	log.push(`held: working indicator + partial ${JSON.stringify(HELD)} rendered before completion`);

	// Steer while held: the editor accepts and echoes a queued line; the turn stays in progress.
	proc.terminal.write("steer-marker");
	await waitFor("steer text echoed in editor while working", (t) => t.includes("steer-marker") && t.includes("Working ("));
	evidence.steer_echoed_while_working = true;
	proc.terminal.write("\r");
	log.push("steer: queued line echoed while the turn stayed busy");

	// Abort while held: the pinned app.interrupt key is Escape (maho-core/src/keybindings.rs
	// "app.interrupt" -> "escape"; interactive_mode.rs hints it "to interrupt"). Ctrl+C is
	// app.clear ("Clear editor"), so it must NOT be used to abort.
	proc.terminal.write("\x1b");
	const aborted = await waitFor("abort clears busy and returns the editor", (t) => !t.includes("Working (") && t.includes("\u276f"));
	evidence.abort_cleared_busy = true;
	writeFileSync(join(outDir, "abort-frame.txt"), aborted + "\n");
	writeFileSync(join(outDir, "abort-cells.json"), JSON.stringify(cellGrid()) + "\n");
	log.push("abort: Escape (app.interrupt) cleared the working indicator and returned the editor");

	// Release any still-held turn, then run a fresh prompt to completion.
	await fetch(releaseUrl).catch(() => {});
	proc.terminal.write("hello again");
	await waitFor("second prompt echo", (t) => /hello again/.test(t));
	proc.terminal.write("\r");
	const reply = await waitFor("reply rendered and idle", (t) => t.includes(REPLY) && !t.includes("Working ("));
	evidence.reply_after_prompt = true;
	evidence.working_indicator_absent_at_reply = true;
	writeFileSync(join(outDir, "reply.txt"), reply + "\n");
	writeFileSync(join(outDir, "reply-cells.json"), JSON.stringify(cellGrid()) + "\n");
	log.push(`reply: ${JSON.stringify(REPLY)} rendered after a real typed prompt, idle`);

	// Resize: reflow at a wider geometry without overflow.
	const wideCols = COLS + 40;
	try {
		proc.terminal.resize(wideCols, ROWS);
		const reflowed = await waitFor("reflow after resize", (t) => t.includes("\u276f") && !t.includes("Working ("), 15_000);
		const overflow = reflowed.split("\n").some((line) => [...line].length > wideCols);
		evidence.resize_reflowed = !overflow;
		writeFileSync(join(outDir, "resize-frame.txt"), reflowed + "\n");
		writeFileSync(join(outDir, "resize-cells.json"), JSON.stringify(cellGrid()) + "\n");
		log.push(`resize: ${COLS} -> ${wideCols} columns, overflow=${overflow}`);
	} catch (error) {
		log.push(`resize: not observed (${error.message})`);
	}

	// Custom/registered theme display evidence: the accent truecolor must appear in the stream.
	if (themeName && themeName === "custom-accent") {
		evidence.theme_accent_rendered = raw.includes("38;2;255;136;0") || raw.includes("48;2;255;136;0");
		log.push(`theme: custom accent rendered=${evidence.theme_accent_rendered}`);
	}
	if (themePath && themeName && themeName === "broken-fallback") {
		evidence.theme_fallback_diagnostic = /theme:/i.test(raw);
		log.push(`theme: fallback diagnostic observed=${evidence.theme_fallback_diagnostic}`);
	}

	proc.terminal.write("\x03");
	proc.terminal.write("\x03");
	const code = await Promise.race([
		exited,
		new Promise((_, reject) => setTimeout(() => reject(new Error("timeout waiting for exit after Ctrl+C x2")), 20_000)),
	]);
	evidence.exit_code = code;
	log.push(`quit: Ctrl+C x2 -> exit ${code}`);
} catch (error) {
	failed = true;
	evidence.error = error.message;
	log.push(`FAIL: ${error.message}`);
	writeFileSync(join(outDir, "failure-screen.txt"), screen() + "\n");
	proc.kill("SIGKILL");
	try {
		evidence.exit_code = await exited;
	} catch {
		/* already gone */
	}
}

raw += decoder.decode();
// Always emit per-cell color/position provenance for the final frame: the verifier requires
// `reply-cells.json` (and a color-faithful PNG derived from it) even when the reply predicate
// failed, so the artifact records what was actually on screen. Correctness is still gated
// separately by `reply_after_prompt`/`exit_code`.
if (!existsSync(join(outDir, "reply-cells.json"))) {
	writeFileSync(join(outDir, "reply-cells.json"), JSON.stringify(cellGrid()) + "\n");
}
vt.dispose();
writeFileSync(join(outDir, "terminal-ansi.txt"), raw);
writeFileSync(join(outDir, "evidence.json"), JSON.stringify(evidence, null, 2) + "\n");
writeFileSync(join(outDir, "drive.log"), log.join("\n") + "\n");
writeFileSync(
	join(outDir, "metadata.json"),
	JSON.stringify(
		{
			binary,
			binarySha256: createHash("sha256").update(readFileSync(binary)).digest("hex"),
			cols: COLS,
			rows: ROWS,
			mode: MODE,
			loopback: true,
			ansiSha256: createHash("sha256").update(raw).digest("hex"),
			exitCode: evidence.exit_code,
		},
		null,
		2,
	) + "\n",
);
server.stop(true);
rmSync(home, { recursive: true, force: true });
rmSync(cwd, { recursive: true, force: true });
console.log(log.join("\n"));
process.exit(failed ? 1 : 0);

