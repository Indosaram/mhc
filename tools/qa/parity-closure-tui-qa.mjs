// TUI QA for the 2026-10-14 parity closure: drives the real `mhc` TUI in a PTY (Bun.Terminal),
// parses with @xterm/headless, writes evidence.json + raw ANSI for xterm.js screenshots.
// usage: bun tools/qa/parity-closure-tui-qa.mjs <mhc> <out-dir>
import { mkdirSync, mkdtempSync, writeFileSync, realpathSync, rmSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const XTERM = "/home/indo/.bun/install/cache/@xterm/headless@6.0.0@@@1/lib-headless/xterm-headless.js";
const { Terminal } = await import(XTERM);
const [, , binary, outDir] = process.argv;
mkdirSync(outDir, { recursive: true });
const COLS = 100, ROWS = 30;
let requests = 0; const bodies = [];
const server = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: async (req) => { requests++; bodies.push(await req.text()); return new Response("data: [DONE]\n\n", { headers: { "content-type": "text/event-stream" } }); } });
const home = mkdtempSync(join(tmpdir(), "pc-tui-home-"));
const agent = join(home, "agent");
mkdirSync(agent, { recursive: true });
writeFileSync(join(agent, "models.json"), JSON.stringify({ providers: { offline: { api: "openai-completions", baseUrl: `http://127.0.0.1:${server.port}/v1`, apiKey: "offline-fixture", models: [
	{ id: "offline-sonnet", reasoning: true, input: ["text"], contextWindow: 128000, maxTokens: 4096 },
	{ id: "offline-haiku", reasoning: false, input: ["text"], contextWindow: 128000, maxTokens: 4096 },
] } } }));
const cwd = realpathSync(mkdtempSync(join(tmpdir(), "pc-tui-cwd-")));
mkdirSync(join(cwd, ".maho"), { recursive: true });
writeFileSync(join(cwd, ".maho", "onboarding-completed"), "");
mkdirSync(join(agent, "omo-senpi", "omo-native"), { recursive: true });
writeFileSync(join(agent, "omo-senpi", "omo-native", "onboarding-completed"), JSON.stringify({ completedAt: new Date().toISOString(), version: 1 }));
const evidence = { scenarios: [] };

const vt = new Terminal({ cols: COLS, rows: ROWS, allowProposedApi: true, scrollback: 500 });
let raw = "";
const decoder = new TextDecoder();
const waiters = new Set();
const screen = () => { const b = vt.buffer.active; return Array.from({ length: b.length }, (_, y) => b.getLine(y)?.translateToString(true) ?? "").join("\n"); };
const check = () => { const t = screen(); for (const w of waiters) if (w.pred(t)) { waiters.delete(w); w.done(t); } };
const waitFor = (what, pred, ms = 30000) => new Promise((res, rej) => { const timer = setTimeout(() => { waiters.delete(w); rej(new Error(`timeout: ${what}\n${screen()}`)); }, ms); const w = { pred, done: (t) => { clearTimeout(timer); res(t); } }; waiters.add(w); check(); });
const proc = Bun.spawn([binary, "--offline", "--no-tools", "--no-skills", "--model", "offline/offline-haiku"], {
	cwd, env: { PATH: process.env.PATH, TMPDIR: tmpdir(), HOME: home, MAHO_CODING_AGENT_DIR: agent, TERM: "xterm-256color", COLORTERM: "truecolor", LANG: "en_US.UTF-8" },
	terminal: { cols: COLS, rows: ROWS, data(_t, d) { const s = typeof d === "string" ? d : decoder.decode(d, { stream: true }); raw += s; vt.write(s, check); } },
});
const type = async (text) => { for (const ch of text) { proc.terminal.write(ch); await Bun.sleep(0); } };
const snap = (name) => {
	writeFileSync(join(outDir, `${name}.ansi`), raw);
	writeFileSync(join(outDir, `${name}.txt`), screen());
	// Serialize the CURRENT rendered grid (with SGR colors) so a plain xterm.js page reproduces it exactly.
	const b = vt.buffer.active; const out = [];
	for (let y = b.viewportY; y < b.viewportY + ROWS; y++) {
		const line = b.getLine(y); let s = "", prev = "";
		for (let x = 0; x < COLS; x++) {
			const c = line?.getCell(x); if (!c) break; if (c.getWidth() === 0) continue;
			const fg = c.isFgRGB() ? `38;2;${(c.getFgColor()>>16)&255};${(c.getFgColor()>>8)&255};${c.getFgColor()&255}` : c.isFgPalette() ? `38;5;${c.getFgColor()}` : "39";
			const bg = c.isBgRGB() ? `48;2;${(c.getBgColor()>>16)&255};${(c.getBgColor()>>8)&255};${c.getBgColor()&255}` : c.isBgPalette() ? `48;5;${c.getBgColor()}` : "49";
			const sgr = `\x1b[0;${fg};${bg}${c.isBold() ? ";1" : ""}m`;
			if (sgr !== prev) { s += sgr; prev = sgr; }
			s += c.getChars() || " ";
		}
		out.push(s + "\x1b[0m");
	}
	writeFileSync(join(outDir, `${name}.grid.ansi`), out.join("\r\n"));
};
const record = (scenario, pass, detail = {}) => { evidence.scenarios.push({ scenario, pass, ...detail }); console.log(JSON.stringify({ scenario, pass, ...detail })); };

try {
	await waitFor("startup footer", (t) => /offline-haiku/.test(t));
	snap("startup");
	// modes-interactive-commands-03: /arminsayshi renders the easter egg locally and sends nothing.
	const before = requests;
	await type("/arminsayshi"); proc.terminal.write("\r");
	const shown = await waitFor("armin", (t) => /ARMIN|Armin|armin/.test(t), 15000).then(() => true, () => false);
	snap("arminsayshi");
	record("arminsayshi-local", shown && requests === before && requests === 0, { requests: requests - before });
	// modes-interactive-commands-05: `/model sonnet` resolves a bare id term without provider prefix.
	await type("/model offline-sonnet");
	await waitFor("model completion", (t) => /→ offline-sonnet/.test(t), 10000).catch(() => {});
	proc.terminal.write("\x1b"); await waitFor("completion closed", (t) => !/→ offline-sonnet/.test(t), 5000).catch(() => {});
	proc.terminal.write("\r");
	const footer = (t) => t.trimEnd().split("\n").at(-1) ?? "";
	const switched = await waitFor("footer shows offline-sonnet", (t) => /offline-sonnet(:\w+)?\s*$/.test(footer(t)), 20000).then(() => true, () => false);
	snap("model-bare-id");
	record("model-bare-id", switched && !/requires provider/i.test(screen()), {});
} catch (error) {
	record("driver-error", false, { error: String(error).slice(0, 800) });
	snap("error");
} finally {
	proc.kill("SIGTERM");
	await Promise.race([proc.exited, Bun.sleep(5000)]);
	if (proc.exitCode === null) proc.kill("SIGKILL");
	server.stop(true);
	writeFileSync(join(outDir, "evidence.json"), JSON.stringify(evidence, null, 2)); writeFileSync(join(outDir, "requests.json"), JSON.stringify(bodies.map((b) => { try { const j = JSON.parse(b); return j.messages?.slice(-2); } catch { return b.slice(0, 500); } }), null, 1));
	rmSync(home, { recursive: true, force: true }); rmSync(cwd, { recursive: true, force: true });
	console.log(JSON.stringify({ cleanup: [home, cwd], removed: !existsSync(home) && !existsSync(cwd), exit: proc.exitCode ?? proc.signalCode }));
}
