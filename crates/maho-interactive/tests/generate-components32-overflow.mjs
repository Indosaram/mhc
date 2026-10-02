import { pinnedSenpiRoot } from "../../../tools/golden/pin.mjs";
import { pathToFileURL } from "node:url";
import { writeFileSync } from "node:fs";

const root = pinnedSenpiRoot();
const { default: chalk } = await import(pathToFileURL(root + "/node_modules/chalk/source/index.js").href);
chalk.level = 1;
const interactive = root + "/packages/coding-agent/src/modes/interactive/";
const load = (base, name) => import(pathToFileURL(base + name + ".ts").href);

const { initTheme, getMarkdownTheme } = await load(interactive, "theme/theme");
const { ExplorationGroup } = await load(interactive, "components/exploration-group");
const { AssistantMessageComponent } = await load(interactive, "components/assistant-message");
const { UserMessageSelectorComponent } = await load(interactive, "components/user-message-selector");

initTheme("dark", false);
const markdownTheme = getMarkdownTheme();
const trim = (lines) => lines.map((line) => line.replace(/\s+$/, ""));
const notExpanded = { presentationSnapshot: { state: { expanded: false } } };

const groupCases = [];
const makeGroup = (calls) => {
  const group = new ExplorationGroup();
  group.setMembers([], calls.map((call) => ({ call, component: notExpanded })), []);
  return group;
};
const searchCalls = Array.from({ length: 11 }, (_, i) => ({ action: "Search", label: `pattern${i} in src`, pending: false, failed: false }));
const mixedCalls = [
  { action: "Read", label: "a.rs", pending: false, failed: false },
  { action: "Read", label: "b.rs", pending: false, failed: false },
  ...Array.from({ length: 9 }, (_, i) => ({ action: "Search", label: `p${i}`, pending: false, failed: false })),
];
const ruleCalls = [{ action: "List", label: ".", pending: false, failed: false }];
for (const width of [40, 80]) {
  groupCases.push({ name: "overflow", width, calls: searchCalls, rules: [], lines: trim(makeGroup(searchCalls).render(width)) });
  groupCases.push({ name: "mixed-overflow", width, calls: mixedCalls, rules: [], lines: trim(makeGroup(mixedCalls).render(width)) });
  const withRules = new ExplorationGroup();
  withRules.setMembers([], ruleCalls.map((call) => ({ call, component: notExpanded })), ["r1", "r2", "r1"]);
  groupCases.push({ name: "rules-deduped", width, calls: ruleCalls, rules: ["r1", "r2", "r1"], lines: trim(withRules.render(width)) });
}

const assistantCases = [];
const thinkingMessage = { content: [{ type: "thinking", thinking: "reasoning" }, { type: "text", text: "done" }], stopReason: "stop" };
for (const width of [40, 80]) {
  const variants = [
    { name: "hide-thinking", apply: (c) => c.setHideThinkingBlock(true) },
    { name: "hidden-label", apply: (c) => c.setHiddenThinkingLabel("Hmm...") },
    { name: "output-pad", apply: (c) => c.setOutputPad(3) },
    { name: "expanded", apply: (c) => c.setExpanded(true) },
  ];
  for (const { name, apply } of variants) {
    const component = new AssistantMessageComponent(thinkingMessage, false, markdownTheme, "Thinking...", 1, []);
    apply(component);
    assistantCases.push({
      name,
      width,
      message: thinkingMessage,
      hideThinkingBlock: false,
      isExplorationDetail: component.isExplorationDetail,
      lines: trim(component.render(width)),
    });
  }
  const detail = new AssistantMessageComponent({ content: [{ type: "thinking", thinking: "why" }], stopReason: "stop" }, false, markdownTheme, "Thinking...", 1, []);
  assistantCases.push({
    name: "exploration-detail",
    width,
    message: { content: [{ type: "thinking", thinking: "why" }], stopReason: "stop" },
    hideThinkingBlock: false,
    isExplorationDetail: detail.isExplorationDetail,
    lines: trim(detail.render(width)),
  });
  const hiddenDetail = new AssistantMessageComponent({ content: [{ type: "thinking", thinking: "why" }], stopReason: "stop" }, true, markdownTheme, "Thinking...", 1, []);
  assistantCases.push({
    name: "exploration-detail-hidden",
    width,
    message: { content: [{ type: "thinking", thinking: "why" }], stopReason: "stop" },
    hideThinkingBlock: true,
    isExplorationDetail: hiddenDetail.isExplorationDetail,
    lines: trim(hiddenDetail.render(width)),
  });
}

const selectorCases = [];
for (const width of [40, 80]) {
  const many = Array.from({ length: 14 }, (_, i) => ({ id: `e${i}`, text: `message ${i}` }));
  const empty = new UserMessageSelectorComponent([], () => {}, () => {}, undefined);
  selectorCases.push({ name: "empty", width, messages: [], initialSelectedId: null, lines: trim(empty.render(width)) });
  const first = new UserMessageSelectorComponent(many, () => {}, () => {}, "e0");
  selectorCases.push({ name: "scroll-first", width, messages: many, initialSelectedId: "e0", lines: trim(first.render(width)) });
  const last = new UserMessageSelectorComponent(many, () => {}, () => {}, "e13");
  selectorCases.push({ name: "scroll-last", width, messages: many, initialSelectedId: "e13", lines: trim(last.render(width)) });
}

writeFileSync(
  import.meta.dir + "/golden/components32-overflow.json",
  JSON.stringify({ groups: groupCases, assistant: assistantCases, selectors: selectorCases }, null, 2) + "\n",
);
console.log(`Generated ${groupCases.length} group, ${assistantCases.length} assistant, ${selectorCases.length} selector cases`);
