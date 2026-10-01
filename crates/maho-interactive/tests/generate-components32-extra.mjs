import { pinnedSenpiRoot } from "../../../tools/golden/pin.mjs";
import { pathToFileURL } from "node:url";
import { writeFileSync } from "node:fs";

const root = pinnedSenpiRoot();
const { default: chalk } = await import(pathToFileURL(root + "/node_modules/chalk/source/index.js").href);
chalk.level = 1;
const interactive = root + "/packages/coding-agent/src/modes/interactive/";
const load = (base, name) => import(pathToFileURL(base + name + ".ts").href);

const { initTheme, getMarkdownTheme, theme } = await load(interactive, "theme/theme");
const { createMermaidMarkdownTransformer } = await load(interactive, "components/mermaid");
const { BorderedLoader } = await load(interactive, "components/bordered-loader");
const { BashExecutionComponent } = await load(interactive, "components/bash-execution");
const { UserMessageSelectorComponent } = await load(interactive, "components/user-message-selector");
const { CustomEntryComponent } = await load(interactive, "components/custom-entry");
const { ToolExecutionImages } = await load(interactive, "components/tool-execution-images");
const { Text } = await import(pathToFileURL(root + "/packages/tui/src/components/text.ts").href);

initTheme("dark", false);
const markdownTheme = getMarkdownTheme();
const tui = { requestRender: () => {} };
const trim = (lines) => lines.map((line) => line.replace(/\s+$/, ""));

const mermaidCases = [];
const transformerFor = (mode) => createMermaidMarkdownTransformer({ getMode: () => mode, theme });
const mermaidInputs = [
  { name: "unsupported-type", markdown: "```mermaid\npie title Pets\n  \"Dogs\" : 3\n```\n" },
  { name: "blank-block", markdown: "```mermaid\n\n```\n" },
  { name: "not-mermaid-lang", markdown: "```rust\nfn main() {}\n```\n" },
  { name: "plain-text", markdown: "just text\n" },
];
for (const { name, markdown } of mermaidInputs) {
  for (const mode of ["off", "streaming", "on"]) {
    for (const isStreaming of [false, true]) {
      for (const messageType of ["assistant", "assistant-thinking"]) {
        const transform = transformerFor(mode);
        mermaidCases.push({
          name,
          markdown,
          mode,
          isStreaming,
          messageType,
          availableWidth: 80,
          output: transform(markdown, { messageType, isStreaming, availableWidth: 80 }),
        });
      }
    }
  }
}

const borderedCases = [];
for (const width of [40, 80]) {
  for (const cancellable of [true, false]) {
    const loader = new BorderedLoader(tui, theme, "Working...", { cancellable });
    borderedCases.push({ width, cancellable, lines: trim(loader.render(width)) });
    loader.dispose();
  }
}

const bashCases = [];
for (const width of [40, 80]) {
  for (const scenario of [
    { name: "complete", excludeFromContext: false, output: ["hello", "world"], exitCode: 0, cancelled: false },
    { name: "error", excludeFromContext: false, output: ["boom"], exitCode: 2, cancelled: false },
    { name: "cancelled", excludeFromContext: false, output: [], exitCode: undefined, cancelled: true },
    { name: "excluded", excludeFromContext: true, output: ["quiet"], exitCode: 0, cancelled: false },
  ]) {
    const component = new BashExecutionComponent("echo hello", tui, scenario.excludeFromContext);
    for (const line of scenario.output) component.appendOutput(`${line}\n`);
    component.setComplete(scenario.exitCode, scenario.cancelled);
    bashCases.push({
      name: scenario.name,
      width,
      excludeFromContext: scenario.excludeFromContext,
      output: scenario.output,
      exitCode: scenario.exitCode ?? null,
      cancelled: scenario.cancelled,
      lines: trim(component.render(width)),
    });
  }
}

const selectorCases = [];
const messages = [
  { id: "e1", text: "first message" },
  { id: "e2", text: "second\nmessage" },
  { id: "e3", text: "third message" },
];
for (const width of [40, 80]) {
  const component = new UserMessageSelectorComponent(messages, () => {}, () => {}, "e2");
  selectorCases.push({ width, initialSelectedId: "e2", messages, lines: trim(component.render(width)) });
}

const entryCases = [];
const entries = [
  { customType: "note", data: { text: "hello" } },
  { customType: "note", data: {} },
];
for (const width of [40, 80]) {
  for (const entry of entries) {
    const renderer = (value) => (value.data?.text ? new Text(`entry: ${value.data.text}`, 0, 0) : undefined);
    const component = new CustomEntryComponent(entry, renderer);
    entryCases.push({ width, entry, hasContent: component.hasContent(), lines: trim(component.render(width)) });
  }
}

const imageCases = [];
for (const width of [40, 80]) {
  for (const showImages of [true, false]) {
    const images = new ToolExecutionImages(() => {});
    images.updateOptions({ showImages, maxWidthCells: 60, showRendererFallback: false });
    images.updateResult({ content: [{ type: "image", data: "aGVsbG8=", mimeType: "image/png" }], isError: false });
    imageCases.push({ width, showImages, lines: trim(images.render(width)) });
  }
}

writeFileSync(
  import.meta.dir + "/golden/components32-extra.json",
  JSON.stringify(
    { mermaid: mermaidCases, bordered: borderedCases, bash: bashCases, selector: selectorCases, entry: entryCases, images: imageCases },
    null,
    2,
  ) + "\n",
);
console.log(
  `Generated ${mermaidCases.length} mermaid, ${borderedCases.length} bordered, ${bashCases.length} bash, ${selectorCases.length} selector, ${entryCases.length} entry, ${imageCases.length} image cases`,
);
