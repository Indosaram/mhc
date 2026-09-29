// Shared plumbing for the faux reference tools (faux-harness.mjs, faux-tui.mjs).
//
// Both tools register senpi's faux provider (packages/ai/src/providers/faux.ts) with the scripted
// responses in tools/golden/scripts/<name>.json and then drive real senpi code from the pinned
// SENPI_SRC checkout. `--omo` additionally loads omo-senpi's extension from OMO_SRC.
import { spawn, spawnSync } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { constants } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { pinnedSenpiRoot } from "./pin.mjs";

const here = dirname(fileURLToPath(import.meta.url));

export const OMO_PIN = "77f3067f157a4f88e6d8ed48b3a6c338654402ed";

/** Marker env set on the re-executed child so it knows HOME is already a private temp dir. */
const CHILD_MARKER = "MAHO_FAUX_ISOLATED";

/**
 * The isolated child's env is built from scratch (allowlist, not denylist): anything inherited
 * could point senpi/omo at the real agent dirs, a live omo bridge or session, real credentials or
 * telemetry, or leak into the system prompt (senpi's workstation block renders TERM_PROGRAM/TERM).
 * Only these are passed through: PATH to find git/bun, the SENPI_SRC/OMO_SRC pin overrides, and,
 * for the interactive launcher only, the terminal capability vars the real TUI needs.
 */
const PASSED_ENV = ["PATH", "SENPI_SRC", "OMO_SRC"];
const TERMINAL_ENV = ["TERM", "COLORTERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION", "LANG", "LC_ALL", "LC_CTYPE"];

/** Builds the isolated child's env: allowlisted inherited vars plus fixed temp dirs and opt-outs. */
export function isolatedEnv(home, { terminal = false, env = process.env } = {}) {
	const result = {};
	for (const key of terminal ? [...PASSED_ENV, ...TERMINAL_ENV] : PASSED_ENV) {
		if (env[key] !== undefined) result[key] = env[key];
	}
	const agentDir = join(home, "agent");
	return {
		...result,
		[CHILD_MARKER]: "1",
		HOME: home,
		TMPDIR: join(home, "tmp"),
		SHELL: "/bin/bash",
		TZ: "UTC",
		SENPI_CODING_AGENT_DIR: agentDir,
		PI_CODING_AGENT_DIR: agentDir,
		OMO_CODING_AGENT_DIR: agentDir,
		// Every omo telemetry switch (docs/reference/senpi-telemetry.md opt-out matrix) plus senpi's.
		DO_NOT_TRACK: "1",
		OMO_DISABLE_POSTHOG: "1",
		OMO_SENPI_DISABLE_POSTHOG: "1",
		SENPI_TELEMETRY: "0",
		PI_TELEMETRY: "0",
		SENPI_OFFLINE: "1",
		PI_OFFLINE: "1",
		SENPI_SKIP_VERSION_CHECK: "1",
		PI_SKIP_VERSION_CHECK: "1",
	};
}

const HOME_PARENT = "/tmp";
const HOME_PREFIX = "maho-faux-";
const OWNER_FILE = "owner.pid";

function isAlive(pid) {
	try {
		process.kill(pid, 0);
		return true;
	} catch (error) {
		return error.code === "EPERM";
	}
}

/** Removes temp homes left by a launcher that was SIGKILLed (its owner.pid is no longer alive). */
function sweepStaleHomes() {
	for (const entry of readdirSync(HOME_PARENT)) {
		if (!entry.startsWith(HOME_PREFIX)) continue;
		const dir = join(HOME_PARENT, entry);
		let owner;
		try {
			owner = Number(readFileSync(join(dir, OWNER_FILE), "utf8"));
		} catch {
			continue; // not ours, or still being created
		}
		if (Number.isInteger(owner) && owner > 0 && !isAlive(owner)) rmSync(dir, { recursive: true, force: true });
	}
}

/**
 * Re-runs the current script under a fresh temp HOME and agent dir, then deletes it.
 * The launcher forwards SIGTERM/SIGHUP to the child and removes the home after the child exits;
 * a launcher that is SIGKILLed leaves its home behind, which the next launch sweeps (owner.pid).
 * SIGINT is left to the child (it shares the process group, so it already received it).
 * Bun's os.homedir() is fixed at process start, so isolation needs a new process; this keeps
 * senpi and omo from reading or writing the real ~/.senpi, ~/.omo or auth.json, and the env is
 * rebuilt by isolatedEnv() so no inherited agent dir, bridge, session or key reaches the child.
 * `terminal: true` (faux-tui) also passes the terminal capability vars through.
 * Returns only inside the isolated child.
 */
