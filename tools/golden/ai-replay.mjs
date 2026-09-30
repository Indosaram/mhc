#!/usr/bin/env bun
// Shared SSE/JSON replay harness for maho-ai wire-API parity (todos 10-12).
//
// A case describes a recorded HTTP response body (the kind of fixture senpi's own
// `*-replay*.test.ts`/`*-sse-parsing.test.ts` suites construct inline, e.g. a Chat Completions SSE
// stream, a Responses SSE stream, or an Anthropic Messages SSE stream) plus the model/context/options
// to drive one of senpi's `api/*.ts` `stream()` functions with. The harness:
//
//   1. Serves the fixture bytes verbatim from a local `node:http` server (the same
//      `createServer`/`listen(0, "127.0.0.1")` pattern senpi's own tests use to point a provider's
//      SDK client at a fake endpoint via `Model.baseUrl` - see
//      packages/ai/test/openai-completions-stream-lifecycle.test.ts's `startServer`/`testModel`),
//      or serves only the first `closeAfterBytes` bytes for the mid-stream-close cases.
//   2. Builds a `Model<Api>` for the case's `api` with `baseUrl` pointing at that server.
//   3. Calls the pinned senpi `stream()` export for that api directly (api/openai-completions.ts,
//      api/openai-responses.ts, api/anthropic-messages.ts, ...) and drains the returned
//      `AssistantMessageEventStream`.
//   4. Normalizes and writes the resulting `AssistantMessageEvent[]` sequence as JSON to
//      crates/maho-ai/tests/golden/replay/<case>.json.
//
// Normalization: every event's `timestamp: Date.now()` field (set once by `stream()` when it builds
// the initial `AssistantMessage`, then carried unchanged through `partial`/`message`/`error`) is
// replaced with the fixed sentinel 0 so replays of the same fixture are byte-identical across runs -
// see `normalizeEvent` below. No other field is touched: ids, deltas, usage and content all come
// straight from the fixture bytes, which are already deterministic.
//
// Usage:
//   bun tools/golden/ai-replay.mjs --case <name>   generate one case's golden from tools/golden/cases/<name>.json
//   bun tools/golden/ai-replay.mjs --all           generate every ai-replay-*.json case
//   bun tools/golden/ai-replay.mjs [--case <name> | --all] --check
//                                                  replay in memory and require the on-disk golden to
//                                                  be byte-identical; exit 1 on a mismatch or a missing file
//
// Smoke case and TS-side identity check: `ai-replay-openai-completions-basic` (a two-delta text
// stream) is the smoke case that proves the harness end to end. A replay is byte-identical across
// runs because `normalizeEvent` pins the only nondeterministic field (`timestamp`), so
//
//   bun tools/golden/ai-replay.mjs --all && bun tools/golden/ai-replay.mjs --check --all
//
// regenerates the same bytes on a second run and verifies them byte for byte without touching the
// tree (exit 0 means the identity check holds). Run this after any change to the harness or a case.
//
// Case file shape (tools/golden/cases/ai-replay-<name>.json):
//   {
//     "name": "<name>",                       // must equal the filename stem
//     "kind": "ai-replay",
//     "api": "openai-completions" | "openai-responses" | "anthropic-messages",
//     "model": { "id": "...", "provider": "...", ... },  // merged over API-specific defaults; baseUrl is injected
//     "context": { "messages": [...], "tools"?: [...] }, // senpi Context
//     "options": { ... },                     // senpi StreamOptions for that api (apiKey, maxRetries, ...)
//     "fixture": {
//       "status"?: number,                    // default 200
//       "headers"?: { [name]: string },       // default { "content-type": "text/event-stream" }
//       "events"?: [{ "event"?: string, "data": string }],  // SSE frames, joined as `event: <e>\ndata: <d>\n\n`
//       "body"?: "raw bytes when `events` is not used (e.g. a plain JSON error body)",
//       "closeAfterBytes"?: number             // truncate the body/frames to this many bytes (mid-stream close)
//     }
//   }
//
// Only fixtures recorded from senpi's own test suites (or minimal reproductions of their exact wire
// shapes) belong here - never hand-invented wire payloads. This script never edits golden output by
// hand; there is no update mode. See tools/PARITY-FORMAT.md for the ledger this harness feeds.
import { createServer } from "node:http";
import { mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { pinnedSenpiRoot } from "./pin.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");
const casesDir = join(here, "cases");
const goldenDir = join(repoRoot, "crates", "maho-ai", "tests", "golden", "replay");

const API_MODULES = {
	"openai-completions": "packages/ai/src/api/openai-completions.ts",
	"openai-responses": "packages/ai/src/api/openai-responses.ts",
	"anthropic-messages": "packages/ai/src/api/anthropic-messages.ts",
};

function usage(message) {
	console.error(`ai-replay: ${message}\nusage: bun tools/golden/ai-replay.mjs (--case <name> | --all) [--check]`);
	process.exit(2);
}

function parseArgs(argv) {
	const args = { cases: [], all: false, check: false };
	for (let i = 0; i < argv.length; i++) {
		if (argv[i] === "--case") {
			const name = argv[++i];
			if (!name) usage("--case needs a name");
			args.cases.push(name);
		} else if (argv[i] === "--all") {
			args.all = true;
		} else if (argv[i] === "--check") {
			args.check = true;
		} else {
			usage(`unknown argument ${argv[i]}`);
		}
	}
	if (!args.all && args.cases.length === 0) usage("nothing to do");
	return args;
}

