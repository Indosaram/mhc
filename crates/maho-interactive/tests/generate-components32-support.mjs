import { pinnedSenpiRoot } from "../../../tools/golden/pin.mjs";
import { pathToFileURL } from "node:url";
import { writeFileSync } from "node:fs";

const root = pinnedSenpiRoot();
const { default: chalk } = await import(pathToFileURL(root + "/node_modules/chalk/source/index.js").href);
chalk.level = 1;
const interactive = root + "/packages/coding-agent/src/modes/interactive/";
const load = (base, name) => import(pathToFileURL(base + name + ".ts").href);

const { initTheme } = await load(interactive, "theme/theme");
const todoStrike = await load(interactive, "components/todo-strike");
const hints = await load(interactive, "components/keybinding-hints");
const { KeybindingsManager } = await import(pathToFileURL(root + "/packages/coding-agent/src/core/keybindings.ts").href);
const { setKeybindings } = await import(pathToFileURL(root + "/packages/tui/src/index.ts").href);

initTheme("dark", false);
// The interactive app installs the full keybinding registry at startup (cli/startup-ui.ts);
// keyText/keyHint read through it, so the fixtures must too.
setKeybindings(KeybindingsManager.create());

const strikeCases = [];
for (const text of ["", "a", "task one", "a much longer todo entry", "  spaces  "]) {
  for (const frame of [undefined, 0, 1, 2, 3, 7, 13, 14, 20]) {
    strikeCases.push({ text, frame: frame ?? null, count: todoStrike.strikeRevealCount(text, frame) ?? null });
  }
}
for (const details of [undefined, null, {}, { completedTasks: [] }, { completedTasks: [1] }, { completedTasks: [1, 2] }]) {
  strikeCases.push({
    kind: "hasCompleted",
    details: details ?? null,
    hasCompleted: todoStrike.hasCompletedTodoTasks(details),
  });
}

const partialCases = [];
const strikeFn = (value) => `<${value}>`;
for (const text of ["", "abc", "task one"]) {
  for (const visible of [-1, 0, 1, 3, 100]) {
    partialCases.push({ text, visible, output: todoStrike.partialStrikethrough(text, visible, strikeFn) });
  }
}

const keyTextCases = [];
for (const key of ["escape", "alt+up", "ctrl+o", "ctrl+shift+f", "up/down", "enter", "esc+alt"]) {
  for (const capitalize of [false, true]) {
    keyTextCases.push({ key, capitalize, output: hints.formatKeyText(key, { capitalize }) });
  }
}
const keyHintCases = [];
for (const [keybinding, description] of [
  ["app.tools.expand", "to expand"],
  ["tui.select.cancel", "cancel"],
  ["tui.select.confirm", "confirm"],
]) {
  keyHintCases.push({
    keybinding,
    description,
    keyText: hints.keyText(keybinding),
    keyDisplayText: hints.keyDisplayText(keybinding),
    keyHint: hints.keyHint(keybinding, description),
  });
}
const rawKeyCases = [];
for (const [key, description] of [["escape", "cancel"], ["ctrl+c", "quit"]]) {
  rawKeyCases.push({ key, description, output: hints.rawKeyHint(key, description) });
}

writeFileSync(
  import.meta.dir + "/golden/components32-support.json",
  JSON.stringify({ strike: strikeCases, partial: partialCases, keyText: keyTextCases, keyHint: keyHintCases, rawKey: rawKeyCases }, null, 2) + "\n",
);
console.log(
  `Generated ${strikeCases.length} strike, ${partialCases.length} partial, ${keyTextCases.length} keyText, ${keyHintCases.length} keyHint, ${rawKeyCases.length} rawKey cases`,
);
