import { pinnedSenpiRoot } from "../../../tools/golden/pin.mjs";
import { pathToFileURL } from "node:url";
import { writeFileSync } from "node:fs";

const root = pinnedSenpiRoot();
const { default: chalk } = await import(pathToFileURL(root + "/node_modules/chalk/source/index.js").href);
chalk.level = 1;
const interactive = root + "/packages/coding-agent/src/modes/interactive/";
const load = (base, name) => import(pathToFileURL(base + name + ".ts").href);

const { initTheme } = await load(interactive, "theme/theme");
const { createToolCallFallback, createToolResultFallback, formatToolExecutionFallback } = await load(
  interactive,
  "components/tool-execution-fallback",
);

initTheme("dark", false);
const trim = (lines) => lines.map((line) => line.replace(/\s+$/, ""));
const text = (value) => ({ content: [{ type: "text", text: value }], isError: false });

const callCases = [];
for (const toolName of ["read", "a_very_long_tool_name_that_exceeds_the_bound", "  spaced  "]) {
  callCases.push({ toolName, lines: trim(createToolCallFallback(toolName).render(80)) });
}

const resultCases = [];
const resultInputs = [
  { name: "undefined", result: undefined, showImages: false },
  { name: "empty-text", result: text(""), showImages: false },
  { name: "plain-text", result: text("plain output"), showImages: false },
  { name: "json-object", result: text('{"b":2,"a":1}'), showImages: false },
  { name: "json-array", result: text("[1,2,3]"), showImages: false },
  { name: "json-nested", result: text('{"a":{"b":{"c":{"d":1}}}}'), showImages: false },
  {
    name: "json-many-rows",
    result: text(JSON.stringify(Object.fromEntries(Array.from({ length: 30 }, (_, i) => [`key${i}`, i])))),
    showImages: false,
  },
  { name: "json-long-value", result: text(JSON.stringify({ blob: "y".repeat(300) })), showImages: false },
  { name: "json-empty-object", result: text("{}"), showImages: false },
  { name: "ansi-stripped", result: text("\u001b[31mred\u001b[0m"), showImages: false },
  { name: "model-only", result: { content: [{ type: "text", text: "hidden", audience: "model" }], isError: false }, showImages: false },
];
for (const { name, result, showImages } of resultInputs) {
  const component = createToolResultFallback(result, showImages);
  resultCases.push({ name, showImages, result: result ?? null, lines: component ? trim(component.render(80)) : null });
}

const formatCases = [];
const formatInputs = [
  { name: "string-args", toolName: "read", args: { file_path: "a.rs" }, result: text("out") },
  { name: "invalid-content", toolName: "read", args: { file_path: 42 }, result: text("out") },
  { name: "no-result", toolName: "read", args: { file_path: "a.rs" }, result: undefined },
  { name: "long-args", toolName: "write", args: { content: "z".repeat(2500) }, result: undefined },
  { name: "long-output", toolName: "bash", args: { command: "x" }, result: text("o".repeat(2500)) },
  { name: "control-chars", toolName: "bash", args: { command: "a\tb\u0007c" }, result: text("d\ne") },
  { name: "empty-args", toolName: "ls", args: {}, result: text("files") },
  { name: "array-args", toolName: "custom", args: [1, "two", null], result: undefined },
];
for (const { name, toolName, args, result } of formatInputs) {
  formatCases.push({ name, toolName, args, result: result ?? null, output: formatToolExecutionFallback(toolName, args, result, false) });
}

writeFileSync(
  import.meta.dir + "/golden/components32-fallback.json",
  JSON.stringify({ calls: callCases, results: resultCases, formats: formatCases }, null, 2) + "\n",
);
console.log(`Generated ${callCases.length} call, ${resultCases.length} result, ${formatCases.length} format cases`);
