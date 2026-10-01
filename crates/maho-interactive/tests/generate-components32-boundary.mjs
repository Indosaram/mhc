import { pinnedSenpiRoot } from "../../../tools/golden/pin.mjs";
import { pathToFileURL } from "node:url";
import { writeFileSync } from "node:fs";

const root = pinnedSenpiRoot();
const { default: chalk } = await import(pathToFileURL(root + "/node_modules/chalk/source/index.js").href);
chalk.level = 1;
const interactive = root + "/packages/coding-agent/src/modes/interactive/";
const load = (base, name) => import(pathToFileURL(base + name + ".ts").href);

const { initTheme } = await load(interactive, "theme/theme");
const { ToolRendererBoundary } = await load(interactive, "components/tool-renderer-boundary");
const { ProgressiveTranscriptContainer } = await load(interactive, "components/progressive-transcript-container");
const { Text } = await import(pathToFileURL(root + "/packages/tui/src/components/text.ts").href);

initTheme("dark", false);
const trim = (lines) => lines.map((line) => line.replace(/\s+$/, ""));

const boundaryCases = [];
for (const width of [40, 80]) {
  let failures = 0;
  const healthy = new ToolRendererBoundary(new Text("healthy", 0, 0), undefined, () => {
    failures += 1;
  });
  boundaryCases.push({ name: "healthy", width, lines: trim(healthy.render(width)), failures });

  const throwing = {
    render() {
      throw new Error("boom");
    },
    invalidate() {},
  };
  let throwingFailures = 0;
  const boundary = new ToolRendererBoundary(throwing, new Text("fallback", 0, 0), () => {
    throwingFailures += 1;
  });
  const lines = trim(boundary.render(width));
  boundaryCases.push({ name: "throwing", width, lines, failures: throwingFailures });

  const second = trim(boundary.render(width));
  boundaryCases.push({ name: "throwing-again", width, lines: second, failures: throwingFailures });
}

const progressiveCases = [];
const build = (count) => {
  const container = new ProgressiveTranscriptContainer({ tailBudget: 2, warmChunkSize: 1, requestRender: () => {} });
  for (let index = 0; index < count; index++) container.addChild(new Text(`child-${index}`, 0, 0));
  return container;
};
for (const count of [0, 1, 2, 5]) {
  const container = build(count);
  const frames = [];
  for (let frame = 0; frame < 4; frame++) frames.push(trim(container.render(40)));
  progressiveCases.push({ count, tailBudget: 2, warmChunkSize: 1, frames });
}

writeFileSync(
  import.meta.dir + "/golden/components32-boundary.json",
  JSON.stringify({ boundary: boundaryCases, progressive: progressiveCases }, null, 2) + "\n",
);
console.log(`Generated ${boundaryCases.length} boundary and ${progressiveCases.length} progressive cases`);
