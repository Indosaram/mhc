import { mkdirSync, writeFileSync } from "node:fs";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { pinnedSenpiRoot } from "./pin.mjs";
const root = pinnedSenpiRoot();
const source = "packages/coding-agent/src/modes/interactive/";
const load = (path) => import(pathToFileURL(join(root, path)).href);
const themes = await load(source + "theme/theme.ts");
const { FooterComponent } = await load(source + "components/footer.ts");
const { createFooterSession, createFooterData } = await load("packages/coding-agent/test/helpers/footer-test-fixtures.ts");
const dir = resolve(import.meta.dir, "../../crates/maho-interactive/tests/golden");
mkdirSync(dir, { recursive: true });
const write = (file, data) => { writeFileSync(join(dir, file), data); console.log(`wrote ${file}`); };
const options = { sessionName: "port", modelId: "m:high", provider: "test", cwd: "/tmp/project", usage: { input: 100, output: 10, cacheRead: 50, cacheWrite: 50, cost: { total: 1.234 } } };
const footer = new FooterComponent(createFooterSession(options), createFooterData(2));
for (const name of ["dark", "light", "grok-day", "grok-night"]) {
    themes.initTheme(name, false);
    for (const width of [40, 60, 80, 120, 200]) write(`footer-${name}.${width}.ansi`, footer.render(width).join("\n"));
}
themes.initTheme("dark", false);
const snippets = [
    ["rust", "// hello\nlet value = 42;"],
    ["python", "# hello\nreturn 42"],
    ["json", '{"name": "hello", "count": 42}'],
    [null, "plain text\n"], ["not-a-language", "plain text"],
];
write("highlight.json", JSON.stringify(snippets.map(([language, code]) => ({ language, code, lines: themes.highlightCode(code, language ?? undefined) })), null, 2) + "\n");
const modules = {
    "working-status": [["formatWorkingElapsedSeconds", [-1]], ["formatWorkingElapsedSeconds", [59.9]], ["formatWorkingElapsedSeconds", [60]], ["formatWorkingElapsedSeconds", [3661]], ["formatWorkingStatusMessage", ["Working", 63, "Esc"]], ["formatToolHookStatusMessage", ["PreToolUse", "ready", 12]], ["sanitizeWorkingStatusPlainText", ["\u001b[31m red\u001b[0m\n\t text "]], ["formatActiveToolWorkingLabel", ["bash", { command: "echo hi" }]]],
    "streaming-reveal-pacing": [["nextStep", [0, 16, 90]], ["nextStep", [12.6, 16, 90]], ["nextStep", [1000, 16, 90]], ["nextStep", [1000, 0, 90]], ["updateArrivalRate", [90, 10000, 1]], ["updateArrivalRate", [90, 0, 20]]],
    "version-label": [["formatDisplayVersion", ["1.2.3"]], ["formatDisplayVersion", ["dev"]]],
};
for (const [module, calls] of Object.entries(modules)) {
    const mod = await load(source + module + ".ts");
    write(module + ".json", JSON.stringify(calls.map(([fn, args]) => ({ fn, args, result: mod[fn](...args) })), null, 2) + "\n");
}
