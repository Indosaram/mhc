import { pinnedSenpiRoot } from "../../../tools/golden/pin.mjs";
import { pathToFileURL } from "node:url";
import { writeFileSync } from "node:fs";

const root = pinnedSenpiRoot();
const { default: chalk } = await import(pathToFileURL(root + "/node_modules/chalk/source/index.js").href);
chalk.level = 1;
const interactive = root + "/packages/coding-agent/src/modes/interactive/";
const load = (base, name) => import(pathToFileURL(base + name + ".ts").href);

const { initTheme } = await load(interactive, "theme/theme");
const { ToolExecutionComponent } = await load(interactive, "components/tool-execution");
const { Text } = await import(pathToFileURL(root + "/packages/tui/src/components/text.ts").href);
const { KeybindingsManager } = await import(pathToFileURL(root + "/packages/coding-agent/src/core/keybindings.ts").href);
const { setKeybindings } = await import(pathToFileURL(root + "/packages/tui/src/index.ts").href);

initTheme("dark", false);
// The interactive app installs the full keybinding registry at startup (cli/startup-ui.ts); the
// collapse hint reads through it, so the fixtures must too.
setKeybindings(KeybindingsManager.create());
const cwd = "/tmp/project";
const tui = { requestRender: () => {} };
const trim = (lines) => lines.map((line) => line.replace(/\s+$/, ""));

// A definition with renderCall but no renderResult: senpi's card must fall back to the plain result
// fallback and collapse it to the preview budget.
const callOnly = { renderCall: () => new Text("CALL ONLY", 0, 0) };
const shortOutput = "line\n".repeat(4);
const longOutput = Array.from({ length: 15 }, (_, i) => `line ${i + 1}`).join("\n");

const cases = [];
for (const width of [40, 80]) {
  for (const [name, output] of [["short", shortOutput], ["long", longOutput]]) {
    for (const expanded of [false, true]) {
      const component = new ToolExecutionComponent(
        "custom_tool",
        "call-1",
        { alpha: 1 },
        { showImages: false },
        callOnly,
        tui,
        cwd,
        "classic",
      );
      component.setArgsComplete();
      component.updateResult({ content: [{ type: "text", text: output }], isError: false }, false);
      component.setExpanded(expanded);
      component.stopAnimation();
      cases.push({ name, width, expanded, output, lines: trim(component.render(width)) });
    }
  }
}

writeFileSync(
  import.meta.dir + "/golden/components32-callonly.json",
  JSON.stringify({ cases }, null, 2) + "\n",
);
console.log(`Generated ${cases.length} call-only cases`);
