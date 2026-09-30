#!/usr/bin/env bun
// Headless faux reference: runs a real senpi AgentSession against senpi's faux provider with a
// scripted turn and records the session as golden JSON (events, entries, tool results).
//
//   bun tools/golden/faux-harness.mjs --scenario hello [--omo] --out /tmp/faux-hello.json
//
// Scripts live in tools/golden/scripts/<name>.json ({name, prompt, responses:[{content, stopReason}]})
// and are shared with maho-test-support::faux. `--omo` also loads omo-senpi's extension from
// OMO_SRC (pinned). The run happens under a private temp HOME; timestamps, ids and temp paths are
// normalized so the same scenario always produces the same bytes.
import { writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { isolateHome, loadScript, setupFaux } from "./faux-common.mjs";

function usage(message) {
	console.error(`faux-harness: ${message}\nusage: bun tools/golden/faux-harness.mjs --scenario <name> [--omo] --out <file>`);
	process.exit(2);
}

let scenario;
let out;
let omo = false;
const argv = process.argv.slice(2);
for (let i = 0; i < argv.length; i++) {
	if (argv[i] === "--scenario") scenario = argv[++i];
	else if (argv[i] === "--out") out = argv[++i];
	else if (argv[i] === "--omo") omo = true;
	else usage(`unknown argument ${argv[i]}`);
}
if (!out) usage("--out <file> required");
const script = loadScript(scenario, usage);
out = resolve(out);

const home = await isolateHome();
const faux = await setupFaux(script, { omo });
const { load, registration, model, providerConfig } = faux;
const sdk = await load("packages/coding-agent/src/core/sdk.ts");
const { SessionManager } = await load("packages/coding-agent/src/core/session-manager.ts");
const { SettingsManager } = await load("packages/coding-agent/src/core/settings-manager.ts");
const { AuthStorage } = await load("packages/coding-agent/src/core/auth-storage.ts");
const { ModelRegistry } = await load("packages/coding-agent/src/core/model-registry.ts");
const { DefaultResourceLoader } = await load("packages/coding-agent/src/core/resource-loader.ts");
const { emitSessionShutdownEvent } = await load("packages/coding-agent/src/core/extensions/runner.ts");

const agentDir = process.env.SENPI_CODING_AGENT_DIR;
const authStorage = AuthStorage.inMemory();
const modelRegistry = ModelRegistry.inMemory(authStorage);
modelRegistry.registerProvider(model.provider, providerConfig);
const settingsManager = SettingsManager.inMemory({});
const resourceLoader = new DefaultResourceLoader({ cwd: home, agentDir, settingsManager, extensionFactories: faux.extensions });
await resourceLoader.reload();
const { session, extensionsResult } = await sdk.createAgentSession({
	cwd: home,
	agentDir,
	authStorage,
	modelRegistry,
	model,
	settingsManager,
	sessionManager: SessionManager.inMemory(),
	resourceLoader,
	autoTitleSessions: false,
});
try {
	if (extensionsResult.errors.length > 0) {
		for (const e of extensionsResult.errors) console.error(`faux-harness: extension error: ${e.path ?? ""} ${String(e.error)}`);
		process.exit(1);
	}
	const events = [];
	session.subscribe((event) => events.push(normalize(event)));
	await session.prompt(script.prompt);
	if (registration.getPendingResponseCount() !== 0) {
		console.error(`faux-harness: ${registration.getPendingResponseCount()} scripted responses were not consumed`);
		process.exit(1);
	}
	if (events.length === 0) {
		console.error("faux-harness: session emitted no events");
		process.exit(1);
	}
	const entries = session.sessionManager.getEntries().map(normalize);
	const toolResults = entries.filter((e) => e.type === "message" && e.message?.role === "toolResult");
	const doc = { scenario: script.name, omo, model: `${model.provider}/${model.id}`, events, entries, toolResults };
	writeFileSync(out, `${JSON.stringify(doc, null, 1)}\n`);
	console.log(`faux-harness: ${script.name}${omo ? " (omo)" : ""}: ${events.length} events, ${entries.length} entries -> ${out}`);
} finally {
	// Same teardown as senpi's AgentSessionRuntime.dispose(): extensions get session_shutdown (omo's
	// eager ast-grep MCP server is stopped there) before the session is disposed.
	await emitSessionShutdownEvent(session.extensionRunner, { type: "session_shutdown", reason: "quit" });
	session.dispose();
	registration.unregister();
}

// Replaces run-dependent values: wall-clock fields, generated ids and the temp HOME path.
function normalize(value, key) {
	if (typeof value === "string") {
		if (key !== undefined && /^(id|parentId|sessionId|entryId|responseId|toolCallId|turnKey)$/.test(key)) return "<id>";
		return value.split(home).join("<home>");
	}
	if (typeof value === "number" && key !== undefined && /^(timestamp|createdAt|updatedAt|startedAt|endedAt|durationMs|elapsedMs)$/.test(key)) return 0;
	if (Array.isArray(value)) return value.map((v) => normalize(v));
	if (value && typeof value === "object") {
		const result = {};
		for (const [k, inner] of Object.entries(value)) {
			if (typeof inner === "function") continue;
			result[k] = k === "timestamp" && typeof inner === "string" ? "<time>" : normalize(inner, k);
		}
		return result;
	}
	return value;
}
