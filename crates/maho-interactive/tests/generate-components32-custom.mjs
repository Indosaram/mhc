import { pinnedSenpiRoot } from "../../../tools/golden/pin.mjs";
import { pathToFileURL } from "node:url";
import { writeFileSync } from "node:fs";

const root = pinnedSenpiRoot();
const { default: chalk } = await import(pathToFileURL(root + "/node_modules/chalk/source/index.js").href);
chalk.level = 1;
const interactive = root + "/packages/coding-agent/src/modes/interactive/";
const tools = root + "/packages/coding-agent/src/core/tools/";
const load = (base, name) => import(pathToFileURL(base + name + ".ts").href);

const { initTheme } = await load(interactive, "theme/theme");
const { ToolExecutionComponent } = await load(interactive, "components/tool-execution");
const { explorationCall } = await load(interactive, "components/exploration-call");
const { createAllToolRenderers, withBuiltInRenderers } = await load(tools, "renderers/index");
const { Text } = await import(pathToFileURL(root + "/packages/tui/src/components/text.ts").href);

initTheme("dark", false);
const cwd = "/tmp/project";
const tui = { requestRender: () => {} };
const renderers = createAllToolRenderers();

const custom = {
  renderCall: () => new Text("CUSTOM CALL", 0, 0),
  renderResult: () => new Text("CUSTOM RESULT", 0, 0),
};

const build = (toolName, args, definition) =>
  new ToolExecutionComponent(toolName, "call-1", args, { showImages: false }, definition, tui, cwd, "classic");

const callCases = [];
const scenarios = [
  { name: "no-definition", toolName: "read", args: { file_path: "a.rs" }, definition: undefined },
  { name: "explicit-builtin", toolName: "read", args: { file_path: "a.rs" }, definition: renderers.read },
  { name: "with-builtin-renderers", toolName: "read", args: { file_path: "a.rs" }, definition: withBuiltInRenderers("read", {}) },
  { name: "custom-renderer", toolName: "read", args: { file_path: "a.rs" }, definition: custom },
  { name: "partial-override", toolName: "read", args: { file_path: "a.rs" }, definition: { ...renderers.read, renderResult: custom.renderResult } },
  { name: "skill-read", toolName: "read", args: { file_path: "pkg/skills/rust/SKILL.md" }, definition: renderers.read },
  { name: "grep", toolName: "grep", args: { pattern: "needle", path: "src" }, definition: renderers.grep },
  { name: "find", toolName: "find", args: { pattern: "*.rs", path: "src" }, definition: renderers.find },
  { name: "ls", toolName: "ls", args: { path: "src" }, definition: renderers.ls },
  { name: "non-exploration", toolName: "write", args: { file_path: "a.rs" }, definition: renderers.write },
];
for (const { name, toolName, args, definition } of scenarios) {
  const component = build(toolName, args, definition);
  component.setArgsComplete();
  callCases.push({ name, toolName, args, call: explorationCall(component) ?? null });
}

const customRenderCases = [];
for (const width of [40, 80]) {
  const component = build("read", { file_path: "a.rs" }, custom);
  component.setArgsComplete();
  component.updateResult({ content: [{ type: "text", text: "x" }], isError: false }, false);
  component.stopAnimation();
  customRenderCases.push({ width, lines: component.render(width).map((line) => line.replace(/\s+$/, "")) });
}

writeFileSync(
  import.meta.dir + "/golden/components32-custom.json",
  JSON.stringify({ calls: callCases, customRender: customRenderCases }, null, 2) + "\n",
);
console.log(`Generated ${callCases.length} call and ${customRenderCases.length} custom-render cases`);
