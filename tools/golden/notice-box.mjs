import { plugin } from "bun";
import { resolve } from "node:path";

const source = process.env.SENPI_SRC;
if (!source) throw new Error("SENPI_SRC is required");
const pin = Bun.spawn(["git", "-C", source, "rev-parse", "HEAD"], { stdout: "pipe" });
const revision = await new Response(pin.stdout).text();
if (await pin.exited !== 0 || revision.trim() !== "fe8c564bf33a2cbbdbbba99c9bd8b45b21e37407") {
  throw new Error("notice fixture source must match the pinned commit");
}
plugin({
  name: "notice-source-tui",
  setup(build) {
    build.onResolve({ filter: /^@earendil-works\/pi-tui$/ }, () => ({
      path: resolve(source, "packages/tui/src/index.ts"),
    }));
  },
});
const { buildNoticeBox } = await import(resolve(source, "packages/coding-agent/src/core/extensions/notice/box.ts"));
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
