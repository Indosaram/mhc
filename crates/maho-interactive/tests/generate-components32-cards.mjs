import { pinnedSenpiRoot } from "../../../tools/golden/pin.mjs";
import { pathToFileURL } from "node:url";
import { writeFileSync } from "node:fs";

const root = pinnedSenpiRoot();
const { default: chalk } = await import(pathToFileURL(root + "/node_modules/chalk/source/index.js").href);
chalk.level = 1;
const interactive = root + "/packages/coding-agent/src/modes/interactive/";
const tools = root + "/packages/coding-agent/src/core/tools/";
const load = (base, name) => import(pathToFileURL(base + name + ".ts").href);

const { initTheme, getMarkdownTheme } = await load(interactive, "theme/theme");
const { AssistantMessageComponent } = await load(interactive, "components/assistant-message");
const { UserMessageComponent } = await load(interactive, "components/user-message");
const { CustomMessageComponent } = await load(interactive, "components/custom-message");
const { ExplorationGroup } = await load(interactive, "components/exploration-group");
const { ToolExecutionComponent } = await load(interactive, "components/tool-execution");
const { withBuiltInRenderers } = await load(tools, "renderers/index");

initTheme("dark", false);

const markdownTheme = getMarkdownTheme();
const cwd = "/tmp/project";
const widths = [40, 80];
const trim = (lines) => lines.map((line) => line.replace(/\s+$/, ""));

const assistantCases = [];
const messages = [
  {
    name: "text-only",
    message: { content: [{ type: "text", text: "Hello **world**" }], stopReason: "stop" },
  },
  {
    name: "thinking-visible",
    message: {
      content: [
        { type: "thinking", thinking: "Let me think.", startedAt: 0, endedAt: 1500 },
        { type: "text", text: "Done." },
      ],
      stopReason: "stop",
    },
  },
  {
    name: "thinking-hidden",
    message: { content: [{ type: "thinking", thinking: "secret" }], stopReason: "stop" },
    hideThinkingBlock: true,
  },
  {
    name: "provider-native",
    message: {
      content: [{ type: "providerNative", subtype: "web_search", raw: { query: "rust" } }],
      provider: "openai",
      stopReason: "stop",
    },
  },
  {
    name: "length-error",
    message: { content: [{ type: "text", text: "truncated" }], stopReason: "length" },
  },
  {
    name: "aborted",
    message: { content: [{ type: "text", text: "partial" }], stopReason: "aborted", errorMessage: "Request was aborted" },
  },
  {
    name: "tool-calls",
    message: {
      content: [{ type: "text", text: "calling" }, { type: "toolCall", id: "t1", name: "read", arguments: {} }],
      stopReason: "toolUse",
    },
  },
];
for (const { name, message, hideThinkingBlock = false } of messages) {
  for (const width of widths) {
    for (const expanded of [false, true]) {
      const component = new AssistantMessageComponent(message, hideThinkingBlock, markdownTheme, "Thinking...", 1, []);
      component.setExpanded(expanded);
      assistantCases.push({ name, width, expanded, hideThinkingBlock, message, lines: trim(component.render(width)) });
    }
  }
}

const userCases = [];
for (const text of ["Hello **world**", "1. first\n2. second", "plain"]) {
  for (const width of widths) {
    const component = new UserMessageComponent(text, markdownTheme, 1, []);
    userCases.push({ text, width, lines: trim(component.render(width)) });
  }
}

const customCases = [];
for (const width of widths) {
  const component = new CustomMessageComponent({ customType: "note", content: "custom **body**" });
  customCases.push({ width, lines: trim(component.render(width)) });
  const expanded = new CustomMessageComponent({ customType: "note", content: "custom **body**" });
  expanded.setExpanded(true);
  customCases.push({ width, expanded: true, lines: trim(expanded.render(width)) });
}

const explorationCases = [];
for (const width of widths) {
  const group = new ExplorationGroup();
  group.setMembers([], [
    { call: { action: "Read", label: "a.rs", pending: false, failed: false }, component: { presentationSnapshot: { state: { expanded: false } } } },
    { call: { action: "Read", label: "b.rs", pending: false, failed: false }, component: { presentationSnapshot: { state: { expanded: false } } } },
    { call: { action: "Search", label: "needle in src", pending: false, failed: false }, component: { presentationSnapshot: { state: { expanded: false } } } },
    { call: { action: "List", label: ".", pending: false, failed: true }, component: { presentationSnapshot: { state: { expanded: false } } } },
  ], ["rule-a", "rule-b", "rule-a"]);
  explorationCases.push({ width, lines: trim(group.render(width)) });
}

const toolCases = [];
const tui = { requestRender: () => {} };
const toolScenarios = [
  { toolName: "read", args: { file_path: "src/main.rs" }, result: { content: [{ type: "text", text: "fn main() {}\n" }], isError: false } },
  { toolName: "ls", args: { path: "src" }, result: { content: [{ type: "text", text: "a.rs\nb.rs" }], isError: false } },
  { toolName: "grep", args: { pattern: "needle", path: "src" }, result: { content: [{ type: "text", text: "no matches" }], isError: false } },
  { toolName: "read", args: { file_path: "missing.rs" }, result: { content: [{ type: "text", text: "ENOENT: no such file" }], isError: true } },
];
for (const { toolName, args, result } of toolScenarios) {
  for (const width of widths) {
    for (const expanded of [false, true]) {
      const component = new ToolExecutionComponent(
        toolName,
        "call-1",
        args,
        { showImages: false },
        withBuiltInRenderers(toolName, undefined),
        tui,
        cwd,
        "classic",
      );
      component.setExpanded(expanded);
      component.updateResult(result, false);
      component.stopAnimation();
      toolCases.push({ toolName, width, expanded, args, result, lines: trim(component.render(width)) });
    }
  }
}

const data = {
  assistant: assistantCases,
  user: userCases,
  custom: customCases,
  exploration: explorationCases,
  tool: toolCases,
};
writeFileSync(import.meta.dir + "/golden/components32-cards.json", JSON.stringify(data, null, 2) + "\n");
console.log(
  `Generated ${assistantCases.length} assistant, ${userCases.length} user, ${customCases.length} custom, ${explorationCases.length} exploration, ${toolCases.length} tool cases`,
);
