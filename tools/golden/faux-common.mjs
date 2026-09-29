// Shared plumbing for the faux reference tools (faux-harness.mjs, faux-tui.mjs).
//
// Both tools register senpi's faux provider (packages/ai/src/providers/faux.ts) with the scripted
// responses in tools/golden/scripts/<name>.json and then drive real senpi code from the pinned
// SENPI_SRC checkout. `--omo` additionally loads omo-senpi's extension from OMO_SRC.
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { pinnedSenpiRoot } from "./pin.mjs";

const here = dirname(fileURLToPath(import.meta.url));

export const OMO_PIN = "77f3067f157a4f88e6d8ed48b3a6c338654402ed";

/** Marker env set on the re-executed child so it knows HOME is already a private temp dir. */
const CHILD_MARKER = "MAHO_FAUX_ISOLATED";

/**
 * Re-runs the current script under a fresh temp HOME and agent dir, then deletes it.
 * Bun's os.homedir() is fixed at process start, so isolation needs a new process; this keeps
 * senpi and omo from reading or writing the real ~/.senpi, ~/.omo or auth.json.
 * Returns only inside the isolated child.
 */
export function isolateHome() {
	if (process.env[CHILD_MARKER] === "1") return process.env.HOME;
	const home = mkdtempSync(join(tmpdir(), "maho-faux-"));
	const result = spawnSync(process.execPath, process.argv.slice(1), {
		stdio: "inherit",
		env: {
			...process.env,
			[CHILD_MARKER]: "1",
			HOME: home,
			SENPI_CODING_AGENT_DIR: join(home, "agent"),
			TZ: "UTC",
		},
	});
	rmSync(home, { recursive: true, force: true });
	if (result.error) {
		console.error(`faux: cannot re-run under an isolated HOME: ${result.error.message}`);
		process.exit(1);
	}
	process.exit(result.status ?? 1);
}

export function loadScript(name, usage) {
	if (!name || !/^[a-z0-9][a-z0-9-]*$/.test(name)) usage("script name must match [a-z0-9][a-z0-9-]*");
	try {
		return JSON.parse(readFileSync(join(here, "scripts", `${name}.json`), "utf8"));
	} catch (error) {
		usage(`cannot read script ${name}: ${error.message}`);
	}
}

/** Returns the OMO_SRC checkout (default /Users/indo/code/oh-my-openagent), refusing any other HEAD. */
function pinnedOmoRoot() {
	const root = resolve(process.env.OMO_SRC ?? "/Users/indo/code/oh-my-openagent");
	const head = spawnSync("git", ["-C", root, "rev-parse", "HEAD"], { encoding: "utf8" });
	if (head.status !== 0) {
		console.error(`omo: ${root} is not a git checkout`);
		process.exit(3);
	}
	if (head.stdout.trim() !== OMO_PIN) {
		console.error(`omo: ${root} is at ${head.stdout.trim()}, expected ${OMO_PIN}; refusing to run`);
		process.exit(3);
	}
	return root;
}

/**
 * Imports senpi from the pinned checkout and registers faux with the script's responses.
 * A fixed api id and token size remove faux.ts's random inputs so runs stream identical chunks.
 */
export async function setupFaux(script, { omo }) {
	const senpi = pinnedSenpiRoot();
	const load = (rel) => import(pathToFileURL(resolve(senpi, rel)).href);
	const ai = await load("packages/ai/src/compat.ts");
	const faux = await load("packages/ai/src/providers/faux.ts");
	const registration = ai.registerFauxProvider({ api: "faux-golden", tokenSize: { min: 3, max: 3 } });
	registration.setResponses(
		script.responses.map((r) => faux.fauxAssistantMessage(r.content, { stopReason: r.stopReason ?? "stop", timestamp: 0 })),
	);
	const model = registration.getModel();
	// Same provider registration senpi's own test harness performs, done through the public
	// extension API so the interactive mode resolves `--provider faux --model faux-1`.
	const providerConfig = {
		baseUrl: model.baseUrl,
		apiKey: "faux-key",
		api: registration.api,
		models: registration.models.map((m) => ({
			id: m.id,
			name: m.name,
			api: m.api,
			reasoning: m.reasoning,
			input: m.input,
			cost: m.cost,
			contextWindow: m.contextWindow,
			maxTokens: m.maxTokens,
		})),
	};
	const fauxExtension = { name: "faux", hidden: true, factory: (pi) => pi.registerProvider(model.provider, providerConfig) };
	const extensions = [];
	if (omo) {
		const omoRoot = pinnedOmoRoot();
		const mod = await import(pathToFileURL(join(omoRoot, "packages/omo-senpi/src/extension/index.ts")).href);
		extensions.push({ name: "omo-senpi", factory: mod.default });
	}
	return { senpi, load, registration, model, providerConfig, fauxExtension, extensions };
}
