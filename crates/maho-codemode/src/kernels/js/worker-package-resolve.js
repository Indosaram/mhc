import { readFileSync, realpathSync, statSync } from "node:fs";
import { isBuiltin } from "node:module";
import { dirname, join, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

// A cell's bare import runs `import()` from this worker's own module, so the runtime resolves it from Senpi's install
// tree, not the session's project or its managed package revision. This resolver walks `node_modules` from each root
// the cell names, in order, and resolves the package entry the way ESM does (the `exports` field with import
// conditions), so ESM-only packages load and the first root that has the package wins.
const RESOLVE_PACKAGE = Symbol.for("senpi.kernel.resolvePackage");
const CONDITIONS = typeof globalThis.Bun === "object" ? ["bun", "node", "import", "default"] : ["node", "import", "default"];

function isFile(path) {
…
}

function isDirectory(path) {
…
}

function splitSpecifier(specifier) {
	const parts = specifier.split("/");
	const nameParts = specifier.startsWith("@") ? 2 : 1;
	if (parts.length < nameParts || parts.slice(0, nameParts).some((part) => part === "" || part === "." || part === ".."))
		return undefined;
	const rest = parts.slice(nameParts).join("/");
	return { name: parts.slice(0, nameParts).join("/"), subpath: rest === "" ? "." : `./${rest}` };
}

function conditionalTarget(target) {
	if (typeof target === "string") return target;
	if (Array.isArray(target)) {
…
	}
	if (target === null || typeof target !== "object") return undefined;
	for (const [condition, value] of Object.entries(target)) {
		if (condition !== "default" && !CONDITIONS.includes(condition)) continue;
		const resolved = conditionalTarget(value);
		if (resolved !== undefined) return resolved;
	}
	return undefined;
}

function exportsTarget(exportsField, subpath) {
	const subpathKeyed =
		exportsField !== null &&
		typeof exportsField === "object" &&
		!Array.isArray(exportsField) &&
		Object.keys(exportsField).some((key) => key.startsWith("."));
	if (!subpathKeyed) return subpath === "." ? conditionalTarget(exportsField) : undefined;
	if (Object.hasOwn(exportsField, subpath)) return conditionalTarget(exportsField[subpath]);
	let best;
	for (const key of Object.keys(exportsField)) {
…
	}
	if (best === undefined || hasInvalidSegment(best.match)) return undefined;
	const target = conditionalTarget(exportsField[best.key]);
	return target === undefined ? undefined : target.replaceAll("*", best.match);
}

// Node refuses a pattern match or target with "." / ".." / "node_modules" segments (ERR_INVALID_MODULE_SPECIFIER).
function hasInvalidSegment(path) {
	return path.split(/[\\/]/).some((segment) => segment === "." || segment === ".." || segment === "node_modules");
}

function legacyFile(path) {
…
}

function packageEntry(packageDir, subpath, specifier) {
…
}

function findPackageDir(name, rootUrl) {
…
}

/**
 * The file URL the bare `specifier` resolves to from the first of `rootUrls` whose `node_modules` chain has its
 * package, or undefined when it names a builtin or no root has the package (the caller then imports it natively).
 */
export function resolvePackage(specifier, rootUrls) {
…
}

export function installPackageResolver() {
	globalThis[RESOLVE_PACKAGE] = resolvePackage;
}



[Elided source: offset=14 limit=5; offset=22 limit=5; offset=41 limit=5; offset=66 limit=9; offset=87 limit=4; offset=94 limit=19; offset=116 limit=8; offset=131 limit=18. Reread source before editing; markers are not source.]
