// Pinned eval renderer fixtures (senpi 903b8336). Run from the pinned packages/senpi-codemode directory
// (bun resolves its imports there): SENPI_SRC=<pinned senpi> bun <this file>.
import { writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { pinnedSenpiRoot } from "../../../../tools/golden/pin.mjs";
const root = pinnedSenpiRoot();
const { renderEvalCall, renderEvalResult } = await import(pathToFileURL(resolve(root, "packages/senpi-codemode/src/tool/render.ts")).href);
const base = { language: "js", summary: "compute a value", code: "print(41)\n42" };
const cell = (over) => ({ index: 0, language: "js", code: base.code, summary: base.summary, output: "41\n42", status: "complete", durationMs: 1000, ...over });
const scenarios = [
  ["complete", {}, [cell({})], {}],
  ["complete-throughput", {}, [cell({})], { toolCallCount: 3, wallDurationMs: 1500 }],
  ["complete-reset-timeout", { reset: true, timeout: 30 }, [cell({})], {}],
  ["error", {}, [cell({ status: "error", output: "Error: boom" })], {}],
  ["cancelled-nosummary", { summary: undefined }, [cell({ status: "cancelled", summary: undefined, output: "" })], {}],
  ["detached-empty", {}, [cell({ status: "detached", output: "" })], {}],
  ["detached-output", {}, [cell({ status: "detached", output: Array.from({length: 12}, (_, i) => `line ${i}`).join("\n") })], {}],
  ["running", {}, [cell({ status: "running", startedAt: 500, durationMs: undefined, output: "", code: Array.from({length: 9}, (_, i) => `step(${i})`).join("\n") })], {}],
  ["running-output", {}, [cell({ status: "running", startedAt: 500, durationMs: undefined, output: "a\nb\nc\nd\ne" })], {}],
  ["running-status", {}, [cell({ status: "running", startedAt: 500, durationMs: undefined, output: "", statusEvents: [{op:"log",message:"one"},{op:"log",message:"two"},{op:"phase",title:"three"},{op:"log",message:"four"}] })], {}],
  ["queued", {}, [cell({ status: "queued", output: "", durationMs: undefined, queuedBehind: ["c1"] })], {}],
  ["two-cells", {}, [cell({}), cell({ index: 1, status: "error", output: "bad", summary: "second" })], { jsonOutputs: [{ a: 1 }] }],
  ["complete-agent", {}, [cell({ statusEvents: [{ op: "agent", id: "scout", status: "completed", durationMs: 2000 }] })], {}],
];
const cases = [];
for (const [name, argOver, cells, extra] of scenarios) {
  const args = { ...base, ...argOver };
  if (args.summary === undefined) delete args.summary;
  const result = { content: [{ type: "text", text: cells[0].output }], details: { language: "js", summary: args.summary, cells, ...extra } };
  for (const expanded of [false, true]) for (const width of [20, 40, 80]) {
    const context = { args, expanded, hasResult: false, isError: false, spinnerFrame: undefined, now: 2000, invalidate() {} };
    cases.push({ name, kind: "result", width, args, context, result, lines: renderEvalResult(result, { isPartial: false, expanded }, undefined, context).render(width) });
  }
}
for (const argOver of [{}, { reset: true, timeout: 5 }, { summary: undefined }]) for (const expanded of [false, true]) for (const width of [20, 80]) {
  const args = { ...base, ...argOver }; if (args.summary === undefined) delete args.summary;
  const context = { args, expanded, hasResult: false, isError: false, spinnerFrame: undefined, now: 2000, invalidate() {} };
  cases.push({ name: "call", kind: "call", width, args, context, lines: renderEvalCall(args, undefined, context).render(width) });
}
writeFileSync(new URL("./codemode-render.json", import.meta.url), JSON.stringify(cases, null, 2) + "\n");
console.log(cases.length);
