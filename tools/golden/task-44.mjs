import { mkdirSync, writeFileSync } from "node:fs";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { execFileSync } from "node:child_process";
import { pinnedSenpiRoot } from "./pin.mjs";

const senpi = pinnedSenpiRoot();
const upstream = resolve(process.env.OMO_SRC ?? "/home/indo/code/oh-my-openagent");
const expected = "77f3067f157a4f88e6d8ed48b3a6c338654402ed";
const head = execFileSync("git", ["--no-pager", "-C", upstream, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
if (head !== expected) throw new Error(`omo source pin mismatch: ${head}`);
const result = await Bun.build({ entrypoints: [join(upstream, "packages/omo-senpi/src/components/task/renderers.ts")], target: "bun", plugins: [{ name: "task-source-import", setup(builder) {
    builder.onResolve({ filter: /^@earendil-works\/pi-tui$/ }, () => ({ path: join(senpi,"packages/tui/src/index.ts") }));
    builder.onResolve({ filter: /^@oh-my-opencode\/senpi-task$/ }, () => ({ path: "task-render-helpers", namespace: "task-source" }));
    builder.onLoad({ filter: /.*/, namespace: "task-source" }, () => ({ loader: "ts", contents: [
        `export { completionMessageLines } from ${JSON.stringify(join(upstream,"packages/senpi-task/src/completion/notification.ts"))};`,
        `export { linesComponent } from ${JSON.stringify(join(upstream,"packages/senpi-task/src/tools/task/renderers.ts"))};`,
        `export { normalizeRendererText } from ${JSON.stringify(join(upstream,"packages/senpi-task/src/renderer-text.ts"))};`,
    ].join("\n") }));
} }] });
if (!result.success) throw new AggregateError(result.logs, "Upstream renderer bundle failed");
const renderers = await import(`data:text/javascript;base64,${Buffer.from(await result.outputs[0].text()).toString("base64")}`);
const completion = {
    task_id: "st_test", name: "worker", status: "completed", model: "faux/faux-1",
    resolved_model: { source: "explicit", provider: "faux", model_id: "faux-1", display: "faux/faux-1" },
    duration_ms: 1000, tokens: 12, final_response: "finished", continuation_hint: "read output",
};
const cases = [
    { name: "category-empty", renderer: "renderCategoryUnavailable", message: {} },
    { name: "category-control", renderer: "renderCategoryUnavailable", message: { content: "unavailable\nnext\u001b[31m red\u001b[0m" } },
    { name: "completion-empty", renderer: "renderTaskCompletion", message: {} },
    { name: "completion", renderer: "renderTaskCompletion", message: { details: [completion] } },
    { name: "liveness-empty", renderer: "renderTeamMemberLiveness", message: {} },
    { name: "liveness", renderer: "renderTeamMemberLiveness", message: { details: { memberName: "worker", lastKnownState: "lost", reason: "process exited" } } },
];
const outputs = cases.map(test => ({ ...test, renders: [40, 80, 120].map(width => ({ width, lines: renderers[test.renderer](test.message, {}, {}).render(width) })) }));
const out = resolve(import.meta.dir, "../../crates/omo/components/maho-omo-task/tests/golden");
mkdirSync(out, { recursive: true });
writeFileSync(join(out, "renderers.json"), JSON.stringify({ source: expected, cases: outputs }, null, 2) + "\n");
console.log(`Generated ${outputs.length * 3} renderer goldens from ${expected}`);
