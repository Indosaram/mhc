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
    { name: "completion-stats", renderer: "renderTaskCompletion", message: { details: [{...completion,name:"stats-member",category:"quick",model:"apitopia/z-ai/glm-5.2-ultrafast-unlocked",resolved_model:undefined,duration_ms:65000,run_stats:{runtime_ms:65000,turns:3,tool_calls:4,output_tokens:840,tokens_per_second:250},final_response:"team statistics member complete",continuation_hint:'Use task_send({ to: "stats-member", message: "..." }) to continue.'}] } },
    { name: "completion-wide-continuation", renderer: "renderTaskCompletion", message: { details: [{...completion,model:"quotio-openai/gpt-5.6-luna-fast",resolved_model:undefined,duration_ms:1250,final_response:"검증 작업을 완료했습니다.",continuation_hint:'Use task_send({ to: "st_done", message: "continue with the remaining evidence and report the result" }) to continue.'}] } },
    { name: "completion-controls", renderer: "renderTaskCompletion", message: { details: [{...completion,name:"한글\u001b[31m worker",final_response:"결과\u0007 complete"}] } },
    { name: "liveness-empty", renderer: "renderTeamMemberLiveness", message: {} },
    { name: "liveness", renderer: "renderTeamMemberLiveness", message: { details: { memberName: "worker", lastKnownState: "lost", reason: "process exited" } } },
];
const outputs = cases.map(test => ({ ...test, renders: [40, 54, 80, 120, 140].map(width => ({ width, lines: renderers[test.renderer](test.message, {}, {}).render(width) })) }));
const out = resolve(import.meta.dir, "../../crates/omo/components/maho-omo-task/tests/golden");
mkdirSync(out, { recursive: true });
writeFileSync(join(out, "renderers.json"), JSON.stringify({ source: expected, cases: outputs }, null, 2) + "\n");
console.log(`Generated ${outputs.length * 5} renderer goldens from ${expected}`);
