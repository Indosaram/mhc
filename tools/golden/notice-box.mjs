import { plugin } from "bun";
import { existsSync, mkdirSync, renameSync, rmSync } from "node:fs";
import { resolve } from "node:path";

const source = process.env.SENPI_SRC;
if (!source) throw new Error("SENPI_SRC is required");
const PIN = "903b83367545bea516f35fc7f7c7cb692847988d";
const git = async (...args) => {
  const child = Bun.spawn(["git", "-C", source, ...args], { stdout: "pipe", stderr: "pipe" });
  const [out, err] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text()]);
  if (await child.exited !== 0) throw new Error(`git ${args.join(" ")} in ${source}: ${err.trim()}`);
  return out;
};
// A checkout on another commit still renders the pinned bytes: the pinned component files are
// extracted from git under the checkout's node_modules, so their bare imports resolve there.
let root = source;
if ((await git("rev-parse", "HEAD")).trim() !== PIN) {
  root = resolve(source, "node_modules/.cache/mhc-notice-pin", PIN);
  if (!existsSync(resolve(root, "packages/tui/src/index.ts"))) {
    const staging = `${root}.${process.pid}.tmp`;
    rmSync(staging, { recursive: true, force: true });
    mkdirSync(staging, { recursive: true });
    const tar = Bun.spawn(["git", "-C", source, "archive", "--format=tar", PIN, "packages/tui/src", "packages/coding-agent/src/core/extensions/notice"], { stdout: "pipe", stderr: "pipe" });
    const untar = Bun.spawn(["tar", "-x", "-C", staging], { stdin: tar.stdout, stderr: "pipe" });
    if (await tar.exited !== 0 || await untar.exited !== 0) {
      throw new Error(`${source} cannot provide pinned commit ${PIN}: ${(await new Response(tar.stderr).text()).trim()}`);
    }
    rmSync(root, { recursive: true, force: true });
    renameSync(staging, root);
  }
}
plugin({
  name: "notice-source-tui",
  setup(build) {
    build.onResolve({ filter: /^@earendil-works\/pi-tui$/ }, () => ({
      path: resolve(root, "packages/tui/src/index.ts"),
    }));
  },
});
const { buildNoticeBox } = await import(resolve(root, "packages/coding-agent/src/core/extensions/notice/box.ts"));
const cases = [
  ["basic", { title: "Title", why: "Why", extra: [{ text: "Extra", tone: "warning" }], expandedLine: "Detail" }],
  ["empty", { title: "", why: "", extra: [{ text: "" }], expandedLine: "" }],
  ["multiline", { title: "First\nSecond", why: "one\n\nthree\n", extra: [{ text: "\n" }], expandedLine: "\nDetail\n" }],
  ["wrapping", { title: "a long title", why: "alpha beta gamma delta", extra: [{ text: "abcdefghijk" }], expandedLine: "  indented  words  " }],
  ["wide", { title: "한글界", why: "🙂é界 text", extra: [{ text: "👩‍💻 wide" }] }],
  ["nested-ansi", { title: "A\u001b[32mB\u001b[39mC", why: "\u001b[1mbold\u001b[22m plain", extra: [{ text: "\u001b[35mouter \u001b[36minner\u001b[39m end" }] }],
  ["background-reset", { title: "A\u001b[0mB", why: "x\u001b[49my\u001b[44mz", extra: [{ text: "\u001b[48;2;1;2;3mRGB\u001b[0m end" }] }],
  ["tones", { title: "Title", tone: "warning", why: "Why", extra: [{ text: "Muted" }, { text: "Accent", tone: "accent" }], expandedLine: "Detail" }],
];
const results = [];
for (const [name, spec] of cases) {
  for (const width of [0, 1, 2, 3, 5, 12, 40, 80]) {
    for (const expanded of [false, true]) {
      for (const colored of [false, true]) {
        const colors = colored ? { accent: "\u001b[31m", dim: "\u001b[90m", warning: "\u001b[33m" } : {};
        const background = colored ? "\u001b[44m" : "\u001b[49m";
        const theme = {
          fg: (tone, text) => `${colors[tone] ?? "\u001b[39m"}${text}\u001b[39m`,
          bg: (_, text) => `${background}${text}\u001b[49m`,
        };
        results.push({ name, spec, width, expanded, colors, background, lines: buildNoticeBox(spec, { expanded }, theme).render(width) });
      }
    }
  }
}
console.log(JSON.stringify(results));
