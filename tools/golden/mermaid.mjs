import { execFileSync } from "node:child_process";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { pinnedSenpiRoot, SENPI_PIN } from "./pin.mjs";

const root = pinnedSenpiRoot();
const version = execFileSync(
	"node",
	["-p", "require('./packages/coding-agent/package.json').dependencies['grok-mermaid']"],
	{ cwd: root, encoding: "utf8" },
).trim();
if (version !== "0.2.3") throw new Error(`expected grok-mermaid 0.2.3, found ${version}`);

const modulePath = execFileSync(
	"node",
	["-p", "require.resolve('grok-mermaid', { paths: ['./packages/coding-agent'] })"],
	{ cwd: root, encoding: "utf8" },
).trim();
const { render } = await import(pathToFileURL(modulePath).href);
const inputs = [
	{
		name: "flowchart-lr-self-edge-long-label",
		source: "flowchart LR\n  A[Start] --> B[An unusually long descriptive label]\n  B --> B",
	},
	{
		name: "grouped-graph",
		source: "flowchart LR\n  subgraph Frontend\n    A[Browser] --> B[UI]\n  end\n  subgraph Backend\n    C[API] --> D[Database]\n  end\n  B --> C",
	},
	{
		name: "td-diamond",
		source: "flowchart TD\n  A[Request] --> B{Authorized?}\n  B -->|yes| C[Proceed]\n  B -->|no| D[Reject]",
	},
	{
		name: "sequence",
		source: "sequenceDiagram\n  Alice->>Bob: Send request\n  Bob-->>Alice: Return response",
	},
	{
		name: "unicode-label",
		source: "flowchart LR\n  A[こんにちは] --> B[مرحبا]",
	},
	{
		name: "partial-warning",
		source: 'flowchart LR\n  A[Foo] invalid\n  B[Bar] also-invalid',
	},
];
const cases = inputs.map(({ name, source }) => ({ name, source, result: render(source) ?? null }));
const fixture = { provenance: { senpiCommit: SENPI_PIN, package: "grok-mermaid", version }, cases };
const target = resolve(import.meta.dir, "../../crates/maho-interactive/tests/golden/mermaid-hg.json");
const content = JSON.stringify(fixture, null, 2) + "\n";
const quote = (value) => value.split("\n").map((line) => `+${line}`).join("\n");
console.log(`*** Begin Patch\n*** Add File: ${target}\n${quote(content)}\n*** End Patch`);
