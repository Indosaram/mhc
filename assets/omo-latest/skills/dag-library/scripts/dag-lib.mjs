// dag-lib.mjs: keep workflow graph definitions as named JSON files and start them again by name.
// MIT License (c) 2026 Indo Yoon. See ../LICENSE.
//
// Use from a code-mode JS cell:
//   const lib = await import(SKILL_DIR + "/scripts/dag-lib.mjs");
//   const { runId, reused, warnings } = await lib.start("nightly-check");
//   const done = await lib.waitFor(runId);           // blocks until completed/failed/cancelled
//
// Use from a shell (no run, just files):
//   node dag-lib.mjs load <name> [--suffix S | --no-suffix]
//   node dag-lib.mjs save <name> --scope user|project [--force] < definition.json
//   node dag-lib.mjs where

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { delimiter as hostDelimiter, isAbsolute, join, resolve } from "node:path";

export const ENV_VAR = "MHC_DAG_LIBRARY";
export const LIB_SUBDIR = join(".mhc", "dags");
const NAME_RE = /^[A-Za-z0-9._-]+$/;

/** Throws unless `name` is a plain file stem (letters, digits, ".", "-", "_"; no separators, no ".."). */
export function checkName(name) {
	const s = String(name ?? "");
	if (!s || s.includes("/") || s.includes("\\") || s.includes("..") || !NAME_RE.test(s)) {
		throw new Error(
			`dag-library: invalid definition name ${JSON.stringify(s)}. Use only letters, digits, ".", "-" and "_"; path separators and ".." are not allowed.`,
		);
	}
	return s;
}

/** Ordered list of directories to search: env-var entries, then <cwd>/.mhc/dags, then <home>/.mhc/dags. */
export function searchDirs({ env = process.env, cwd = process.cwd(), home = homedir(), delimiter = hostDelimiter } = {}) {
	const dirs = [];
	const raw = env[ENV_VAR];
	if (raw) {
		for (const part of String(raw).split(delimiter)) {
			const p = part.trim();
			if (p) dirs.push(isAbsolute(p) ? p : resolve(cwd, p));
		}
	}
	dirs.push(join(cwd, LIB_SUBDIR), join(home, LIB_SUBDIR));
	return dirs;
}

/** Returns { path, dir } of the first `<name>.json` found, or throws listing every directory searched. */
export function locate(name, opts = {}) {
	checkName(name);
	const dirs = searchDirs(opts);
	for (const dir of dirs) {
		const path = join(dir, `${name}.json`);
		if (existsSync(path)) return { path, dir };
	}
	throw new Error(`dag-library: no definition named "${name}". Searched:\n${dirs.map((d) => `  - ${d}`).join("\n")}`);
}

const pad = (n) => String(n).padStart(2, "0");
export function utcDate(now = new Date()) {
	return `${now.getUTCFullYear()}${pad(now.getUTCMonth() + 1)}${pad(now.getUTCDate())}`;
}
export function utcStamp(now = new Date()) {
	return `${utcDate(now)}-${pad(now.getUTCHours())}${pad(now.getUTCMinutes())}${pad(now.getUTCSeconds())}`;
}

/**
 * Final key from a stored base key.
 *   suffix undefined/null -> `<base>-<UTC stamp>` (always a fresh run)
 *   suffix ""             -> `<base>` (reuse the previous run on purpose)
 *   suffix "x"            -> `<base>-x` (same suffix = same logical run)
 */
export function rotateKey(base, suffix, now = new Date()) {
	if (suffix === "") return base;
	if (suffix === undefined || suffix === null) return `${base}-${utcStamp(now)}`;
	return `${base}-${suffix}`;
}

/** Deep-replace {{key}}, {{date}}, {{datetime}} in every string value. */
export function fillPlaceholders(value, vars) {
	if (typeof value === "string") return value.replace(/\{\{(key|date|datetime)\}\}/g, (_, k) => vars[k]);
	if (Array.isArray(value)) return value.map((v) => fillPlaceholders(v, vars));
	if (value && typeof value === "object") {
		const out = {};
		for (const [k, v] of Object.entries(value)) out[k] = fillPlaceholders(v, vars);
		return out;
	}
	return value;
}

/** Minimal structural check against the workflow `definition` shape. Returns a list of problems (empty = ok). */
export function validateDefinition(def) {
	const problems = [];
	if (!def || typeof def !== "object" || Array.isArray(def)) return ["definition must be an object"];
	for (const f of ["key", "name"]) if (typeof def[f] !== "string" || !def[f]) problems.push(`"${f}" must be a non-empty string`);
	if (!Array.isArray(def.nodes) || def.nodes.length === 0) {
		problems.push(`"nodes" must be a non-empty array`);
		return problems;
	}
	const ids = new Set();
	def.nodes.forEach((n, i) => {
		const at = `nodes[${i}]`;
		if (!n || typeof n !== "object") return problems.push(`${at} must be an object`);
		if (typeof n.id !== "string" || !n.id) problems.push(`${at}.id is required`);
		else if (ids.has(n.id)) problems.push(`${at}.id "${n.id}" is duplicated`);
		else ids.add(n.id);
		if (typeof n.prompt !== "string" || !n.prompt) problems.push(`${at}.prompt is required`);
		if (n.category && n.subagent_type) problems.push(`${at} sets both category and subagent_type`);
		if (n.model && !n.subagent_type) problems.push(`${at}.model needs subagent_type`);
		if (n.dependsOn !== undefined && !Array.isArray(n.dependsOn)) problems.push(`${at}.dependsOn must be an array`);
	});
	def.nodes.forEach((n, i) => {
		for (const d of n?.dependsOn ?? []) if (!ids.has(d)) problems.push(`nodes[${i}].dependsOn names unknown node "${d}"`);
	});
	return problems;
}

