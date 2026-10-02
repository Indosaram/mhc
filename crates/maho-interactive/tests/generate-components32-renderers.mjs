import { pinnedSenpiRoot } from "../../../tools/golden/pin.mjs";
import { pathToFileURL } from "node:url";
import { writeFileSync } from "node:fs";

const root = pinnedSenpiRoot();
const { default: chalk } = await import(pathToFileURL(root + "/node_modules/chalk/source/index.js").href);
chalk.level = 1;
const interactive = root + "/packages/coding-agent/src/modes/interactive/";
const tools = root + "/packages/coding-agent/src/core/tools/";
const load = (base, name) => import(pathToFileURL(base + name + ".ts").href);

const { initTheme, theme } = await load(interactive, "theme/theme");
const renderers = await load(tools, "renderers/index");
const { KeybindingsManager } = await import(pathToFileURL(root + "/packages/coding-agent/src/core/keybindings.ts").href);
const { setKeybindings } = await import(pathToFileURL(root + "/packages/tui/src/index.ts").href);
initTheme("dark", false);
// The interactive app installs the full keybinding registry at startup (cli/startup-ui.ts); the
// collapse hints read through it, so the fixtures must too.
setKeybindings(KeybindingsManager.create());

const cwd = "/tmp/project";

const render = (component) => {
  const lines = component.render(80);
  return lines.map((line) => line.replace(/\s+$/, ""));
};

const state = () => ({});
const context = (extra = {}) => ({
  args: {},
  toolCallId: "call-1",
  invalidate: () => {},
  lastComponent: undefined,
  state: state(),
  cwd,
  executionStarted: true,
  argsComplete: true,
  isPartial: false,
  expanded: false,
  showImages: false,
  imageProtocol: undefined,
  isError: false,
  hasResult: false,
  spinnerFrame: undefined,
  ...extra,
});

const all = renderers.createAllToolRenderers();
const cases = [];

const callCase = (name, args, extra = {}) => {
  const renderer = all[name];
  const component = renderer.renderCall(args, theme, context({ args, ...extra }));
  cases.push({ name, kind: "call", args, expanded: Boolean(extra.expanded), lines: render(component) });
};

const resultCase = (name, args, result, extra = {}) => {
  const renderer = all[name];
  const ctx = context({ args, hasResult: true, ...extra });
  renderer.renderCall(args, theme, ctx);
  const component = renderer.renderResult(result, { expanded: Boolean(extra.expanded), isPartial: false }, theme, ctx);
  cases.push({
    name,
    kind: "result",
    args,
    result,
    expanded: Boolean(extra.expanded),
    isError: Boolean(extra.isError),
    lines: render(component),
  });
};

callCase("read", { file_path: "src/main.rs" });
callCase("read", { file_path: "src/main.rs", offset: 3, limit: 5 });
callCase("ls", { path: "src", limit: 25 });
callCase("find", { pattern: "*.rs", path: "src", limit: 5 });
callCase("grep", { pattern: "needle", path: "src" });
callCase("bash", { command: "echo hi", timeout: 5 });
callCase("edit", { file_path: "src/main.rs" });
callCase("write", { file_path: "src/main.rs", content: "fn main() {}\n" });

resultCase("read", { file_path: "src/main.rs" }, { content: [{ type: "text", text: "fn main() {\n    let value = 42;\n}\n" }], details: undefined }, { expanded: true });
resultCase("ls", { path: "src" }, { content: [{ type: "text", text: "a.rs\nb.rs" }], details: { entryLimitReached: 3 } });
resultCase("find", { pattern: "*.rs", path: "src" }, { content: [{ type: "text", text: "src/a.rs" }], details: { resultLimitReached: 7 } });
resultCase("grep", { pattern: "needle", path: "src" }, { content: [{ type: "text", text: "no matches" }], details: undefined });
resultCase("bash", { command: "echo hi" }, { content: [{ type: "text", text: "hi\n" }], details: undefined });
resultCase("write", { file_path: "src/main.rs", content: "x" }, { content: [{ type: "text", text: "boom" }], details: undefined }, { isError: true });
// An image block with images suppressed: the fallback indicator is what the card shows.
resultCase("read", { file_path: "shot.png" }, { content: [{ type: "image", data: "aGVsbG8=", mimeType: "image/png" }], details: undefined }, { expanded: true });
resultCase("ls", { path: "pics" }, { content: [{ type: "text", text: "shot.png" }, { type: "image", data: "aGVsbG8=", mimeType: "image/png" }], details: undefined });

// Long outputs exercise the collapse hint, which carries the expand keybinding text.
const longLines = (count, prefix) => Array.from({ length: count }, (_, i) => `${prefix} ${i + 1}`).join("\n");
resultCase("read", { file_path: "src/big.rs" }, { content: [{ type: "text", text: longLines(20, "row") }], details: undefined }, { expanded: false });
resultCase("ls", { path: "many" }, { content: [{ type: "text", text: longLines(30, "file") }], details: undefined }, { expanded: false });
resultCase("find", { pattern: "*.rs", path: "src" }, { content: [{ type: "text", text: longLines(30, "src/file") }], details: undefined }, { expanded: false });
resultCase("grep", { pattern: "needle", path: "src" }, { content: [{ type: "text", text: longLines(30, "hit") }], details: undefined }, { expanded: false });
resultCase("write", { file_path: "src/big.rs", content: longLines(40, "line") }, { content: [{ type: "text", text: "wrote" }], details: undefined }, { expanded: false });
resultCase("bash", { command: "run" }, { content: [{ type: "text", text: longLines(12, "out") }], details: undefined }, { expanded: false });

writeFileSync(import.meta.dir + "/golden/components32-renderers.json", JSON.stringify(cases, null, 2) + "\n");
console.log(`Generated ${cases.length} renderer fixtures from pinned Senpi`);
