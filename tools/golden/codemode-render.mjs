import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { pinnedSenpiRoot } from "./pin.mjs";

const root = pinnedSenpiRoot();
const { renderEvalCall, renderEvalResult } = await import(pathToFileURL(resolve(root, "packages/senpi-codemode/src/tool/render.ts")).href);
const repository = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const args = { language: "py", summary: "compute a value", code: "print(41)\n42" };
const result = { content: [{ type: "text", text: "41\n42" }], details: { language: "py", summary: args.summary, cells: [{ index: 0, language: "py", code: args.code, summary: args.summary, output: "41\n42", status: "complete", durationMs: 1000 }] } };
const context = { args, expanded: false, hasResult: false, isError: false, spinnerFrame: undefined, now: 2000, invalidate() {} };
const cases = [];
for (const width of [20, 80]) {
    cases.push({ kind: "call", width, args, context, lines: renderEvalCall(args, undefined, context).render(width) });
    cases.push({ kind: "result", width, args, context, result, lines: renderEvalResult(result, { isPartial: false }, undefined, context).render(width) });
}
const directory = resolve(repository, "crates/maho-codemode/tests/golden");
mkdirSync(directory, { recursive: true });
writeFileSync(resolve(directory, "codemode-render.json"), JSON.stringify(cases, null, 2) + "\n");
console.log(`Generated ${cases.length} pinned codemode renderer cases`);