/**
 * Read `<name>.json`, rotate its key, fill placeholders. Does not start anything.
 * opts: { suffix, now, env, cwd, home, delimiter }
 * Returns { definition, path, baseKey, key }.
 */
export function load(name, opts = {}) {
	const { path } = locate(name, opts);
	const stored = JSON.parse(readFileSync(path, "utf8"));
	const problems = validateDefinition(stored);
	if (problems.length) throw new Error(`dag-library: ${path} is not a valid definition:\n  - ${problems.join("\n  - ")}`);
	const now = opts.now ?? new Date();
	const baseKey = stored.key;
	const key = rotateKey(baseKey, opts.suffix, now);
	// Rotate first, then fill, so {{key}} receives the rotated key.
	const definition = fillPlaceholders({ ...stored, key }, { key, date: utcDate(now), datetime: utcStamp(now) });
	return { definition, path, baseKey, key };
}

function defaultCall(args) {
	const t = globalThis.tool;
	if (!t || typeof t.workflow !== "function") {
		throw new Error("dag-library: tool.workflow is not available here; pass opts.call or run inside a code-mode cell.");
	}
	return t.workflow(args);
}

/**
 * Load and start. opts as for load(), plus `call` (async (args) => result) to override tool.workflow.
 * Returns { runId, reused, warnings, key, path, definition, raw }.
 */
export async function start(name, opts = {}) {
	const loaded = load(name, opts);
	const call = opts.call ?? defaultCall;
	const raw = await call({ action: "start", definition: loaded.definition });
	return {
		runId: raw?.run_id,
		reused: Boolean(raw?.reused),
		warnings: raw?.warnings ?? [],
		key: loaded.key,
		path: loaded.path,
		definition: loaded.definition,
		raw,
	};
}

/** Block until the run settles (detach:false). Returns the tool's wait result. */
export async function waitFor(runId, opts = {}) {
	const call = opts.call ?? defaultCall;
	return await call({ action: "wait", run_id: runId, detach: false });
}

/**
 * Write a definition to the user or project library.
 * opts: { scope: "user" | "project", force, cwd, home }
 * Returns { path, scope, overwrote }. Throws if the file exists and force is not set.
 */
export function save(name, definition, opts = {}) {
	checkName(name);
	const problems = validateDefinition(definition);
	if (problems.length) throw new Error(`dag-library: refusing to save an invalid definition:\n  - ${problems.join("\n  - ")}`);
	const scope = opts.scope;
	if (scope !== "user" && scope !== "project") throw new Error('dag-library: scope must be "user" or "project"');
	const root = scope === "user" ? (opts.home ?? homedir()) : (opts.cwd ?? process.cwd());
	const dir = join(root, LIB_SUBDIR);
	const path = join(dir, `${name}.json`);
	const overwrote = existsSync(path);
	if (overwrote && !opts.force) throw new Error(`dag-library: ${path} already exists; pass force to overwrite.`);
	mkdirSync(dir, { recursive: true });
	writeFileSync(path, `${JSON.stringify(definition, null, 2)}\n`);
	return { path, scope, overwrote };
}

// ---- CLI ----
function cli(argv) {
	const [cmd, name, ...rest] = argv;
	const flag = (f) => rest.includes(f);
	const val = (f) => {
		const i = rest.indexOf(f);
		return i >= 0 ? rest[i + 1] : undefined;
	};
	try {
		if (cmd === "where") {
			process.stdout.write(`${searchDirs().join("\n")}\n`);
		} else if (cmd === "load") {
			const suffix = flag("--no-suffix") ? "" : val("--suffix");
			process.stdout.write(`${JSON.stringify(load(name, { suffix }).definition, null, 2)}\n`);
		} else if (cmd === "save") {
			const def = JSON.parse(readFileSync(0, "utf8"));
			const r = save(name, def, { scope: val("--scope"), force: flag("--force") });
			process.stdout.write(`${JSON.stringify(r)}\n`);
		} else {
			process.stderr.write("usage: dag-lib.mjs where | load <name> [--suffix S|--no-suffix] | save <name> --scope user|project [--force]\n");
			return 2;
		}
		return 0;
	} catch (e) {
		process.stderr.write(`${e.message}\n`);
		return 1;
	}
}

const invokedDirectly = (() => {
	try {
		return process.argv[1] && import.meta.url === new URL(`file://${resolve(process.argv[1])}`).href;
	} catch {
		return false;
	}
})();
if (invokedDirectly) process.exitCode = cli(process.argv.slice(2));
