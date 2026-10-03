import { pathToFileURL } from "node:url";
import { pinnedSenpiRoot } from "./pin.mjs";

const root = pinnedSenpiRoot();
const { estimateTokens } = await import(pathToFileURL(`${root}/packages/coding-agent/src/core/compaction/compaction.ts`).href);
const text = "\u{1f600}".repeat(5);
const messages = [
  { role: "user", content: text },
  { role: "assistant", content: [{ type: "text", text }] },
  { role: "assistant", content: [{ type: "thinking", thinking: text }] },
  { role: "toolResult", content: [{ type: "text", text }] },
  { role: "custom", content: text },
  { role: "bashExecution", command: text, output: "" },
  { role: "branchSummary", summary: text },
  { role: "compactionSummary", summary: text },
  { role: "assistant", content: [{ type: "toolCall", name: text, arguments: {} }] },
  { role: "user", content: `${"A".repeat(600)} ${text}` },
];
console.log(JSON.stringify(messages.map(message => ({ message, tokens: estimateTokens(message) }))));
