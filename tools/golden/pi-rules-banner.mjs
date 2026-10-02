import { pathToFileURL } from "node:url";
import { resolve } from "node:path";
const root = process.env.PI_RULES_SRC;
if (!root) throw new Error("PI_RULES_SRC is required");
const { renderBannerLines, statusLineText } = await import(pathToFileURL(resolve(root, "src/ui/rules-banner.ts")));
const width = Number(process.argv[2] ?? 80);
const palette = { border: 8, accent: 6, muted: 7, error: 1, success: 2, warning: 3 };
const theme = { fg: (color, text) => `\x1b[38;5;${palette[color]}m${text}\x1b[39m`, bold: (text) => `\x1b[1m${text}\x1b[22m` };
for (const props of [{ ruleCount: 0, diagnostics: [] }, { ruleCount: 2, diagnostics: [{ source: "rules.md", severity: "warning", message: "fixture" }], topRules: [{ relativePath: "AGENTS.md", matchReason: "single-file" }, { relativePath: "rules.md", matchReason: { kind: "glob", pattern: "src/**" } }] }]) {
    for (const line of renderBannerLines(props, theme, width)) console.log(line);
}
console.log(statusLineText({ ruleCount: 2, hasErrors: true }, theme));
