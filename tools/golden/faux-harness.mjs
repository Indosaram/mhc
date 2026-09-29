#!/usr/bin/env bun
// Drives senpi's faux provider with a scripted turn and records the streamed events as JSON.
//   bun tools/golden/faux-harness.mjs --scenario hello --out /tmp/faux-hello.json
// Scripts live in tools/golden/scripts/<name>.json and are shared with maho-test-support::faux.
// Timestamps and generated ids are normalized so the output is deterministic.
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { pinnedSenpiRoot } from "./pin.mjs";

const here = dirname(fileURLToPath(import.meta.url));

function usage(message) {
	console.error(`faux-harness: ${message}\nusage: bun tools/golden/faux-harness.mjs --scenario <name> --out <file>`);
	process.exit(2);
}

let scenario;
let out;
const argv = process.argv.slice(2);
for (let i = 0; i < argv.length; i++) {
	if (argv[i] === "--scenario") scenario = argv[++i];
	else if (argv[i] === "--out") out = argv[++i];
	else usage(`unknown argument ${argv[i]}`);
}
if (!scenario || !/^[a-z0-9][a-z0-9-]*$/.test(scenario)) usage("--scenario <name> required");
if (!out) usage("--out <file> required");

let script;
try {
	script = JSON.parse(readFileSync(join(here, "scripts", `${scenario}.json`), "utf8"));
} catch (error) {
	usage(`cannot read scenario ${scenario}: ${error.message}`);
}

const senpi = pinnedSenpiRoot();
const ai = await import(pathToFileURL(resolve(senpi, "packages/ai/src/compat.ts")).href);
const faux = await import(pathToFileURL(resolve(senpi, "packages/ai/src/providers/faux.ts")).href);

const registration = ai.registerFauxProvider();
try {
	registration.setResponses(
		script.responses.map((r) => faux.fauxAssistantMessage(r.content, { stopReason: r.stopReason ?? "stop", timestamp: 0 })),
	);
	const model = registration.models[0];
	const context = { messages: [{ role: "user", content: script.prompt, timestamp: 0 }] };
	const events = [];
	for (let turn = 0; turn < script.responses.length; turn++) {
		for await (const event of ai.streamSimple(model, context)) events.push(normalize(event));
	}
	if (events.length === 0) {
		console.error("faux-harness: provider streamed no events");
		process.exit(1);
	}
	writeFileSync(out, `${JSON.stringify({ scenario, model: model.id, events }, null, 1)}\n`);
	console.log(`faux-harness: ${scenario}: ${events.length} events -> ${out}`);
} finally {
	registration.unregister();
}

function normalize(value) {
	if (Array.isArray(value)) return value.map(normalize);
	if (value && typeof value === "object") {
		const result = {};
		for (const [key, inner] of Object.entries(value)) {
			if (key === "timestamp") result[key] = 0;
			else if (key === "id" && typeof inner === "string") result[key] = "<id>";
			else result[key] = normalize(inner);
		}
		return result;
	}
	return value;
}