function loadCase(name) {
	const path = join(casesDir, `${name}.json`);
	let spec;
	try {
		spec = JSON.parse(readFileSync(path, "utf8"));
	} catch (error) {
		usage(`cannot read case ${path}: ${error.message}`);
	}
	if (spec.name !== name) usage(`case ${path} declares name ${spec.name}`);
	if (spec.kind !== "ai-replay") usage(`case ${name}: kind must be "ai-replay"`);
	if (!Object.hasOwn(API_MODULES, spec.api)) usage(`case ${name}: unknown api ${spec.api}`);
	return spec;
}

/** Renders `fixture.events`/`fixture.body` into the exact bytes to serve, honoring `closeAfterBytes`. */
function fixtureBytes(fixture) {
	let body;
	if (fixture.events) {
		body = fixture.events.map(({ event, data }) => `${event ? `event: ${event}\n` : ""}data: ${data}\n\n`).join("");
	} else if (typeof fixture.body === "string") {
		body = fixture.body;
	} else {
		usage("fixture needs either events[] or body");
	}
	const bytes = Buffer.from(body, "utf8");
	return fixture.closeAfterBytes === undefined ? bytes : bytes.subarray(0, fixture.closeAfterBytes);
}

/** Starts a local HTTP server that serves `bytes` for every request, then ends the response. */
async function startFixtureServer(fixture) {
	const bytes = fixtureBytes(fixture);
	const server = createServer((_request, response) => {
		response.writeHead(fixture.status ?? 200, fixture.headers ?? { "content-type": "text/event-stream" });
		// `fixtureBytes` already applied `closeAfterBytes`, so a mid-stream close is simply a short
		// body that ends without the fixture's terminator frame - the SSE transport EOF senpi's own
		// openai-completions-stream-lifecycle test produces with `response.end()` after a partial write.
		response.end(bytes);
	});
	await new Promise((resolveListen, reject) => {
		server.once("error", reject);
		server.listen(0, "127.0.0.1", resolveListen);
	});
	const address = server.address();
	if (address === null || typeof address === "string") throw new Error("expected a TCP server address");
	return { server, baseUrl: `http://127.0.0.1:${address.port}/v1` };
}

/** Strips the one nondeterministic field (`Date.now()` timestamps) so replays are byte-identical. */
function normalizeMessage(message) {
	if (message === undefined || message === null) return message;
	return { ...message, timestamp: 0 };
}

function normalizeEvent(event) {
	const normalized = { ...event };
	if ("partial" in normalized) normalized.partial = normalizeMessage(normalized.partial);
	if ("message" in normalized) normalized.message = normalizeMessage(normalized.message);
	if ("error" in normalized && normalized.error && typeof normalized.error === "object" && "role" in normalized.error) {
		normalized.error = normalizeMessage(normalized.error);
	}
	return normalized;
}

/** Drives the case's api `stream()` against the fixture server and returns the normalized events. */
async function runCase(senpi, spec) {
	const { server, baseUrl } = await startFixtureServer(spec.fixture);
	try {
		const mod = await import(pathToFileURL(resolve(senpi, API_MODULES[spec.api])).href);
		const model = { ...spec.model, api: spec.api, baseUrl };
		const eventStream = mod.stream(model, spec.context, spec.options ?? {});
		const events = [];
		for await (const event of eventStream) events.push(normalizeEvent(event));
		return events;
	} finally {
		await new Promise((resolveClose) => server.close(resolveClose));
	}
}

const args = parseArgs(process.argv.slice(2));
const senpi = pinnedSenpiRoot();
const names = args.all
	? readdirSync(casesDir)
			.filter((f) => f.startsWith("ai-replay-") && f.endsWith(".json"))
			.map((f) => f.slice(0, -5))
			.sort()
	: args.cases;

let mismatches = 0;
for (const name of names) {
	const spec = loadCase(name);
	const events = await runCase(senpi, spec);
	const path = join(goldenDir, `${spec.name}.json`);
	const relative = path.slice(repoRoot.length + 1);
	const content = `${JSON.stringify(events, null, "\t")}\n`;
	if (args.check) {
		// The identity check: replaying the same fixture again must reproduce the bytes on disk.
		let existing;
		try {
			existing = readFileSync(path, "utf8");
		} catch (error) {
			console.error(`ai-replay: ${relative} is missing (${error.code}); run without --check to generate it`);
			mismatches += 1;
			continue;
		}
		if (existing !== content) {
			console.error(`ai-replay: ${spec.name} is not byte-identical to a fresh replay of ${relative}`);
			mismatches += 1;
			continue;
		}
		console.log(`ok ${spec.name} (${events.length} events, ${Buffer.byteLength(content)} bytes)`);
		continue;
	}
	mkdirSync(goldenDir, { recursive: true });
	writeFileSync(path, content);
	console.log(`wrote ${relative} (${events.length} events, ${Buffer.byteLength(content)} bytes)`);
}

if (args.check && mismatches > 0) {
	console.error(`ai-replay: ${mismatches} case(s) failed the identity check`);
	process.exit(1);
}
