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
const { createBoundedRenderSignature } = await load(interactive, "components/render-signature");
const { sourceBox } = await import(pathToFileURL(root + "/node_modules/grok-mermaid/dist/index.js").href);

initTheme("dark", false);

const signatureCases = [
  { value: "short" },
  { value: "x".repeat(400) },
  { value: { b: 1, a: [1, 2, 3], c: null } },
  { value: Array.from({ length: 60 }, (_, i) => i) },
  { value: Array.from({ length: 120 }, (_, i) => ({ key: i, text: "v".repeat(200) })) },
  { value: { nested: { deep: { deeper: { deepest: { value: 1 } } } } } },
  { value: 42 },
  { value: true },
  { value: null },
];

const boxCases = [
  { src: "graph TD\n  A --> B\n", width: null },
  { src: "graph LR; A-->B;\n", width: null },
  { src: "sequenceDiagram\n  A->>B: hi\n", width: 40 },
  { src: "flowchart TD\n  subgraph one\n    A --> B\n  end\n", width: 24 },
  { src: "no diagram type here\n", width: null },
  { src: "graph TD\n\tA --> B\n\r\n", width: null },
];

const data = {
  signatures: signatureCases.map(({ value }) => ({ value, signature: createBoundedRenderSignature(value) })),
  sourceBoxes: boxCases.map(({ src, width }) => ({
    src,
    width,
    art: sourceBox(src, width === null ? undefined : width),
  })),
};

writeFileSync(import.meta.dir + "/golden/components32-tools.json", JSON.stringify(data, null, 2) + "\n");
console.log("Generated components32 tool fixtures from pinned Senpi");