export async function isolateHome({ terminal = false } = {}) {
	if (process.env[CHILD_MARKER] === "1") return process.env.HOME;
	sweepStaleHomes();
	// A fixed parent (not the inherited TMPDIR) keeps the home path, which senpi renders into the
	// system prompt's cwd line and so into token usage, the same length on every host and env.
	const home = realpathSync(mkdtempSync(join(HOME_PARENT, HOME_PREFIX)));
	writeFileSync(join(home, OWNER_FILE), String(process.pid));
	mkdirSync(join(home, "tmp"));
	// The interactive launcher starts in the temp home, so senpi's cwd (system prompt, project
	// AGENTS.md/context discovery, footer path) never points at the caller's directory. The harness
	// keeps the caller's cwd because it resolves --out there; it passes cwd: home to senpi itself.
	const child = spawn(process.execPath, process.argv.slice(1), {
		cwd: terminal ? home : process.cwd(),
		stdio: "inherit",
		env: isolatedEnv(home, { terminal }),
	});
	const forward = (signal) => child.kill(signal);
	const ignore = () => {};
	process.on("SIGTERM", forward);
	process.on("SIGHUP", forward);
	process.on("SIGINT", ignore);
	const { promise, resolve: done } = Promise.withResolvers();
	child.on("error", (error) => done({ error }));
	child.on("exit", (code, signal) => done({ code, signal }));
	const result = await promise;
	rmSync(home, { recursive: true, force: true });
	if (result.error) {
		console.error(`faux: cannot re-run under an isolated HOME: ${result.error.message}`);
		process.exit(1);
	}
	process.exit(result.code ?? 128 + (constants.signals[result.signal] ?? 0));
}

/**
 * omo-senpi resolves three runtime pieces from its packaged plugin layout, which the pinned source
 * checkout does not contain (they are gitignored build outputs): the agent-toolkit binary (ulw-loop),
 * the staged ast-grep MCP entry, and the lsp-daemon CLI. Without them omo logs "omo binary not
 * found" / "staged MCP runtime is missing" at startup, which is environment noise, not omo UI.
 * Point the first two at the pinned sources through shims inside the temp HOME (never ~/.omo and
 * never a write to OMO_SRC). The lsp-daemon is resolved lazily by the lsp tools and the process
 * sweep, logs nothing at startup, and is not exercised by the faux scripts, so it is left unset.
 */
function provisionOmoRuntime(omoRoot, home) {
	// omo's first-run onboarding (components/onboarding) queues a hidden follow-up turn on the first
	// startup of a fresh agent home, which would consume a scripted response or fail with "No more
	// faux responses queued". Goldens show the steady-state UI, so pre-write omo's own marker in the
	// temp agent dir (getOmoNativeStateDir -> <agentDir>/omo-senpi/omo-native/onboarding-completed).
	const stateDir = join(process.env.OMO_CODING_AGENT_DIR, "omo-senpi", "omo-native");
	mkdirSync(stateDir, { recursive: true });
	writeFileSync(join(stateDir, "onboarding-completed"), JSON.stringify({ completedAt: "1970-01-01T00:00:00.000Z", version: 1 }));
	const bin = join(home, "omo-runtime");
	mkdirSync(bin, { recursive: true });
	const toolkit = join(bin, "omo-agent-toolkit");
	const toolkitSrc = join(omoRoot, "packages/omo-codex/plugin/components/ulw-loop/src/cli.ts");
	writeFileSync(toolkit, `#!/bin/sh\nexec ${JSON.stringify(process.execPath)} ${JSON.stringify(toolkitSrc)} "$@"\n`);
	chmodSync(toolkit, 0o755);
	process.env.OMO_AGENT_TOOLKIT_BIN = toolkit;
	const astGrep = join(bin, "ast-grep-mcp.js");
	writeFileSync(astGrep, `import ${JSON.stringify(pathToFileURL(join(omoRoot, "packages/ast-grep-mcp/src/cli.ts")).href)};\n`);
	return { astGrepEntry: astGrep };
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
		const { astGrepEntry } = provisionOmoRuntime(omoRoot, process.env.HOME);
		const src = (rel) => import(pathToFileURL(join(omoRoot, "packages/omo-senpi/src", rel)).href);
		const [ext, list, task, astGrep] = await Promise.all([
			src("extension/compose.ts"),
			src("extension/component-list.ts"),
			src("components/task/index.ts"),
			src("components/ast-grep/index.ts"),
		]);
		// Same component list and order as omo-senpi's default export (extension/index.ts); only the ast-grep entry is
		// injected through that component's own resolveEntry option.
		const components = list
			.createOmoSenpiComponents(task.createTaskComponent())
			.map((c) => (c.name === "ast-grep" ? astGrep.createAstGrepComponent({ resolveEntry: () => astGrepEntry }) : c));
		extensions.push({ name: "omo-senpi", factory: ext.composeOmoSenpiExtension(components) });
	}
	return { senpi, load, registration, model, providerConfig, fauxExtension, extensions };
}
