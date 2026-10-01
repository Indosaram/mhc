import { pinnedSenpiRoot } from "../../../tools/golden/pin.mjs";
import { pathToFileURL } from "node:url";
import { writeFileSync } from "node:fs";

const root = pinnedSenpiRoot();
const { default: chalk } = await import(pathToFileURL(root + "/node_modules/chalk/source/index.js").href);
chalk.level = 1;
const interactive = root + "/packages/coding-agent/src/modes/interactive/";
const load = (base, name) => import(pathToFileURL(base + name + ".ts").href);

const { initTheme } = await load(interactive, "theme/theme");
const { createAssistantRenderDescriptors } = await load(interactive, "components/assistant-render-descriptors");

initTheme("dark", false);

const base = { expanded: false, providerErrorOwned: false, hiddenThinkingLabel: "Thinking...", hideThinkingBlock: false, hasToolCalls: false };

const scenarios = [
  { name: "text-only", message: { content: [{ type: "text", text: "hello" }], stopReason: "stop" }, options: {} },
  { name: "blank-text", message: { content: [{ type: "text", text: "   " }], stopReason: "stop" }, options: {} },
  { name: "thinking-untimed", message: { content: [{ type: "thinking", thinking: "why" }], stopReason: "stop" }, options: {} },
  { name: "thinking-hidden", message: { content: [{ type: "thinking", thinking: "why" }], stopReason: "stop" }, options: { hideThinkingBlock: true } },
  {
    name: "thinking-override-visible",
    message: { content: [{ type: "thinking", thinking: "why" }], stopReason: "stop" },
    options: { hideThinkingBlock: true, thinkingVisibilityOverrides: [[0, false]] },
  },
  {
    name: "two-thinking-runs",
    message: {
      content: [
        { type: "thinking", thinking: "first", startedAt: 0, endedAt: 500 },
        { type: "text", text: "between" },
        { type: "thinking", thinking: "second", startedAt: 1000, endedAt: 2000 },
      ],
      stopReason: "stop",
    },
    options: {},
  },
  ...[
    ["duration-ms", 0, 900],
    ["duration-seconds", 0, 1500],
    ["duration-minutes", 0, 125000],
    ["duration-hours", 0, 7500000],
    ["duration-days", 0, 180000000],
  ].map(([name, startedAt, endedAt]) => ({
    name,
    message: { content: [{ type: "thinking", thinking: "t", startedAt, endedAt }], stopReason: "stop" },
    options: {},
  })),
  { name: "provider-native-collapsed", message: { content: [{ type: "providerNative", subtype: "web", raw: { a: 1 } }], provider: "openai", stopReason: "stop" }, options: {} },
  { name: "provider-native-expanded", message: { content: [{ type: "providerNative", subtype: "web", raw: { a: 1 } }], provider: "openai", stopReason: "stop" }, options: { expanded: true } },
  {
    name: "provider-native-large-body",
    message: { content: [{ type: "providerNative", subtype: "big", raw: { blob: "x".repeat(3000) } }], provider: "openai", stopReason: "stop" },
    options: {},
  },
  {
    name: "provider-native-astral-body",
    message: { content: [{ type: "providerNative", subtype: "big", raw: { blob: "\u{1f600}".repeat(1500) } }], provider: "openai", stopReason: "stop" },
    options: {},
  },
  {
    name: "provider-native-bmp-wide-body",
    message: { content: [{ type: "providerNative", subtype: "big", raw: { blob: "\u{ac00}".repeat(2500) } }], provider: "openai", stopReason: "stop" },
    options: {},
  },
  { name: "length", message: { content: [{ type: "text", text: "cut" }], stopReason: "length" }, options: {} },
  { name: "aborted-plain", message: { content: [{ type: "text", text: "part" }], stopReason: "aborted", errorMessage: "Request was aborted" }, options: {} },
  { name: "aborted-message", message: { content: [{ type: "text", text: "part" }], stopReason: "aborted", errorMessage: "user cancelled" }, options: {} },
  { name: "aborted-owned", message: { content: [{ type: "text", text: "part" }], stopReason: "aborted", errorMessage: "x" }, options: { providerErrorOwned: true } },
  { name: "aborted-with-tools", message: { content: [{ type: "toolCall", id: "t" }], stopReason: "aborted", errorMessage: "x" }, options: { hasToolCalls: true } },
  { name: "error", message: { content: [{ type: "text", text: "part" }], stopReason: "error", errorMessage: "boom" }, options: {} },
  { name: "error-server-fallback", message: { content: [{ type: "text", text: "part" }], stopReason: "error", errorMessage: "boom", diagnostics: [{ type: "server_fallback_aborted" }] }, options: {} },
  { name: "error-empty", message: { content: [{ type: "text", text: "part" }], stopReason: "error" }, options: {} },
];

/**
 * A 2000-UTF-16-unit cut can split a surrogate pair, leaving a lone surrogate. Rust strings are
 * valid UTF-8 and JSON cannot carry a lone surrogate, so the fixture records the U+FFFD a terminal
 * renders for it; the Rust port produces the same character via `from_utf16_lossy`.
 */
const wellFormed = (text) =>
  text.replace(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/g, "\uFFFD");

const cases = scenarios.map(({ name, message, options }) => {
  const merged = { ...base, ...options };
  const thinkingVisibilityOverrides = new Map(merged.thinkingVisibilityOverrides ?? []);
  const descriptors = createAssistantRenderDescriptors(message, { ...merged, thinkingVisibilityOverrides });
  return {
    name,
    message,
    options: { ...merged, thinkingVisibilityOverrides: merged.thinkingVisibilityOverrides ?? [] },
    descriptors: descriptors.map((descriptor) => ({ ...descriptor, text: wellFormed(descriptor.text) })),
  };
});

writeFileSync(import.meta.dir + "/golden/components32-descriptors.json", JSON.stringify(cases, null, 2) + "\n");
console.log(`Generated ${cases.length} descriptor cases`);
