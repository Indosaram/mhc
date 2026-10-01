import { pinnedSenpiRoot } from "../../../tools/golden/pin.mjs";
import { pathToFileURL } from "node:url";
import { writeFileSync } from "node:fs";

const root = pinnedSenpiRoot();
const { default: chalk } = await import(pathToFileURL(root + "/node_modules/chalk/source/index.js").href);
chalk.level = 1;
const interactive = root + "/packages/coding-agent/src/modes/interactive/";
const load = (base, name) => import(pathToFileURL(base + name + ".ts").href);

const { initTheme, getMarkdownTheme } = await load(interactive, "theme/theme");
const { ExplorationTranscriptContainer } = await load(interactive, "components/exploration-transcript-container");
const { ToolExecutionComponent } = await load(interactive, "components/tool-execution");
const { AssistantMessageComponent } = await load(interactive, "components/assistant-message");
const { CustomEntryComponent } = await load(interactive, "components/custom-entry");
const { Text } = await import(pathToFileURL(root + "/packages/tui/src/components/text.ts").href);
const { createAllToolRenderers } = await load(root + "/packages/coding-agent/src/core/tools/", "renderers/index");

initTheme("dark", false);
const markdownTheme = getMarkdownTheme();
const cwd = "/tmp/project";
const tui = { requestRender: () => {} };
const trim = (lines) => lines.map((line) => line.replace(/\s+$/, ""));

const buildChildren = () => {
  const renderers = createAllToolRenderers();
  const read = (filePath) => {
    const component = new ToolExecutionComponent("read", "call-1", { file_path: filePath }, { showImages: false }, renderers.read, tui, cwd);
    component.updateResult({ content: [{ type: "text", text: "body\n" }], isError: false }, false);
    component.stopAnimation();
    return component;
  };
  const grep = () => {
    const component = new ToolExecutionComponent("grep", "call-2", { pattern: "needle", path: "src" }, { showImages: false }, renderers.grep, tui, cwd);
    component.updateResult({ content: [{ type: "text", text: "no matches" }], isError: false }, false);
    component.stopAnimation();
    return component;
  };
  const reasoning = new AssistantMessageComponent(
    { content: [{ type: "thinking", thinking: "reasoning" }], stopReason: "stop" },
    false,
    markdownTheme,
    "Thinking...",
    1,
    [],
  );
  const rules = new CustomEntryComponent(
    { customType: "rule-activation", data: { kind: "project-rules", targetPath: "src", rules: ["r1", "r2"], toolCallId: "call-1" } },
    () => new Text("rules", 0, 0),
  );
  const text = new AssistantMessageComponent(
    { content: [{ type: "text", text: "done" }], stopReason: "stop" },
    false,
    markdownTheme,
    "Thinking...",
    1,
    [],
  );
  return { read, grep, reasoning, rules, text };
};

const transcriptCases = [];
for (const width of [40, 80]) {
  const { read, grep, reasoning, rules, text } = buildChildren();
  const container = new ExplorationTranscriptContainer({ tailBudget: 60, warmChunkSize: 100, requestRender: () => {} });
  container.addChild(read("a.rs"));
  container.addChild(read("b.rs"));
  container.addChild(reasoning);
  container.addChild(rules);
  container.addChild(grep());
  container.addChild(text);
  transcriptCases.push({ name: "grouped", width, lines: trim(container.render(width)) });
}

const fallbackCases = [];
for (const width of [40, 80]) {
  const component = new ToolExecutionComponent("custom_tool", "call-9", { alpha: 1, beta: "two" }, { showImages: false }, undefined, tui, cwd);
  component.markExecutionStarted();
  component.setArgsComplete();
  component.stopAnimation();
  fallbackCases.push({ name: "call-only", width, lines: trim(component.render(width)) });

  const withResult = new ToolExecutionComponent("custom_tool", "call-10", { alpha: 1 }, { showImages: false }, undefined, tui, cwd);
  withResult.markExecutionStarted();
  withResult.setArgsComplete();
  withResult.updateResult({ content: [{ type: "text", text: "custom output" }], isError: false }, false);
  withResult.stopAnimation();
  fallbackCases.push({ name: "with-result", width, lines: trim(withResult.render(width)) });
}

writeFileSync(
  import.meta.dir + "/golden/components32-transcript.json",
  JSON.stringify({ transcript: transcriptCases, fallback: fallbackCases }, null, 2) + "\n",
);
console.log(`Generated ${transcriptCases.length} transcript and ${fallbackCases.length} fallback cases`);
