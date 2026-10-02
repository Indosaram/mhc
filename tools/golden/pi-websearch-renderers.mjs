import { pathToFileURL } from "node:url";
import { resolve } from "node:path";
const root = process.env.PI_WEBSEARCH_SRC;
if (!root) throw new Error("PI_WEBSEARCH_SRC is required");
const { renderSearchCall, renderSearchResult } = await import(pathToFileURL(resolve(root, "src/websearch/renderers.ts")));
const width = Number(process.argv[2] ?? 80);
const palette = { toolTitle: 6, accent: 6, muted: 7, dim: 8, error: 1, success: 2, warning: 3 };
const theme = { fg: (color, text) => `\x1b[38;5;${palette[color]}m${text}\x1b[39m`, bold: (text) => `\x1b[1m${text}\x1b[22m` };
const result = { provider: "exa", entryId: "primary", query: "native renderer", results: [{ title: "Native result", url: "https://example.org", snippet: "A deterministic snippet" }], durationMs: 1500, truncated: true, strategy: "priority", attempts: [{ provider: "exa", entryId: "primary", durationMs: 1500, resultsCount: 1 }] };
const progress = { phase: "searching", query: "native renderer", providerLabels: ["exa/primary", "tavily"], routeLabels: ["exa/primary", "tavily"], currentProvider: "tavily", maxResults: 5, attempts: [{ provider: "exa", entryId: "primary", durationMs: 10, resultsCount: 0, error: "fixture" }] };
for (const component of [renderSearchCall({ query: "native renderer", allowed_domains: ["example.org"] }, theme), renderSearchResult({ content: [], details: progress }, { expanded: true, isPartial: true }, theme), renderSearchResult({ content: [], details: result }, { expanded: true }, theme)]) {
    for (const line of component.render(width)) console.log(line);
}
