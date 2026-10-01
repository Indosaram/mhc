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
const { ExplorationGroup } = await load(interactive, "components/exploration-group");
const { ToolExecutionComponent } = await load(interactive, "components/tool-execution");
const { createAllToolRenderers } = await load(tools, "renderers/index");

initTheme("dark", false);
const cwd = "/tmp/project";
const tui = { requestRender: () => {} };
const renderers = createAllToolRenderers();
const trim = (lines) => lines.map((line) => line.replace(/\s+$/, ""));

const makeCard = (filePath, id) => {
  const component = new ToolExecutionComponent("read", id, { file_path: filePath }, { showImages: false }, renderers.read, tui, cwd);
  component.updateResult({ content: [{ type: "text", text: "body\n" }], isError: false }, false);
  component.stopAnimation();
  return component;
};

const event = (overrides) => ({
  type: "click",
  button: "left",
  x: 0,
  y: 1,
  screenX: 0,
  screenY: 1,
  width: 80,
  height: 10,
  shift: false,
  alt: false,
  ctrl: false,
  ...overrides,
});

const cases = [];
const buildGroup = () => {
  const group = new ExplorationGroup();
  const cards = [makeCard("a.rs", "call-1"), makeCard("b.rs", "call-2")];
  const calls = cards.map((component) => ({
    component,
    call: { action: "Read", label: "x", pending: false, failed: false },
  }));
  group.setMembers(cards, calls, []);
  return group;
};

const scenario = (name, events) => {
  const group = buildGroup();
  const steps = [];
  for (const ev of events) {
    const result = group.handleMouse(event(ev));
    steps.push({
      event: ev,
      handled: result !== undefined && result !== null,
      capture: result?.capture ?? null,
      focus: result?.focus ?? null,
      hasTarget: result?.target !== undefined,
      lines: trim(group.render(80)),
    });
  }
  cases.push({ name, steps });
};

scenario("click-y1-toggles-on", [{}, {}]);
scenario("press-y1-handled-without-toggle", [{ type: "press" }]);
scenario("click-y0-not-handled", [{ y: 0 }]);
scenario("click-right-not-handled", [{ button: "right" }]);
scenario("click-y3-collapsed-not-handled", [{ y: 3 }]);
scenario("click-y5-then-y3-expanded-delegates", [{ y: 5 }, { y: 3 }]);

writeFileSync(import.meta.dir + "/golden/components32-mouse.json", JSON.stringify({ cases }, null, 2) + "\n");
console.log(`Generated ${cases.length} mouse cases`);
