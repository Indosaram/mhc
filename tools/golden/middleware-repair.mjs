#!/usr/bin/env bun
// Middleware repair golden. Runs the pinned senpi tool-call middleware repair path on malformed
// tool-call JSON and records the normalized result. Fixtures are only ever produced here; the Rust
// side never regenerates or edits them.
//
//   bun tools/golden/middleware-repair.mjs --write   write crates/maho-ai/tests/golden/middleware-repair.json
//   bun tools/golden/middleware-repair.mjs           print the normalized JSON to stdout
//
// Tools are built with TypeBox, exactly as senpi's own tools are (packages/coding-agent/src/core/
// tools/*.ts): senpi's `IsObject`/`IsArray` guards read TypeBox's non-enumerable `~kind` marker, so
// a plain JSON schema without it takes a different code path. The Rust port has no such marker and
// emulates the TypeBox runtime by reading `type`, which is why the recorded tool JSON (marker-free)
// is what the Rust test feeds back in.
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { pinnedSenpiRoot } from "./pin.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");
const senpi = pinnedSenpiRoot();

async function importSenpi(relativePath) {
	const absolute = resolve(senpi, relativePath);
	if (!absolute.startsWith(senpi + "/")) throw new Error(`module ${relativePath} escapes SENPI_SRC`);
	return import(pathToFileURL(absolute).href);
}

const { Type } = await import(pathToFileURL(join(senpi, "node_modules/typebox/build/index.mjs")).href);
const repair = await importSenpi("packages/ai/src/tool-call-middleware/protocols/antml/repair.ts");
const antmlParse = await importSenpi("packages/ai/src/tool-call-middleware/protocols/antml/parse.ts");
const anthropicXmlParse = await importSenpi("packages/ai/src/tool-call-middleware/protocols/anthropic-xml/parse.ts");
const kimiParse = await importSenpi("packages/ai/src/tool-call-middleware/protocols/kimi-xtml/parse.ts");

// Tool schemas mirror senpi's own tool definitions and the shapes the TS middleware tests use.
const bashTool = { name: "Bash", description: "Run a shell command", parameters: Type.Object({ command: Type.String() }) };
const weatherTool = { name: "get_weather", description: "weather", parameters: Type.Object({ city: Type.String() }) };
const configTool = {
	name: "config_tool",
	description: "Writes a nested options object",
	parameters: Type.Object({ options: Type.Object({ text: Type.String() }) }),
};
const editTool = {
	name: "Edit",
	description: "Edit a file",
	parameters: Type.Object({ file_path: Type.String(), edits: Type.Array(Type.Object({ oldText: Type.String(), newText: Type.String() })) }),
};
const countTool = {
	name: "count_tool",
	description: "Coerces tolerant scalar spellings",
	parameters: Type.Object({ enabled: Type.Boolean(), count: Type.Number(), index: Type.Integer() }),
};

// Numeric parameters: senpi's `Number(...)` coercion yields an integral JS number for `"3"`, so the
// JSON argument must serialize as `3`, not `3.0`.
const anthropicXmlCalls = [
	{
		label: "coerces-an-integral-number-parameter",
		text: '<invoke name="count_tool"><parameter name="enabled">true</parameter><parameter name="count">3</parameter><parameter name="index">-2</parameter></invoke>',
		tools: [countTool],
	},
	{
		label: "keeps-a-fractional-number-parameter",
		text: '<invoke name="count_tool"><parameter name="count">1.25</parameter></invoke>',
		tools: [countTool],
	},
];

const kimiCalls = [
	{
		label: "coerces-an-integral-number-argument",
		text: '<|open|>tools<|sep|><|open|>call tool="count_tool" index="1"<|sep|><|open|>argument key="count" type="number"<|sep|>3<|close|>argument<|sep|><|close|>call<|sep|><|close|>tools<|sep|>',
		tools: [countTool],
	},
];

function tryParse(json) {
	try {
		return { ok: true, value: JSON.parse(json) };
	} catch {
		return { ok: false };
	}
}

// Malformed `\u` escapes: the repair turns a `\u` that cannot start a JSON escape into a literal
// `\\u` so the surrounding JSON parses. Inputs and outputs are pure ASCII, so the recorded JSON is
// lossless. Lone-surrogate repair is not recorded directly: a Rust `String` cannot hold an unpaired
// UTF-16 surrogate, so the only lossless way to exercise it is through a tool call whose raw text
// carries the `\uD800` escape literally (see `repairs-lone-surrogates-inside-json-arguments`).
const unicodeEscapeInputs = [
	String.raw`{"text":"bad\uZZZZescape"}`,
	String.raw`{"text":"cut\u12"}`,
	String.raw`{"text":"ok\u0041"}`,
	String.raw`{"path":"C:\\users"}`,
];

