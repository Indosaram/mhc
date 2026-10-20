#!/usr/bin/env bun
import { createRequire } from "node:module";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { detectFerryx } from "./ferryx-check.mjs";

const repo = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const payload = "ulw drift-probe-7a91";
const timeout = 30000;

// Follow the native QA harness's existing Bun-cache resolution, without pinning
// another user's home. Prefer normal resolution, then the pinned golden checkout.
function loadTerminal() {
  const errors = [];
  for (const base of [import.meta.url, join(process.env.SENPI_SRC ?? join(homedir(), "code/senpi"), "package.json")]) {
    try { return createRequire(base)("@xterm/headless").Terminal; }
    catch (error) { errors.push(String(error)); }
  }
  const cached = join(process.env.BUN_INSTALL_CACHE_DIR ?? join(homedir(), ".bun/install/cache"), "@xterm/headless@6.0.0@@@1/lib-headless/xterm-headless.js");
  try { return createRequire(import.meta.url)(cached).Terminal; }
  catch (error) { errors.push(String(error)); }
  throw new Error(`Cannot resolve existing @xterm/headless; no dependencies installed. ${errors.join("; ")}`);
}

function options() {
  const opts = { out: join(repo, ".omo/evidence/ulw-input-drift"), binary: null, requireFerryx: false, checkFerryx: false };
  const args = process.argv.slice(2);
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (arg === "--help" || arg === "-h") return null;
    if (arg === "--require-ferryx") opts.requireFerryx = true;
    else if (arg === "--check-ferryx") opts.checkFerryx = true;
    else if (arg === "--binary" || arg === "--out-dir") {
      const value = args[++i];
      if (!value || value.startsWith("--")) throw new Error(`Missing value for ${arg}`);
      opts[arg === "--binary" ? "binary" : "out"] = resolve(value);
    } else throw new Error(`Unknown argument: ${arg}`);
  }
  if (!opts.checkFerryx && !opts.requireFerryx && !opts.binary) throw new Error("--binary <CI-built-mhc> is required");
  return opts;
}

// Timeouts are failure deadlines only, never synchronization delays.
function deadline(promise, label, ms = timeout) {
  let timer;
  return Promise.race([promise, new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`Timeout: ${label}`)), ms);
  })]).finally(() => clearTimeout(timer));
}

async function run(opts, result) {
  const Terminal = loadTerminal();
  const binary = resolve(opts.binary);
  result.binary = { path: binary, sha256: createHash("sha256").update(readFileSync(binary)).digest("hex") };
  const scratch = mkdtempSync(join(tmpdir(), "ulw-input-drift-"));
  const home = join(scratch, "home"), cwd = join(scratch, "cwd"), agent = join(home, ".omo/agent");
  let proc, server, vt, raw = "", pending = "", revision = 0, fatal = null;
  let stream, requestBody, closing = false, guard = null;
  const waiters = new Set();
  const request = Promise.withResolvers();
  const changed = () => {
    for (const waiter of [...waiters]) waiter();
  };
  const die = (error) => { fatal ??= error; changed(); };
  let cols = 100, rows = 30;
  function snapshot() {
    const b = vt.buffer.active;
    const lines = Array.from({ length: b.length }, (_, y) => b.getLine(y)?.translateToString(true) ?? "");
    // User message history does not use CustomEditor's leading U+276F marker.
    const prompts = lines.flatMap((line, y) => /^\s*❯(?: |$)/u.test(line) ? [y] : []);
    const visible = lines.slice(b.viewportY, b.viewportY + rows);
    const borders = visible.flatMap((line, y) => {
      const count = [...line].filter(c => c === "─" || c === "━").length;
      return count >= cols - 18 ? [y] : [];
    });
    const prompt = prompts[0] - b.viewportY;
    const top = borders.filter(y => y < prompt).at(-1);
    const bottom = borders.find(y => y > prompt);
    const colors = top === undefined ? [] : Array.from({ length: cols }, (_, x) => {
      const c = b.getLine(b.viewportY + top)?.getCell(x);
      return [c?.getChars(), c?.getFgColor(), c?.getFgColorMode(), c?.getBgColor()];
    });
    return { revision, cols, rows, prompts, prompt, top, bottom,
      anchor: [rows - prompt, prompt - top, bottom - prompt],
      signature: JSON.stringify(colors), text: visible.join("\n"), buffer: lines.join("\n") };
  }
  function single(s) {
    if (s.prompts.length !== 1 || s.prompt < 0 || s.prompt >= rows || s.top === undefined || s.bottom === undefined) {
      throw new Error(`Expected one editor in viewport AND scrollback: ${JSON.stringify({ prompts: s.prompts, top: s.top, bottom: s.bottom })}`);
    }
  }
  function stable(s, anchor) {
    single(s);
    if (JSON.stringify(s.anchor) !== JSON.stringify(anchor)) throw new Error(`Editor drift: ${s.anchor} != ${anchor}`);
  }
  function wait(label, predicate) {
    let listener;
    const promise = new Promise((resolveWait, reject) => {
      listener = () => {
        try {
          if (fatal) throw fatal;
          const s = snapshot();
          if (predicate(s)) { waiters.delete(listener); resolveWait(s); }
        } catch (error) { waiters.delete(listener); reject(error); }
      };
      listener.cancel = () => reject(new Error(`Cancelled: ${label}`));
      waiters.add(listener);
      listener();
    });
    return deadline(promise, label).finally(() => waiters.delete(listener));
  }
  function capture(name, s) {
    writeFileSync(join(opts.out, `${name}.txt`), s.text);
    writeFileSync(join(opts.out, `${name}.buffer.txt`), s.buffer);
    result.scenarios.push({ name, status: "pass", revision: s.revision, cols, rows, anchor: s.anchor, prompts: s.prompts, artifact: `${name}.txt` });
  }
  async function action(label, trigger, predicate) {
    const before = revision;
    const ready = wait(label, s => s.revision > before && predicate(s));
    try { trigger(); } catch (error) { die(error); }
    return ready;
  }
  async function frames(label, anchor) {
    guard = s => stable(s, anchor);
    try {
      for (let i = 0; i < 5; i++) {
        const before = snapshot();
        const next = await wait(`${label} distinct color frame ${i}`, s => s.revision > before.revision && s.signature !== before.signature);
        stable(next, anchor);
        capture(`${label}-${i}`, next);
      }
    } finally { guard = null; }
  }
  function delta(content, finish = null) {
    stream.enqueue(new TextEncoder().encode(`data: ${JSON.stringify({ id: "drift-stream", object: "chat.completion.chunk", created: 1, model: "controlled-model", choices: [{ index: 0, delta: content ? { role: "assistant", content } : {}, finish_reason: finish }] })}\n\n`));
  }
  const interrupt = () => die(new Error("Interrupted"));
  process.on("SIGINT", interrupt); process.on("SIGTERM", interrupt);
  try {
    for (const dir of [agent, cwd, join(scratch, "tmp"), join(agent, "omo-senpi/omo-native")]) mkdirSync(dir, { recursive: true });
    writeFileSync(join(agent, "omo-senpi/omo-native/onboarding-completed"), JSON.stringify({ completedAt: new Date().toISOString(), version: 1 }));
    server = Bun.serve({ hostname: "127.0.0.1", port: 0, idleTimeout: 0, async fetch(req) {
      try {
        if (req.method !== "POST" || new URL(req.url).pathname !== "/v1/chat/completions") throw new Error(`Unexpected transport request: ${req.method} ${req.url}`);
        if (requestBody) throw new Error("Unexpected second model request");
        requestBody = await req.json();
        if (requestBody.model !== "controlled-model" || requestBody.stream !== true || !JSON.stringify(requestBody.messages).includes(payload)) throw new Error("Model request did not contain submitted ulw prompt and streaming model");
        const body = new ReadableStream({ start(controller) { stream = controller; } });
        request.resolve();
        return new Response(body, { headers: { "content-type": "text/event-stream", "cache-control": "no-cache" } });
      } catch (error) { die(error); request.reject(error); return new Response(String(error), { status: 500 }); }
    } });
    // Attach rejection handler before the child can send a request.
    request.promise.catch(() => {});
    writeFileSync(join(agent, "models.json"), JSON.stringify({ providers: { controlled: {
      api: "openai-completions", baseUrl: `http://127.0.0.1:${server.port}/v1`, apiKey: "local-fixture",
      models: [{ id: "controlled-model", reasoning: false, input: ["text"], contextWindow: 128000, maxTokens: 4096 }],
    } } }));
    writeFileSync(join(agent, "settings.json"), JSON.stringify({ defaultProvider: "controlled", defaultModel: "controlled-model", quietStartup: true, tipsEnabled: false, smoothStreaming: false, autoTitleSessions: false, theme: "dark" }));
    vt = new Terminal({ cols, rows, allowProposedApi: true, scrollback: 10000 });
    vt.onData(data => proc?.terminal?.write(data)); // reply to terminal capability/cursor queries
    const decoder = new TextDecoder();
    const startup = wait("startup editor and fixture model", s => s.text.includes("controlled-model") && s.prompts.length > 0);
    startup.catch(() => {}); // spawn itself can throw before the await below
    proc = Bun.spawn([binary, "--offline", "--tui-mode", "regular", "--no-tools", "--no-skills", "--no-prompt-templates", "--no-context-files", "--model", "controlled/controlled-model"], {
      cwd, env: { PATH: process.env.PATH, HOME: home, XDG_CONFIG_HOME: join(home, ".config"), XDG_CACHE_HOME: join(home, ".cache"), XDG_DATA_HOME: join(home, ".local/share"),
        TMPDIR: join(scratch, "tmp"), NODE_COMPILE_CACHE: join(scratch, "node-cache"), BUN_RUNTIME_TRANSPILER_CACHE_PATH: join(scratch, "bun-cache"),
        MAHO_CODING_AGENT_DIR: agent, SENPI_CODING_AGENT_DIR: agent, PI_CODING_AGENT_DIR: agent, TERM: "xterm-256color", COLORTERM: "truecolor", LANG: "C.UTF-8", DO_NOT_TRACK: "1", OMO_DISABLE_POSTHOG: "1" },
      terminal: { cols, rows, data(_terminal, bytes) {
        const text = decoder.decode(bytes, { stream: true }); raw += text; pending += text;
        // Observe completed native synchronized-update frames, not arbitrary PTY
        // chunks (a chunk may end after erase and before repaint).
        const end = "\x1b[?2026l";
        let at;
        while ((at = pending.indexOf(end)) !== -1) {
          const frame = pending.slice(0, at + end.length); pending = pending.slice(at + end.length);
          vt.write(frame, () => {
            revision++;
            try { if (guard) guard(snapshot()); } catch (error) { die(error); }
            changed();
          });
        }
      } },
    });
    proc.exited.then(code => { if (!closing) die(new Error(`TUI exited unexpectedly: ${code}`)); });
    let s = await startup; single(s); capture("startup", s);
    s = await action("typed ulw", () => proc.terminal.write(payload), s => s.text.includes(`❯ ${payload}`));
    single(s); capture("typed", s); await frames("animation", s.anchor);
    const anchor = s.anchor;
    s = await action("edit while animated", () => proc.terminal.write("x"), s => s.text.includes(`❯ ${payload}x`));
    stable(s, anchor);
    s = await action("backspace while animated", () => proc.terminal.write("\x7f"), s => s.text.includes(`❯ ${payload}`) && !s.text.includes(`${payload}x`));
    stable(s, anchor); capture("edited", s);
    proc.terminal.write("\r");
    await deadline(request.promise, "submitted ulw reaches controlled transport");
    s = await action("first streamed output", () => delta("DRIFT_STREAM_ONE"), s => s.text.includes("DRIFT_STREAM_ONE"));
    single(s); capture("stream", s);
    // Resize readiness requires a *new product frame*, painted at the new
    // width, AND the next independently gated provider token on that grid.
    for (const [width, height, marker] of [[80, 24, "DRIFT_RESIZED"], [100, 30, "DRIFT_RESTORED"]]) {
      s = await action(marker, () => {
        cols = width; rows = height; vt.resize(cols, rows); proc.terminal.resize(cols, rows);
        delta(`\n${marker}`);
      }, s => {
        if (!s.text.includes(marker) || s.top === undefined || s.bottom === undefined) return false;
        const lines = s.text.split("\n");
        return [s.top, s.bottom].every(y => [...lines[y]].length === width);
      });
      single(s); capture(marker, s); await frames(marker, s.anchor);
    }
    s = await action("final streamed output", () => {
      delta("\nDRIFT_COMPLETE"); delta("", "stop");
      stream.enqueue(new TextEncoder().encode("data: [DONE]\n\n")); stream.close();
    }, s => s.text.includes("DRIFT_COMPLETE"));
    single(s); capture("final-output", s);
    // An additional editor interaction proves the retained input is usable;
    // do not infer agent-settled merely from seeing the last output token.
    s = await action("post-output editor input", () => proc.terminal.write("post-output-probe"), s => s.text.includes("❯ post-output-probe"));
    single(s); capture("post-output-editor", s);
    if (fatal) throw fatal;
    result.pty.status = "pass";
  } catch (error) {
    result.pty = { status: "fail", error: String(error), stack: error.stack };
    if (vt) writeFileSync(join(opts.out, "failure-buffer.txt"), snapshot().buffer);
  } finally {
    closing = true;
    for (const waiter of waiters) waiter.cancel();
    waiters.clear();
    const cleanupErrors = [];
    try {
      if (proc) {
        proc.kill("SIGTERM");
        try { await deadline(proc.exited, "TUI SIGTERM exit", 5000); }
        catch { proc.kill("SIGKILL"); await deadline(proc.exited, "TUI SIGKILL exit", 5000); }
        proc.terminal?.close();
      }
    } catch (error) { cleanupErrors.push(`child: ${error}`); }
    try { if (server) await server.stop(true); } catch (error) { cleanupErrors.push(`server: ${error}`); }
    try { vt?.dispose(); } catch (error) { cleanupErrors.push(`terminal: ${error}`); }
    // Retain a sandbox if a child could still be using it.
    if (!proc || proc.exitCode !== null) {
      try { rmSync(scratch, { recursive: true, force: true }); } catch (error) { cleanupErrors.push(`sandbox: ${error}`); }
    } else cleanupErrors.push("sandbox retained: child exit not confirmed");
    result.cleanup = { scratch, removed: !existsSync(scratch), child_exit: proc?.exitCode ?? null, errors: cleanupErrors };
    if (cleanupErrors.length) {
      result.pty = { status: "fail", error: `Cleanup failed: ${cleanupErrors.join("; ")}`, previous: result.pty };
    }
    process.off("SIGINT", interrupt); process.off("SIGTERM", interrupt);
    writeFileSync(join(opts.out, "terminal.ansi"), raw);
    if (requestBody) writeFileSync(join(opts.out, "request.json"), JSON.stringify(requestBody, null, 2));
  }
}

async function main() {
  const opts = options();
  if (!opts) {
    console.log("bun tools/qa/ulw-input-drift/driver.mjs --binary /absolute/CI/mhc [--out-dir /evidence/run] [--require-ferryx]\nbun tools/qa/ulw-input-drift/driver.mjs --check-ferryx");
    return;
  }
  const result = { schema_version: 1, surface: "local-pty-xterm-headless", status: "fail", pty: { status: "not-run" }, ferryx: detectFerryx(), scenarios: [] };
  mkdirSync(opts.out, { recursive: true });
  if (opts.checkFerryx || opts.requireFerryx) result.status = "ferryx-unavailable";
  else {
    try { await run(opts, result); } catch (error) { result.pty = { status: "fail", error: String(error) }; }
    result.status = result.pty.status;
  }
  writeFileSync(join(opts.out, "evidence.json"), JSON.stringify(result, null, 2));
  console.log(JSON.stringify(result));
  process.exitCode = result.status === "pass" ? 0 : result.status === "ferryx-unavailable" ? 77 : 1;
}
main().catch(error => { console.log(JSON.stringify({ status: "fail", error: String(error) })); process.exitCode = 1; });