// Malformed tool-call JSON inside an antml `<invoke>`: the middleware repairs what it can and
// reports what it cannot through `onError`.
const malformedCalls = [
	{
		label: "repairs-a-broken-unicode-escape-inside-json-arguments",
		text: String.raw`<invoke name="config_tool"><parameter name="options">{"text":"bad\uZZZZescape"}</parameter></invoke>`,
		tools: [configTool],
	},
	{
		label: "repairs-the-pi-edits-invented-trailing-keys",
		text: `<invoke name="Edit"><parameter name="file_path">some/file.py</parameter><parameter name="edits">[{"oldText":"a","newText":"b","requireUnique":true}]</parameter></invoke>`,
		tools: [editTool],
	},
	{
		label: "repairs-a-broken-unicode-escape-in-a-plain-string",
		text: String.raw`<invoke name="Bash"><parameter name="command">echo \uZZZZ</parameter></invoke>`,
		tools: [bashTool],
	},
	{
		label: "repairs-lone-surrogates-inside-json-arguments",
		text: String.raw`<invoke name="config_tool"><parameter name="options">{"text":"a\uD800b"}</parameter></invoke>`,
		tools: [configTool],
	},
	{
		label: "coerces-tolerant-scalar-spellings",
		text: `<invoke name="count_tool"><parameter name="enabled"> TRUE </parameter><parameter name="count"> 1.25 </parameter><parameter name="index">"2"</parameter></invoke>`,
		tools: [countTool],
	},
	{
		label: "keeps-the-last-value-when-a-parameter-repeats",
		text: `<invoke name="get_weather"><parameter name="city">first</parameter><parameter name="city">second</parameter></invoke>`,
		tools: [weatherTool],
	},
	{
		label: "rejects-unparseable-json-arguments",
		text: `<invoke name="config_tool"><parameter name="options">{bad</parameter></invoke>`,
		tools: [configTool],
	},
	{
		label: "rejects-a-missing-required-parameter",
		text: `<invoke name="get_weather"><parameter name="country">KR</parameter></invoke>`,
		tools: [weatherTool],
	},
	{
		label: "rejects-an-unknown-tool",
		text: `<invoke name="Missing"><parameter name="command">x</parameter></invoke>`,
		tools: [bashTool],
	},
];

const golden = {
	generator: "tools/golden/middleware-repair.mjs",
	senpiPin: senpi,
	repairUnicodeEscapes: unicodeEscapeInputs.map((input) => {
		const repaired = repair.repairUnicodeEscapes(input);
		return { input, repaired, parsed: tryParse(repaired) };
	}),
	antmlParse: malformedCalls.map((entry) => {
		const errors = [];
		const calls = antmlParse.parseAntmlGeneratedText(entry.text, entry.tools, {
			onError: (message, metadata) => errors.push({ message, metadata: metadata ?? null }),
		});
		return {
			label: entry.label,
			text: entry.text,
			tools: entry.tools.map((tool) => ({ name: tool.name, description: tool.description, parameters: tool.parameters })),
			calls,
			errors,
		};
	}),
	anthropicXmlParse: anthropicXmlCalls.map((entry) => ({
		label: entry.label,
		text: entry.text,
		tools: entry.tools.map((tool) => ({ name: tool.name, description: tool.description, parameters: tool.parameters })),
		calls: anthropicXmlParse.parseAnthropicXmlGeneratedText(entry.text, entry.tools),
	})),
	kimiXtmlParse: kimiCalls.map((entry) => ({
		label: entry.label,
		text: entry.text,
		tools: entry.tools.map((tool) => ({ name: tool.name, description: tool.description, parameters: tool.parameters })),
		calls: kimiParse.parseKimiXtmlGeneratedText(entry.text, entry.tools),
	})),
};

const serialized = `${JSON.stringify(golden, null, 2)}\n`;
const target = join(repoRoot, "crates", "maho-ai", "tests", "golden", "middleware-repair.json");
if (process.argv.includes("--write")) {
	mkdirSync(dirname(target), { recursive: true });
	writeFileSync(target, serialized);
	console.log(`wrote ${target}`);
} else {
	process.stdout.write(serialized);
}
