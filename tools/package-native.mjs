#!/usr/bin/env bun
// Package a native distribution: stage the built `mhc` binary beside its sibling binaries, the
// bundled skills, the browser skill runtime and the retained licenses, and write a hash manifest that
// can be re-verified. mhc is MIT-only (docs/MIT-PROFILE.md), so there is one packaging mode.
//
//   bun tools/package-native.mjs --binary <path/to/mhc> --omowright-runtime <dir> --output <staging-dir>
//   bun tools/package-native.mjs --verify-only <staging-dir>
//   bun tools/package-native.mjs --self-test
//
// The staged layout is exactly what the native runtime resolves at startup (`.exe` on Windows):
//
//   <staging>/mhc                   the payload CLI binary (runs from R/versions/<version>/ once installed)
//   <staging>/mhc-launcher          the stable launcher: installers copy it to the install root as `mhc`
//   <staging>/mhc-lsp               optional: the LSP mux daemon (crate maho-lsp)
//   <staging>/senpi-desktop-engine  optional: the desktop engine (crate maho-senpi-desktop-engine)
//   <staging>/mhc-git-bash          optional: the Git Bash MCP server (crate maho-git-bash)
//   <staging>/mhc-ast-grep          optional: the ast-grep MCP server (crate maho-ast-grep)
//   <staging>/skills/<name>/        the bundled skills (maho-skills: $MAHO_SKILLS_ROOT, else <exe dir>/skills)
//   <staging>/skills/browser/runtime/omowright/  the browser skill's materialized engine runtime
//   <staging>/licenses/             retained license texts
//   <staging>/manifest.json         per-file sha256, sizes and modes, the build version, pins and sources
//
// Each optional sibling is staged from its flag (which must name an existing file) or, without the
// flag, from `<binary dir>/<name>` when that file exists; it is never guessed or written as a placeholder.
//
// Skills come from `assets/skills/` and its `manifest.json` (`skills.<name>: [{path, bytes, sha256}]`):
// every listed file must exist with the recorded size and sha256, and a file or skill directory the
// manifest does not list refuses the run, so a stage never ships a truncated or polluted skill.
//
// The browser skill's engine runtime is build-materialized and never committed: `--omowright-runtime
// <dir>` is required (`node tools/stage-omowright-runtime.mjs --source <omowright checkout> --target
// <dir>`), and its own manifest must record the omowright commit `assets/skills/manifest.json` declares
// in `browserRuntime.dependency.commit`.
//
// The manifest's `versions.mhc` is the real build version of the payload CLI (what `mhc --version`
// prints, i.e. MHC_BUILD_VERSION at build time): it is read by running the staged payload, or taken from
// --build-version when the payload cannot run on the packaging host. Installers use it as the
// `versions/<version>/` directory name.
//
// Options:
//   --binary <path>               the built payload `mhc` executable (required to stage)
//   --output <dir>                the staging directory to create (required to stage)
//   --omowright-runtime <dir>     materialized browser engine runtime directory (required to stage)
//   --launcher-binary <path>      `mhc-launcher` to stage (default: <binary dir>/mhc-launcher)
//   --mhc-lsp-binary <path>       `mhc-lsp` sibling (default: <binary dir>/mhc-lsp when present)
//   --desktop-engine-binary <p>   `senpi-desktop-engine` sibling (default: <binary dir>/senpi-desktop-engine when present)
//   --git-bash-binary <path>      `mhc-git-bash` sibling (default: <binary dir>/mhc-git-bash when present)
//   --ast-grep-binary <path>      `mhc-ast-grep` sibling (default: <binary dir>/mhc-ast-grep when present)
//   --build-version <v>           build version of the payload (default: the output of `<binary> --version`)
//   --repo-root <dir>             this checkout (default: the tree holding Cargo.toml + crates/)
//   --force                       overwrite an existing non-empty staging directory
//   --verify-only <dir>           verify an existing staging directory against its manifest
//   --self-test                   run the deterministic fixture self-test
//
// The script never writes to a real install path: --output is mandatory and must be named
// explicitly. Exit 0 = pass, 1 = failure, 2 = usage error.
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { chmod, copyFile, mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { dirname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

const MANIFEST_NAME = "manifest.json";
const SKILLS_DIR = "skills";
const LICENSES_DIR = "licenses";
const SCHEMA_VERSION = 1;
// Windows executables carry `.exe`; the runtime resolves every sibling by that platform name.
const EXE_SUFFIX = process.platform === "win32" ? ".exe" : "";
const CLI_BINARY_NAME = `mhc${EXE_SUFFIX}`;
const LAUNCHER_NAME = `mhc-launcher${EXE_SUFFIX}`;
const BUILD_VERSION_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._+-]{0,127}$/;
const COMMIT_PATTERN = /^[0-9a-f]{40}$/;
const SHA256_PATTERN = /^[0-9a-f]{64}$/;

// Optional sibling binaries: manifest layout key, staged name, CLI flag and option key.
export const SIBLINGS = [
	{ key: "lspDaemon", name: `mhc-lsp${EXE_SUFFIX}`, flag: "--mhc-lsp-binary", option: "mhcLspBinary" },
	{ key: "desktopEngine", name: `senpi-desktop-engine${EXE_SUFFIX}`, flag: "--desktop-engine-binary", option: "desktopEngineBinary" },
	{ key: "gitBash", name: `mhc-git-bash${EXE_SUFFIX}`, flag: "--git-bash-binary", option: "gitBashBinary" },
	{ key: "astGrep", name: `mhc-ast-grep${EXE_SUFFIX}`, flag: "--ast-grep-binary", option: "astGrepBinary" },
];

// The repository-owned skill tree and its manifest, relative to the repository root.
const SKILLS_ASSETS_DIR = "assets/skills";
const SKILLS_MANIFEST = `${SKILLS_ASSETS_DIR}/manifest.json`;

// The browser skill's engine runtime, materialized from the omowright commit the skills manifest
// declares; the repository holds no committed copy.
const BROWSER_SKILL_NAME = "browser";
const BROWSER_RUNTIME_MANIFEST = "manifest.json";
const BROWSER_RUNTIME_FILES = ["index.js", "page-bundle.js", BROWSER_RUNTIME_MANIFEST];
const BROWSER_RUNTIME_TARGET_DIR = `${SKILLS_DIR}/${BROWSER_SKILL_NAME}/runtime/omowright`;

// License texts retained with the staged runtime: the MIT text and the notices of the third-party
// components the package carries (crate dependencies, bundled skills and the browser runtime).
export const LICENSE_FILES = [
	{ name: "MIT.txt", source: "LICENSES/MIT.txt" },
	{ name: "LICENSE", source: "LICENSE" },
	{ name: "THIRD_PARTY_LICENSES.md", source: "THIRD_PARTY_LICENSES.md" },
	{ name: "lazycodex-MIT.txt", source: "LICENSES/lazycodex-MIT.txt" },
	{ name: "maho-ext-webfetch-LICENSE-READABILITY.txt", source: "crates/builtins/maho-ext-webfetch/LICENSE-READABILITY.txt" },
	{ name: "maho-ext-webfetch-LICENSE-TURNDOWN.txt", source: "crates/builtins/maho-ext-webfetch/LICENSE-TURNDOWN.txt" },
	{ name: "maho-ext-webfetch-LICENSE-HTMLPARSER2.txt", source: "crates/builtins/maho-ext-webfetch/LICENSE-HTMLPARSER2.txt" },
	{ name: "maho-ext-omo-zcode-oauth-LICENSE", source: "crates/extensions/maho-ext-omo-zcode-oauth/LICENSE" },
	{ name: "maho-ext-pi-webfetch-THIRD_PARTY_NOTICES.md", source: "crates/extensions/maho-ext-pi-webfetch/THIRD_PARTY_NOTICES.md" },
	{ name: "maho-ext-pi-rules-THIRD_PARTY_NOTICES.md", source: "crates/extensions/maho-ext-pi-rules/THIRD_PARTY_NOTICES.md" },
	{ name: "frontend-LICENSE-Apache-2.0.txt", source: "assets/skills/frontend/LICENSE-Apache-2.0.txt" },
	{ name: "ast-grep-LICENSE", source: "assets/skills/ast-grep/LICENSE" },
	{ name: "node-omowright-LICENSE", source: "LICENSES/node/omowright-LICENSE" },
	{ name: "node-ws-LICENSE", source: "LICENSES/node/ws-LICENSE" },
	{ name: "node-zod-LICENSE", source: "LICENSES/node/zod-LICENSE" },
];

function usage() {
	console.log(`usage: bun tools/package-native.mjs --binary <mhc> --omowright-runtime <dir> --output <dir> [--launcher-binary <path>] [--mhc-lsp-binary <path>] [--desktop-engine-binary <path>] [--git-bash-binary <path>] [--ast-grep-binary <path>] [--build-version <v>] [--repo-root <dir>] [--force]
       bun tools/package-native.mjs --verify-only <dir>
       bun tools/package-native.mjs --self-test`);
}

function usageError(message) {
	console.error(`package-native: ${message}`);
	usage();
	process.exit(2);
}

// ---------- hashing and copying ----------

async function sha256(path) {
	const hash = createHash("sha256");
	hash.update(await readFile(path));
	return hash.digest("hex");
}

function modeString(mode) {
	return `0${(mode & 0o777).toString(8).padStart(3, "0")}`;
}

/** Every regular file under `root` as sorted POSIX-style relative paths; any other entry type throws. */
async function listFiles(root) {
	const files = [];
	async function visit(rel) {
		const entries = await readdir(join(root, rel), { withFileTypes: true });
		for (const entry of entries) {
			const child = rel === "" ? entry.name : `${rel}/${entry.name}`;
			if (entry.isDirectory()) await visit(child);
			else if (entry.isFile()) files.push(child);
			else throw new Error(`not a regular file or directory: ${join(root, child)}`);
		}
	}
	await visit("");
	return files.sort();
}

async function isFile(path) {
	return existsSync(path) && (await stat(path)).isFile();
}

async function copyExecutable(source, destination) {
	await copyFile(source, destination);
	await chmod(destination, 0o755);
}

async function fileEntry(output, relPath, source) {
	const absolute = join(output, relPath);
	const info = await stat(absolute);
	return { path: relPath, source, sha256: await sha256(absolute), size: info.size, mode: modeString(info.mode) };
}

// ---------- inputs ----------

/** Normalizes a build version to the string `mhc --version` prints, without a leading `v`. */
function normalizeBuildVersion(raw, source) {
	const text = String(raw ?? "").replace(/\r/g, "").trim();
	const version = text.startsWith("v") || text.startsWith("V") ? text.slice(1) : text;
	if (!BUILD_VERSION_PATTERN.test(version)) throw new Error(`${source} did not yield a usable build version (got ${JSON.stringify(text.slice(0, 80))})`);
	return version;
}

/** The real build version of the payload: its own `--version` output, bounded by a timeout. */
async function payloadBuildVersion(binary) {
	let child;
	try {
		child = Bun.spawn([binary, "--version"], { stdout: "pipe", stderr: "ignore", stdin: "ignore" });
	} catch (error) {
		throw new Error(`cannot run ${binary} --version to read the build version (pass --build-version for a payload that cannot run here): ${error.message}`);
	}
	const timer = setTimeout(() => child.kill(), 30000);
	try {
		const [stdout, code] = await Promise.all([new Response(child.stdout).text(), child.exited]);
		if (code !== 0) throw new Error(`${binary} --version exited with ${code}`);
		return normalizeBuildVersion(stdout, `${binary} --version`);
	} finally {
		clearTimeout(timer);
	}
}

/** Reads the senpi source pin from PINS.md (`| senpi | short | full | location |`). */
async function parseSenpiPin(repoRoot) {
	const path = join(repoRoot, "PINS.md");
	if (!existsSync(path)) return null;
	for (const line of (await readFile(path, "utf8")).split("\n")) {
		const cells = line.split("|").map((cell) => cell.trim());
		if (cells.length >= 5 && cells[1] === "senpi") return cells[3];
	}
	return null;
}

function resolveRepoRoot(explicit) {
	if (explicit) return resolve(explicit);
	let directory = here;
	for (;;) {
		if (existsSync(join(directory, "Cargo.toml")) && existsSync(join(directory, "crates"))) return directory;
		const parent = dirname(directory);
		if (parent === directory) break;
		directory = parent;
	}
	throw new Error("could not locate the repository root (Cargo.toml + crates/); pass --repo-root");
}

/**
 * Reads `assets/skills/manifest.json` and proves the skill tree against it: every listed file exists
 * with the recorded size and sha256, and no file or skill directory is unlisted. Returns the skill
 * names, the per-skill file lists and the declared browser runtime identity.
 */
export async function readSkills(repoRoot) {
	const root = join(repoRoot, SKILLS_ASSETS_DIR);
	const manifestPath = join(repoRoot, SKILLS_MANIFEST);
	if (!existsSync(manifestPath)) throw new Error(`skills manifest not found: ${manifestPath}`);
	let manifest;
	try {
		manifest = JSON.parse(await readFile(manifestPath, "utf8"));
	} catch (error) {
		throw new Error(`skills manifest is not valid JSON: ${error.message}`);
	}
	if (manifest === null || typeof manifest !== "object" || Array.isArray(manifest)) throw new Error(`skills manifest is not an object: ${manifestPath}`);
	if (manifest.schemaVersion !== 1) throw new Error(`skills manifest schemaVersion must be 1 (found ${JSON.stringify(manifest.schemaVersion)})`);
	const declared = manifest.skills !== null && typeof manifest.skills === "object" && !Array.isArray(manifest.skills) ? manifest.skills : {};
	const names = Object.keys(declared).sort();
	if (names.length === 0) throw new Error(`skills manifest declares no skills: ${manifestPath}`);

	const errors = [];
	// The materializer's default output (`browserRuntime.sourceDir`, gitignored) may sit in the tree;
	// it is a build output, not a skill, and is staged only through --omowright-runtime.
	const runtimeSourceTop = typeof manifest.browserRuntime?.sourceDir === "string" ? manifest.browserRuntime.sourceDir.split("/")[0] : null;
	const onDiskDirs = (await readdir(root, { withFileTypes: true })).filter((entry) => entry.isDirectory() && entry.name !== runtimeSourceTop).map((entry) => entry.name);
	for (const name of onDiskDirs) if (!(name in declared)) errors.push(`skill directory not listed in ${SKILLS_MANIFEST}: ${SKILLS_ASSETS_DIR}/${name}`);
	const skills = new Map();
	for (const name of names) {
		const directory = join(root, name);
		const entries = declared[name];
		if (!existsSync(directory) || !(await stat(directory)).isDirectory()) {
			errors.push(`missing skill tree: ${SKILLS_ASSETS_DIR}/${name}`);
			continue;
		}
		if (!Array.isArray(entries) || entries.length === 0) {
			errors.push(`skill ${name} has no declared file list`);
			continue;
		}
		const valid = entries.every((entry) => entry !== null && typeof entry === "object" && typeof entry.path === "string" && entry.path.length > 0 && Number.isSafeInteger(entry.bytes) && typeof entry.sha256 === "string" && SHA256_PATTERN.test(entry.sha256));
		if (!valid) {
			errors.push(`skill ${name} declares an entry without a path, bytes and sha256`);
			continue;
		}
		const declaredPaths = entries.map((entry) => entry.path);
		if (!declaredPaths.includes("SKILL.md")) errors.push(`skill ${name} does not declare SKILL.md`);
		const runtimeOverlap = declaredPaths.filter((path) => `${SKILLS_DIR}/${name}/${path}`.startsWith(`${BROWSER_RUNTIME_TARGET_DIR}/`));
		if (runtimeOverlap.length > 0) errors.push(`skill ${name} declares files inside the materialized runtime directory: ${runtimeOverlap.join(", ")}`);
		const onDisk = await listFiles(directory);
		const missing = declaredPaths.filter((path) => !onDisk.includes(path));
		const extra = onDisk.filter((path) => !declaredPaths.includes(path));
		if (missing.length > 0) errors.push(`skill ${name} is missing declared files: ${missing.join(", ")}`);
		if (extra.length > 0) errors.push(`skill ${name} has files not listed in ${SKILLS_MANIFEST}: ${extra.join(", ")}`);
		const mismatched = [];
		for (const entry of entries) {
			const path = join(directory, entry.path);
			if (!onDisk.includes(entry.path)) continue;
			if ((await stat(path)).size !== entry.bytes || (await sha256(path)) !== entry.sha256) mismatched.push(entry.path);
		}
		if (mismatched.length > 0) errors.push(`skill ${name} files disagree with their recorded size/sha256: ${mismatched.join(", ")}`);
		skills.set(name, { directory, files: [...declaredPaths].sort() });
	}

	let browserRuntime = null;
	const runtime = manifest.browserRuntime;
	if (runtime !== undefined && runtime !== null) {
		const dependency = runtime.dependency;
		if (typeof runtime !== "object" || Array.isArray(runtime)) errors.push("browserRuntime is not an object");
		else if (runtime.skill !== BROWSER_SKILL_NAME) errors.push(`browserRuntime.skill must be ${JSON.stringify(BROWSER_SKILL_NAME)} (found ${JSON.stringify(runtime.skill)})`);
		else if (runtime.targetDir !== BROWSER_RUNTIME_TARGET_DIR) errors.push(`browserRuntime.targetDir must be ${JSON.stringify(BROWSER_RUNTIME_TARGET_DIR)} (found ${JSON.stringify(runtime.targetDir)})`);
		else if (typeof runtime.materializer !== "string" || runtime.materializer.length === 0) errors.push("browserRuntime must name the materializer that produces the runtime");
		else if (JSON.stringify([...(runtime.files ?? [])].sort()) !== JSON.stringify([...BROWSER_RUNTIME_FILES].sort())) errors.push(`browserRuntime.files must be ${JSON.stringify(BROWSER_RUNTIME_FILES)}`);
		else if (dependency === null || typeof dependency !== "object" || dependency.name !== "omowright" || typeof dependency.commit !== "string" || !COMMIT_PATTERN.test(dependency.commit)) {
			errors.push("browserRuntime.dependency must record the omowright package and a full 40-hex commit");
		} else if (!names.includes(BROWSER_SKILL_NAME)) errors.push(`browserRuntime is declared but the ${BROWSER_SKILL_NAME} skill is not listed`);
		else {
			browserRuntime = {
				skill: runtime.skill,
				targetDir: runtime.targetDir,
				materializer: runtime.materializer,
				dependency: { name: dependency.name, spec: typeof dependency.spec === "string" ? dependency.spec : null, commit: dependency.commit },
			};
		}
	}
	if (errors.length > 0) throw new Error(`${SKILLS_ASSETS_DIR} does not match ${SKILLS_MANIFEST}:\n  ${errors.join("\n  ")}`);
	return { names, skills, browserRuntime, manifestSha256: await sha256(manifestPath) };
}

/**
 * Resolves the materialized browser engine runtime: an explicit directory holding every runtime file,
 * whose own manifest records the omowright commit the skills manifest declares.
 */
async function resolveBrowserRuntime(declared, explicit) {
	if (explicit === undefined || explicit === null) {
		throw new Error(`the ${declared.skill} skill needs its engine runtime: pass --omowright-runtime <dir>, materialized with \`node ${declared.materializer} --source <omowright checkout> --target <dir>\``);
	}
	const directory = resolve(explicit);
	if (!existsSync(directory) || !(await stat(directory)).isDirectory()) throw new Error(`--omowright-runtime is not a directory: ${directory}`);
	for (const file of BROWSER_RUNTIME_FILES) {
		if (!(await isFile(join(directory, file)))) throw new Error(`--omowright-runtime is missing ${file}: ${join(directory, file)}`);
	}
	let runtimeManifest;
	try {
		runtimeManifest = JSON.parse(await readFile(join(directory, BROWSER_RUNTIME_MANIFEST), "utf8"));
	} catch (error) {
		throw new Error(`--omowright-runtime manifest is not valid JSON: ${error.message}`);
	}
	if (runtimeManifest === null || typeof runtimeManifest !== "object" || Array.isArray(runtimeManifest)) throw new Error(`--omowright-runtime manifest is not an object: ${directory}`);
	if (typeof runtimeManifest.version !== "string" || runtimeManifest.version.length === 0) throw new Error(`--omowright-runtime manifest records no omowright version: ${directory}`);
	if (typeof runtimeManifest.sourceDigest !== "string" || !SHA256_PATTERN.test(runtimeManifest.sourceDigest)) throw new Error(`--omowright-runtime manifest records no 64-hex sourceDigest: ${directory}`);
	if (typeof runtimeManifest.commit !== "string" || !COMMIT_PATTERN.test(runtimeManifest.commit)) throw new Error(`--omowright-runtime manifest records no full omowright commit: ${directory}`);
	if (runtimeManifest.commit !== declared.dependency.commit) {
		throw new Error(`--omowright-runtime was materialized from omowright ${runtimeManifest.commit}, not the declared commit ${declared.dependency.commit}`);
	}
	return { directory, version: runtimeManifest.version, sourceDigest: runtimeManifest.sourceDigest };
}

/** The optional sibling for `spec`: its flag (must be a file), else `<binary dir>/<name>` when present. */
async function resolveSibling(spec, options, binary) {
	const explicit = options[spec.option];
	if (explicit !== undefined && explicit !== null) {
		const path = resolve(explicit);
		if (!(await isFile(path))) throw new Error(`${spec.flag} is not a regular file: ${path}`);
		return path;
	}
	const beside = join(dirname(binary), spec.name);
	return (await isFile(beside)) ? beside : null;
}

// ---------- staging ----------

export async function stage(options) {
	const binary = resolve(options.binary);
	if (!(await isFile(binary))) throw new Error(`--binary is not a regular file: ${binary}`);
	const launcher = resolve(options.launcherBinary ?? join(dirname(binary), LAUNCHER_NAME));
	if (!(await isFile(launcher))) {
		throw new Error(`mhc launcher not found at ${launcher}: build the maho-cli bins (cargo build -p maho-cli --bins) so ${LAUNCHER_NAME} sits beside ${CLI_BINARY_NAME}, or pass --launcher-binary`);
	}
	const siblings = [];
	for (const spec of SIBLINGS) {
		const path = await resolveSibling(spec, options, binary);
		if (path !== null) siblings.push({ ...spec, path });
	}
	const buildVersion = options.buildVersion ? normalizeBuildVersion(options.buildVersion, "--build-version") : await payloadBuildVersion(binary);

	// Refuse an unsafe or non-empty destination before anything is written. Every input is resolved,
	// read and validated before the destination is cleared, so a refused run leaves an existing stage intact.
	const output = resolve(options.output);
	if (output === sep || output === resolve(homedir())) throw new Error(`refusing to stage into ${output}`);
	const outputExists = existsSync(output);
	if (outputExists && (await readdir(output)).length > 0 && !options.force) throw new Error(`staging directory is not empty: ${output} (pass --force to overwrite)`);

	const repoRoot = resolveRepoRoot(options.repoRoot);
	const skills = await readSkills(repoRoot);
	const browserRuntime = skills.browserRuntime === null ? null : await resolveBrowserRuntime(skills.browserRuntime, options.omowrightRuntime);
	const licenseSources = new Map();
	for (const license of LICENSE_FILES) {
		const source = join(repoRoot, license.source);
		if (!(await isFile(source))) throw new Error(`license text missing from the checkout: ${license.source}`);
		licenseSources.set(license.name, source);
	}
	const senpiPin = await parseSenpiPin(repoRoot);
	if (typeof senpiPin !== "string" || !COMMIT_PATTERN.test(senpiPin)) throw new Error(`PINS.md senpi pin must be a full 40-hex commit (found ${JSON.stringify(senpiPin)})`);

	if (outputExists) await rm(output, { recursive: true, force: true });
	await mkdir(join(output, SKILLS_DIR), { recursive: true });
	await mkdir(join(output, LICENSES_DIR), { recursive: true });

	const layout = { binary: CLI_BINARY_NAME, launcher: LAUNCHER_NAME, skills: SKILLS_DIR, licenses: LICENSES_DIR, manifest: MANIFEST_NAME };
	const executables = [];
	await copyExecutable(binary, join(output, CLI_BINARY_NAME));
	executables.push(await fileEntry(output, CLI_BINARY_NAME, binary));
	await copyExecutable(launcher, join(output, LAUNCHER_NAME));
	executables.push(await fileEntry(output, LAUNCHER_NAME, launcher));
	for (const sibling of siblings) {
		await copyExecutable(sibling.path, join(output, sibling.name));
		executables.push(await fileEntry(output, sibling.name, sibling.path));
		layout[sibling.key] = sibling.name;
	}

	const licenses = [];
	for (const license of LICENSE_FILES) {
		const relPath = `${LICENSES_DIR}/${license.name}`;
		await copyFile(licenseSources.get(license.name), join(output, relPath));
		licenses.push(await fileEntry(output, relPath, license.source));
	}

	const skillFiles = [];
	for (const name of skills.names) {
		const { directory, files } = skills.skills.get(name);
		for (const rel of files) {
			const relPath = `${SKILLS_DIR}/${name}/${rel}`;
			await mkdir(dirname(join(output, relPath)), { recursive: true });
			await copyFile(join(directory, rel), join(output, relPath));
			await chmod(join(output, relPath), (await stat(join(directory, rel))).mode & 0o777);
			skillFiles.push(await fileEntry(output, relPath, `${SKILLS_ASSETS_DIR}/${name}/${rel}`));
		}
	}

	const sources = { skills: { root: SKILLS_ASSETS_DIR, manifest: SKILLS_MANIFEST, manifestSha256: skills.manifestSha256 } };
	if (browserRuntime !== null) {
		for (const file of BROWSER_RUNTIME_FILES) {
			const relPath = `${BROWSER_RUNTIME_TARGET_DIR}/${file}`;
			await mkdir(dirname(join(output, relPath)), { recursive: true });
			await copyFile(join(browserRuntime.directory, file), join(output, relPath));
			skillFiles.push(await fileEntry(output, relPath, `${skills.browserRuntime.materializer}:${file}`));
		}
		sources.browserRuntime = {
			...skills.browserRuntime,
			version: browserRuntime.version,
			sourceDigest: browserRuntime.sourceDigest,
			files: BROWSER_RUNTIME_FILES.map((file) => `${BROWSER_RUNTIME_TARGET_DIR}/${file}`),
		};
	}

	const manifest = {
		schemaVersion: SCHEMA_VERSION,
		generator: "tools/package-native.mjs",
		license: "MIT",
		layout,
		versions: { mhc: buildVersion },
		pins: { senpi: senpiPin },
		sources,
		executables,
		licenses,
		skills: { names: skills.names, files: skillFiles },
	};
	await writeFile(join(output, MANIFEST_NAME), `${JSON.stringify(manifest, null, 2)}\n`, "utf8");

	const verification = await verify(output);
	if (!verification.ok) throw new Error(`staged manifest failed self-verification:\n  ${verification.errors.join("\n  ")}`);
	return { ok: true, output, manifestPath: join(output, MANIFEST_NAME), manifest };
}

// ---------- GLIBC ceiling ----------

const DEFAULT_GLIBC_CEILING = "2.28";

export function compareGlibcVersions(a, b) {
	const left = String(a).split(".").map(Number);
	const right = String(b).split(".").map(Number);
	for (let index = 0; index < Math.max(left.length, right.length); index++) {
		const delta = (left[index] ?? 0) - (right[index] ?? 0);
		if (delta !== 0) return delta < 0 ? -1 : 1;
	}
	return 0;
}

// Highest GLIBC_x.y symbol version an ELF64 little-endian image requires (via .gnu.version_r), or
// null when the image is not such an ELF or needs no versioned glibc symbol. Pure JS: no binutils.
export function highestGlibcRequirement(buffer) {
	if (buffer.length < 64 || buffer[0] !== 0x7f || buffer[1] !== 0x45 || buffer[2] !== 0x4c || buffer[3] !== 0x46) return null;
	if (buffer[4] !== 2 || buffer[5] !== 1) return null;
	const shoff = Number(buffer.readBigUInt64LE(0x28));
	const shentsize = buffer.readUInt16LE(0x3a);
	const shnum = buffer.readUInt16LE(0x3c);
	let highest = null;
	for (let index = 0; index < shnum; index++) {
		const header = shoff + index * shentsize;
		if (header + 64 > buffer.length) break;
		if (buffer.readUInt32LE(header + 4) !== 0x6ffffffe) continue;
		const offset = Number(buffer.readBigUInt64LE(header + 0x18));
		const link = buffer.readUInt32LE(header + 0x28);
		const info = buffer.readUInt32LE(header + 0x2c);
		const strtab = shoff + link * shentsize;
		const strOffset = Number(buffer.readBigUInt64LE(strtab + 0x18));
		const readString = (at) => {
			const start = strOffset + at;
			return buffer.toString("latin1", start, buffer.indexOf(0, start));
		};
		let verneed = offset;
		for (let entry = 0; entry < info; entry++) {
			const auxCount = buffer.readUInt16LE(verneed + 2);
			let aux = verneed + buffer.readUInt32LE(verneed + 8);
			for (let k = 0; k < auxCount; k++) {
				const match = /^GLIBC_(\d+(?:\.\d+)+)$/.exec(readString(buffer.readUInt32LE(aux + 8)));
				if (match && (highest === null || compareGlibcVersions(match[1], highest) > 0)) highest = match[1];
				aux += buffer.readUInt32LE(aux + 12);
			}
			verneed += buffer.readUInt32LE(verneed + 12);
		}
	}
	return highest;
}

async function glibcCeilingErrors(directory) {
	const ceiling = process.env.MHC_GLIBC_CEILING || DEFAULT_GLIBC_CEILING;
	if (!/^\d+(\.\d+)+$/.test(ceiling)) return [`MHC_GLIBC_CEILING must look like 2.28 (found ${JSON.stringify(ceiling)})`];
	const errors = [];
	const walk = async (dir) => {
		for (const entry of await readdir(dir, { withFileTypes: true })) {
			const absolute = join(dir, entry.name);
			if (entry.isDirectory()) await walk(absolute);
			else if (entry.isFile()) {
				const buffer = await readFile(absolute);
				const required = highestGlibcRequirement(buffer);
				if (required !== null && compareGlibcVersions(required, ceiling) > 0) {
					errors.push(`${absolute.slice(directory.length + 1)} requires GLIBC_${required}, above the GLIBC_${ceiling} ceiling`);
				}
			}
		}
	};
	await walk(directory);
	return errors;
}


// ---------- verification ----------

export async function verify(directory) {
	const manifestPath = join(directory, MANIFEST_NAME);
	if (!existsSync(manifestPath)) return { ok: false, errors: [`${MANIFEST_NAME} not found in ${directory}`] };
	let manifest;
	try {
		manifest = JSON.parse(await readFile(manifestPath, "utf8"));
	} catch (error) {
		return { ok: false, errors: [`${MANIFEST_NAME} is not valid JSON: ${error.message}`] };
	}
	if (manifest === null || typeof manifest !== "object" || Array.isArray(manifest)) {
		return { ok: false, errors: [`${MANIFEST_NAME} is not a manifest object`], manifest };
	}
	// The manifest is the package proof: an empty or foreign object must not pass just because its
	// optional arrays are absent.
	const errors = [];
	if (manifest.schemaVersion !== SCHEMA_VERSION) errors.push(`unsupported schemaVersion: ${JSON.stringify(manifest.schemaVersion)} (expected ${SCHEMA_VERSION})`);
	const expectedLayout = { binary: CLI_BINARY_NAME, launcher: LAUNCHER_NAME, skills: SKILLS_DIR, licenses: LICENSES_DIR, manifest: MANIFEST_NAME };
	for (const [key, value] of Object.entries(expectedLayout)) {
		if (manifest.layout?.[key] !== value) errors.push(`layout.${key} must be ${JSON.stringify(value)} (found ${JSON.stringify(manifest.layout?.[key])})`);
	}
	const siblingByKey = new Map(SIBLINGS.map((spec) => [spec.key, spec]));
	for (const [key, value] of Object.entries(manifest.layout ?? {})) {
		if (key in expectedLayout) continue;
		const spec = siblingByKey.get(key);
		if (spec === undefined) errors.push(`layout.${key} is not a known package entry`);
		else if (value !== spec.name) errors.push(`layout.${key} must be ${JSON.stringify(spec.name)} (found ${JSON.stringify(value)})`);
	}
	if (typeof manifest.pins?.senpi !== "string" || !COMMIT_PATTERN.test(manifest.pins.senpi)) errors.push(`pin senpi must be a full 40-hex commit (found ${JSON.stringify(manifest.pins?.senpi)})`);
	// versions.mhc is the install directory name (versions/<version>/): it must be the concrete build version.
	if (typeof manifest.versions?.mhc !== "string" || !BUILD_VERSION_PATTERN.test(manifest.versions.mhc)) {
		errors.push(`versions.mhc must be the payload build version (found ${JSON.stringify(manifest.versions?.mhc)})`);
	}

	const executables = Array.isArray(manifest.executables) ? manifest.executables : [];
	const licenses = Array.isArray(manifest.licenses) ? manifest.licenses : [];
	const skillFiles = Array.isArray(manifest.skills?.files) ? manifest.skills.files : [];
	const tracked = new Set([MANIFEST_NAME]);
	for (const entry of [...executables, ...licenses, ...skillFiles]) {
		if (entry === null || typeof entry !== "object" || typeof entry.path !== "string") {
			errors.push("manifest entry without a path");
			continue;
		}
		if (tracked.has(entry.path)) errors.push(`duplicate manifest path: ${entry.path}`);
		tracked.add(entry.path);
		const absolute = join(directory, entry.path);
		if (!existsSync(absolute)) {
			errors.push(`missing: ${entry.path}`);
			continue;
		}
		const info = await stat(absolute);
		if (!info.isFile()) {
			errors.push(`not a regular file: ${entry.path}`);
			continue;
		}
		if (info.size !== entry.size) errors.push(`size mismatch: ${entry.path} (${info.size} != ${entry.size})`);
		if ((await sha256(absolute)) !== entry.sha256) errors.push(`sha256 mismatch: ${entry.path}`);
		if (modeString(info.mode) !== entry.mode) errors.push(`mode mismatch: ${entry.path} (${modeString(info.mode)} != ${entry.mode})`);
	}

	// Executables: mhc and the launcher always; a sibling exactly when the layout declares it.
	const allowedExecutables = new Set([CLI_BINARY_NAME, LAUNCHER_NAME]);
	for (const spec of SIBLINGS) if (manifest.layout?.[spec.key] === spec.name) allowedExecutables.add(spec.name);
	for (const name of allowedExecutables) {
		if (!executables.some((entry) => entry?.path === name)) errors.push(`required executable missing from the manifest: ${name}`);
	}
	for (const entry of executables) {
		if (typeof entry?.path === "string" && !allowedExecutables.has(entry.path)) errors.push(`executable not declared by the layout: ${entry.path}`);
	}
	for (const license of LICENSE_FILES) {
		const relPath = `${LICENSES_DIR}/${license.name}`;
		if (!licenses.some((entry) => entry?.path === relPath)) errors.push(`required license missing from the manifest: ${relPath}`);
	}
	for (const entry of licenses) {
		if (typeof entry?.path === "string" && !LICENSE_FILES.some((license) => `${LICENSES_DIR}/${license.name}` === entry.path)) errors.push(`unknown license entry: ${entry.path}`);
	}

	const skillNames = Array.isArray(manifest.skills?.names) ? manifest.skills.names : [];
	if (skillNames.length === 0) errors.push("manifest declares no bundled skills");
	if (skillFiles.length === 0) errors.push("manifest declares no staged skill files");
	for (const name of skillNames) {
		const skillMd = `${SKILLS_DIR}/${name}/SKILL.md`;
		if (!skillFiles.some((entry) => entry?.path === skillMd)) errors.push(`declared skill SKILL.md is not in the hashed manifest: ${skillMd}`);
	}
	for (const entry of skillFiles) {
		if (typeof entry?.path !== "string") continue;
		const name = entry.path.split("/")[1];
		if (!entry.path.startsWith(`${SKILLS_DIR}/`) || !skillNames.includes(name)) errors.push(`skill file outside the declared skills: ${entry.path}`);
	}

	// The browser engine runtime is required whenever the browser skill ships: without it the skill's
	// loader throws "omowright is not staged in this skill".
	const runtime = manifest.sources?.browserRuntime;
	if (skillNames.includes(BROWSER_SKILL_NAME) || runtime !== undefined) {
		if (runtime === null || typeof runtime !== "object" || Array.isArray(runtime)) {
			errors.push("sources.browserRuntime must record the staged browser engine runtime");
		} else {
			if (runtime.targetDir !== BROWSER_RUNTIME_TARGET_DIR) errors.push(`browserRuntime.targetDir must be ${JSON.stringify(BROWSER_RUNTIME_TARGET_DIR)} (found ${JSON.stringify(runtime.targetDir)})`);
			if (typeof runtime.dependency?.commit !== "string" || !COMMIT_PATTERN.test(runtime.dependency.commit)) errors.push("browserRuntime.dependency.commit must be a full 40-hex commit");
			if (typeof runtime.version !== "string" || runtime.version.length === 0) errors.push("browserRuntime.version must record the materialized omowright version");
			if (typeof runtime.sourceDigest !== "string" || !SHA256_PATTERN.test(runtime.sourceDigest)) errors.push("browserRuntime.sourceDigest must be a 64-hex digest");
			for (const file of BROWSER_RUNTIME_FILES) {
				const relPath = `${BROWSER_RUNTIME_TARGET_DIR}/${file}`;
				if (!skillFiles.some((entry) => entry?.path === relPath)) errors.push(`browser runtime file is not in the hashed manifest: ${relPath}`);
			}
		}
	}

	// Nothing in the package may escape the manifest.
	for (const relPath of await listFiles(directory)) {
		if (!tracked.has(relPath)) errors.push(`file not listed in the manifest: ${relPath}`);
	}
	for (const error of await glibcCeilingErrors(directory)) errors.push(error);
	return { ok: errors.length === 0, errors, manifest };
}

function manifestFileCount(manifest) {
	return (manifest?.executables?.length ?? 0) + (manifest?.licenses?.length ?? 0) + (manifest?.skills?.files?.length ?? 0);
}

// ---------- self-test ----------

async function selfTest() {
	const root = await mkdtemp(join(tmpdir(), "package-native-self-"));
	const results = [];
	const check = (name, condition) => {
		results.push({ name, ok: Boolean(condition) });
		console.log(`${condition ? "ok  " : "FAIL"} ${name}`);
	};
	const expectReject = async (name, promise, includes) => {
		try {
			await promise;
			check(name, false);
		} catch (error) {
			const ok = typeof includes === "string" ? String(error.message).includes(includes) : true;
			if (!ok) console.log(`     got: ${error.message}`);
			check(name, ok);
		}
	};
	const copyDir = async (from, to) => {
		for (const rel of await listFiles(from)) {
			await mkdir(dirname(join(to, rel)), { recursive: true });
			await copyFile(join(from, rel), join(to, rel));
			await chmod(join(to, rel), (await stat(join(from, rel))).mode & 0o777);
		}
	};
	const editManifest = async (dir, edit) => {
		const manifest = JSON.parse(await readFile(join(dir, MANIFEST_NAME), "utf8"));
		edit(manifest);
		await writeFile(join(dir, MANIFEST_NAME), `${JSON.stringify(manifest, null, 2)}\n`);
	};
	try {
		check("glibc version comparison is numeric, not lexical", compareGlibcVersions("2.9", "2.28") < 0 && compareGlibcVersions("2.43", "2.28") > 0 && compareGlibcVersions("2.28", "2.28.0") === 0);
		const realRepo = resolve(here, "..");
		const missingSources = LICENSE_FILES.filter((license) => !existsSync(join(realRepo, license.source))).map((license) => license.source);
		check(`every LICENSE_FILES source exists in the real repo${missingSources.length ? ` (missing: ${missingSources.join(", ")})` : ""}`, missingSources.length === 0);
		let realSkillsOk = false;
		try {
			const real = await readSkills(realRepo);
			realSkillsOk = real.names.length > 0 && real.browserRuntime !== null;
		} catch (error) {
			console.log(`     ${error.message}`);
		}
		check("the real assets/skills tree matches its manifest", realSkillsOk);
		{
			// Synthetic ELF64 little-endian aarch64 (e_machine 0xb7) image with a .gnu.version_r needing GLIBC_2.17 and GLIBC_2.34.
			const strtab = Buffer.from("\0libc.so.6\0GLIBC_2.17\0GLIBC_2.34\0", "latin1");
			const verneed = Buffer.alloc(48);
			verneed.writeUInt16LE(1, 0);
			verneed.writeUInt16LE(2, 2);
			verneed.writeUInt32LE(1, 4);
			verneed.writeUInt32LE(16, 8);
			verneed.writeUInt32LE(0, 12);
			verneed.writeUInt32LE(11, 16 + 8);
			verneed.writeUInt32LE(16, 16 + 12);
			verneed.writeUInt32LE(22, 32 + 8);
			const strOffset = 64;
			const verneedOffset = strOffset + strtab.length;
			const shoff = verneedOffset + verneed.length;
			const image = Buffer.alloc(shoff + 3 * 64);
			image.set([0x7f, 0x45, 0x4c, 0x46, 2, 1, 1], 0);
			image.writeUInt16LE(0xb7, 0x12);
			image.writeBigUInt64LE(BigInt(shoff), 0x28);
			image.writeUInt16LE(64, 0x3a);
			image.writeUInt16LE(3, 0x3c);
			strtab.copy(image, strOffset);
			verneed.copy(image, verneedOffset);
			const verneedHeader = shoff + 64;
			image.writeUInt32LE(0x6ffffffe, verneedHeader + 4);
			image.writeBigUInt64LE(BigInt(verneedOffset), verneedHeader + 0x18);
			image.writeUInt32LE(2, verneedHeader + 0x28);
			image.writeUInt32LE(1, verneedHeader + 0x2c);
			image.writeBigUInt64LE(BigInt(strOffset), shoff + 2 * 64 + 0x18);
			check("glibc checker reads the highest requirement from an aarch64 ELF64 fixture", highestGlibcRequirement(image) === "2.34");
		}

		// Fixture checkout: PINS.md, the license sources, and an assets/skills tree with its manifest.
		const repo = join(root, "repo");
		const binDir = join(root, "bin");
		const senpiPin = "a".repeat(40);
		const omowrightCommit = "293ca5002cbd4c8b0c104c5934683385ef7e0d3a";
		await mkdir(join(repo, "crates"), { recursive: true });
		await writeFile(join(repo, "Cargo.toml"), `[workspace]\n\n[workspace.package]\nversion = "9.9.9"\n`);
		await writeFile(join(repo, "PINS.md"), `# Source pins\n\n| Source | Short | Full commit | Location |\n| --- | --- | --- | --- |\n| senpi | aaaaaaa | ${senpiPin} | code-yeongyu/senpi |\n`);
		for (const license of LICENSE_FILES) {
			await mkdir(dirname(join(repo, license.source)), { recursive: true });
			await writeFile(join(repo, license.source), `fixture for ${license.name}\n`);
		}
		const skillsRoot = join(repo, SKILLS_ASSETS_DIR);
		const skillFixture = {
			alpha: { "SKILL.md": "# alpha\n", "references/note.md": "note alpha\n" },
			[BROWSER_SKILL_NAME]: { "SKILL.md": "# browser\n", "scripts/omowright.mjs": "export {};\n" },
		};
		// License sources that live inside a skill (frontend, ast-grep) make that skill part of the fixture.
		for (const license of LICENSE_FILES) {
			const match = license.source.match(/^assets\/skills\/([^/]+)\/(.+)$/);
			if (match === null) continue;
			skillFixture[match[1]] ??= { "SKILL.md": `# ${match[1]}\n` };
			skillFixture[match[1]][match[2]] = `fixture for ${license.name}\n`;
		}
		const declaredSkills = {};
		for (const [name, files] of Object.entries(skillFixture)) {
			declaredSkills[name] = [];
			for (const [rel, content] of Object.entries(files)) {
				await mkdir(dirname(join(skillsRoot, name, rel)), { recursive: true });
				await writeFile(join(skillsRoot, name, rel), content);
				declaredSkills[name].push({ path: rel, bytes: Buffer.byteLength(content), sha256: createHash("sha256").update(content).digest("hex") });
			}
		}
		const browserRuntimeDecl = {
			skill: BROWSER_SKILL_NAME,
			targetDir: BROWSER_RUNTIME_TARGET_DIR,
			sourceDir: "runtime/browser/omowright",
			materializer: "tools/stage-omowright-runtime.mjs",
			files: BROWSER_RUNTIME_FILES,
			dependency: { name: "omowright", spec: `github:code-yeongyu/omowright#${omowrightCommit}`, commit: omowrightCommit },
		};
		const writeSkillsManifest = async (extra = {}) => {
			await writeFile(join(repo, SKILLS_MANIFEST), `${JSON.stringify({ schemaVersion: 1, license: "MIT", browserRuntime: browserRuntimeDecl, skills: declaredSkills, ...extra }, null, 2)}\n`);
		};
		await writeSkillsManifest();
		await writeFile(join(skillsRoot, "MIT-SKILLS.md"), "top-level notes are not a skill\n");

		const runtimeDir = join(root, "omowright-runtime");
		const writeRuntime = async (extra = {}) => {
			await mkdir(runtimeDir, { recursive: true });
			await writeFile(join(runtimeDir, "index.js"), "export const connectBrowserSkill = () => 'fixture-session';\n");
			await writeFile(join(runtimeDir, "page-bundle.js"), "(function(){ globalThis.__omowright = { fixture: true } })();\n");
			await writeFile(join(runtimeDir, BROWSER_RUNTIME_MANIFEST), `${JSON.stringify({ name: "omowright", version: "0.0.0-fixture", commit: omowrightCommit, sourceDigest: "a".repeat(64), ...extra }, null, 2)}\n`);
		};
		await writeRuntime();

		await mkdir(binDir, { recursive: true });
		await writeFile(join(binDir, CLI_BINARY_NAME), "#!/bin/sh\necho v2026.1.2-fixture\n");
		await chmod(join(binDir, CLI_BINARY_NAME), 0o755);
		await writeFile(join(binDir, LAUNCHER_NAME), "#!/bin/sh\necho launcher\n");
		const base = { binary: join(binDir, CLI_BINARY_NAME), repoRoot: repo, omowrightRuntime: runtimeDir };

		// --- the minimal stage: mhc, the launcher, skills, the browser runtime and the licenses ---
		const out = join(root, "out");
		const result = await stage({ ...base, output: out });
		check("stage writes a schema-versioned MIT manifest", result.ok && result.manifest.schemaVersion === 1 && result.manifest.license === "MIT");
		check("manifest verifies the staged tree", (await verify(out)).ok);
		check("mhc and the launcher are staged executables", JSON.stringify(result.manifest.executables.map((entry) => entry.path).sort()) === JSON.stringify([CLI_BINARY_NAME, LAUNCHER_NAME].sort()));
		check("staged executables carry the exec bit", ((await stat(join(out, CLI_BINARY_NAME))).mode & 0o111) !== 0 && ((await stat(join(out, LAUNCHER_NAME))).mode & 0o111) !== 0);
		check("no sibling is staged when none is named or built", SIBLINGS.every((spec) => !existsSync(join(out, spec.name)) && result.manifest.layout[spec.key] === undefined));
		check("staged skills are the manifest's skills", JSON.stringify(result.manifest.skills.names) === JSON.stringify(Object.keys(skillFixture).sort()));
		check("staged skill bytes equal the source bytes", (await readFile(join(out, SKILLS_DIR, "alpha/references/note.md"), "utf8")) === skillFixture.alpha["references/note.md"]);
		check("top-level files beside the skill directories are not staged", !existsSync(join(out, SKILLS_DIR, "MIT-SKILLS.md")));
		check("the browser runtime is staged beside its skill", BROWSER_RUNTIME_FILES.every((file) => existsSync(join(out, BROWSER_RUNTIME_TARGET_DIR, file))));
		check("the browser runtime is hashed into the manifest", BROWSER_RUNTIME_FILES.every((file) => result.manifest.skills.files.some((entry) => entry.path === `${BROWSER_RUNTIME_TARGET_DIR}/${file}`)));
		check("the browser runtime records its omowright identity", result.manifest.sources.browserRuntime.dependency.commit === omowrightCommit && result.manifest.sources.browserRuntime.version === "0.0.0-fixture" && result.manifest.sources.browserRuntime.sourceDigest === "a".repeat(64));
		check("licenses retained", LICENSE_FILES.every((license) => existsSync(join(out, LICENSES_DIR, license.name))));
		check("no SUL licence text is staged", !existsSync(join(out, LICENSES_DIR, "SUL-1.0.md")) && !LICENSE_FILES.some((license) => /SUL/i.test(license.name)));
		check("versions.mhc is the payload build version, not Cargo.toml", result.manifest.versions.mhc === "2026.1.2-fixture");
		check("the senpi pin is recorded", result.manifest.pins.senpi === senpiPin);
		const second = await stage({ ...base, output: join(root, "out2") });
		check("manifest is deterministic across runs", JSON.stringify(result.manifest) === JSON.stringify(second.manifest));
		const overridden = await stage({ ...base, output: join(root, "build-version-override"), buildVersion: "v7.7.7" });
		check("--build-version overrides the payload probe and drops a leading v", overridden.manifest.versions.mhc === "7.7.7");
		await expectReject("an unusable build version is refused", stage({ ...base, output: join(root, "bad-build-version"), buildVersion: "../evil" }), "build version");

		// --- optional siblings: explicit flag, or found beside the binary ---
		const siblingSources = join(root, "sibling-sources");
		await mkdir(siblingSources, { recursive: true });
		for (const spec of SIBLINGS) await writeFile(join(siblingSources, spec.name), `#!/bin/sh\necho ${spec.name}\n`);
		const flagged = Object.fromEntries(SIBLINGS.map((spec) => [spec.option, join(siblingSources, spec.name)]));
		const allOut = join(root, "all-siblings");
		const all = await stage({ ...base, ...flagged, output: allOut });
		check("every sibling named by its flag is staged and declared", SIBLINGS.every((spec) => existsSync(join(allOut, spec.name)) && all.manifest.layout[spec.key] === spec.name && all.manifest.executables.some((entry) => entry.path === spec.name)));
		check("staged siblings carry the exec bit", (await Promise.all(SIBLINGS.map(async (spec) => ((await stat(join(allOut, spec.name))).mode & 0o111) !== 0))).every(Boolean));
		check("a stage with every sibling verifies", (await verify(allOut)).ok);
		for (const spec of SIBLINGS) {
			await expectReject(`a named ${spec.name} that is not a file is refused`, stage({ ...base, [spec.option]: join(root, "nope"), output: join(root, `missing-${spec.key}`) }), `${spec.flag} is not a regular file`);
		}
		await writeFile(join(binDir, SIBLINGS[0].name), "#!/bin/sh\necho beside\n");
		await writeFile(join(binDir, SIBLINGS[2].name), "#!/bin/sh\necho beside\n");
		const besideOut = join(root, "beside");
		const beside = await stage({ ...base, output: besideOut });
		check("siblings beside the binary are staged without a flag", beside.manifest.layout[SIBLINGS[0].key] === SIBLINGS[0].name && beside.manifest.layout[SIBLINGS[2].key] === SIBLINGS[2].name);
		check("siblings absent beside the binary stay absent", beside.manifest.layout[SIBLINGS[1].key] === undefined && beside.manifest.layout[SIBLINGS[3].key] === undefined && !existsSync(join(besideOut, SIBLINGS[1].name)));
		const flagWinsOut = join(root, "flag-wins");
		await stage({ ...base, [SIBLINGS[0].option]: join(siblingSources, SIBLINGS[0].name), output: flagWinsOut });
		check("an explicit flag wins over the file beside the binary", (await readFile(join(flagWinsOut, SIBLINGS[0].name), "utf8")) === `#!/bin/sh\necho ${SIBLINGS[0].name}\n`);
		await rm(join(binDir, SIBLINGS[0].name));
		await rm(join(binDir, SIBLINGS[2].name));

		// --- verifier negatives: the manifest is the package proof ---
		const negative = async (name, edit, includes) => {
			const dir = join(root, `neg-${results.length}`);
			await copyDir(allOut, dir);
			await edit(dir);
			const verdict = await verify(dir);
			const ok = !verdict.ok && verdict.errors.some((error) => error.includes(includes));
			if (!ok) console.log(`     got: ${JSON.stringify(verdict.errors)}`);
			check(name, ok);
		};
		const emptyDir = join(root, "empty-manifest");
		await mkdir(emptyDir, { recursive: true });
		await writeFile(join(emptyDir, MANIFEST_NAME), "{}\n");
		check("an empty manifest object is rejected", !(await verify(emptyDir)).ok);
		await negative("a manifest omitting the launcher is rejected", (dir) => editManifest(dir, (m) => { m.executables = m.executables.filter((entry) => entry.path !== LAUNCHER_NAME); }), "required executable missing");
		await negative("a declared sibling missing from the executables is rejected", (dir) => editManifest(dir, (m) => { m.executables = m.executables.filter((entry) => entry.path !== SIBLINGS[3].name); }), SIBLINGS[3].name);
		await negative("a sibling under a foreign layout name is rejected", (dir) => editManifest(dir, (m) => { m.layout.lspDaemon = "lsp-daemon"; }), "layout.lspDaemon");
		await negative("an unknown layout key is rejected", (dir) => editManifest(dir, (m) => { m.layout.schema = "omo.schema.json"; }), "layout.schema");
		await negative("a tampered sibling fails verification", (dir) => writeFile(join(dir, SIBLINGS[1].name), "#!/bin/sh\necho tampered\n"), "sha256 mismatch");
		await negative("a changed file mode fails verification", (dir) => chmod(join(dir, CLI_BINARY_NAME), 0o644), "mode mismatch");
		await negative("a tampered skill file fails verification", (dir) => writeFile(join(dir, SKILLS_DIR, "alpha/SKILL.md"), "tampered and longer than before\n"), "skills/alpha/SKILL.md");
		await negative("a declared skill without its SKILL.md is rejected", (dir) => editManifest(dir, (m) => { m.skills.files = m.skills.files.filter((entry) => entry.path !== "skills/alpha/SKILL.md"); }), "not in the hashed manifest");
		await negative("a dropped browser runtime file is rejected", (dir) => editManifest(dir, (m) => { m.skills.files = m.skills.files.filter((entry) => entry.path !== `${BROWSER_RUNTIME_TARGET_DIR}/index.js`); }), "browser runtime file is not in the hashed manifest");
		await negative("a browser skill without its runtime record is rejected", (dir) => editManifest(dir, (m) => { delete m.sources.browserRuntime; }), "sources.browserRuntime");
		await negative("an omitted license is rejected", (dir) => editManifest(dir, (m) => { m.licenses = m.licenses.filter((entry) => entry.path !== `${LICENSES_DIR}/MIT.txt`); }), "required license missing");
		await negative("omitted pins are rejected", (dir) => editManifest(dir, (m) => { delete m.pins; }), "pin senpi");
		await negative("an untracked file in the package is rejected", (dir) => writeFile(join(dir, "omo.schema.json"), "{}\n"), "file not listed in the manifest: omo.schema.json");
		await negative("an untracked SUL licence is rejected", (dir) => writeFile(join(dir, LICENSES_DIR, "SUL-1.0.md"), "sul\n"), "licenses/SUL-1.0.md");

		// --- refused inputs: checked before the destination is cleared ---
		const preserved = await readFile(join(out, CLI_BINARY_NAME));
		const refuseKeepsStage = async (name, mutate, restore, includes) => {
			await mutate();
			await expectReject(name, stage({ ...base, output: out, force: true }), includes);
			await restore();
		};
		const skillMd = join(skillsRoot, "alpha", "SKILL.md");
		await refuseKeepsStage("a skill file whose sha256 disagrees is refused", () => writeFile(skillMd, "# alpha!\n"), () => writeFile(skillMd, skillFixture.alpha["SKILL.md"]), "disagree with their recorded size/sha256");
		await refuseKeepsStage("a missing declared skill file is refused", () => rm(join(skillsRoot, "alpha/references/note.md")), () => writeFile(join(skillsRoot, "alpha/references/note.md"), skillFixture.alpha["references/note.md"]), "missing declared files: references/note.md");
		await refuseKeepsStage("an unlisted file in a skill is refused", () => writeFile(join(skillsRoot, "alpha/extra.md"), "x\n"), () => rm(join(skillsRoot, "alpha/extra.md")), "files not listed");
		await mkdir(join(skillsRoot, "runtime/browser/omowright"), { recursive: true });
		await writeFile(join(skillsRoot, "runtime/browser/omowright/index.js"), "local materialization\n");
		check("the gitignored materializer output under assets/skills is not a skill", (await stage({ ...base, output: join(root, "with-local-runtime") })).manifest.skills.names.every((name) => name !== "runtime"));
		await rm(join(skillsRoot, "runtime"), { recursive: true });
		await refuseKeepsStage("an unlisted skill directory is refused", () => mkdir(join(skillsRoot, "rogue"), { recursive: true }), () => rm(join(skillsRoot, "rogue"), { recursive: true }), "skill directory not listed");
		await refuseKeepsStage("a listed skill without a tree is refused", () => writeSkillsManifest({ skills: { ...declaredSkills, ghost: [{ path: "SKILL.md", bytes: 1, sha256: "0".repeat(64) }] } }), () => writeSkillsManifest(), "missing skill tree: assets/skills/ghost");
		await refuseKeepsStage("a missing skills manifest is refused", () => rm(join(repo, SKILLS_MANIFEST)), () => writeSkillsManifest(), "skills manifest not found");
		await refuseKeepsStage("a browserRuntime without a full omowright commit is refused", () => writeSkillsManifest({ browserRuntime: { ...browserRuntimeDecl, dependency: { name: "omowright", commit: "abc" } } }), () => writeSkillsManifest(), "full 40-hex commit");
		await refuseKeepsStage("a runtime from a different omowright commit is refused", () => writeRuntime({ commit: "f".repeat(40) }), () => writeRuntime(), "not the declared commit");
		await refuseKeepsStage("a runtime missing a file is refused", () => rm(join(runtimeDir, "page-bundle.js")), () => writeRuntime(), "missing page-bundle.js");
		await refuseKeepsStage("a missing license source is refused", () => rm(join(repo, "LICENSES/MIT.txt")), () => writeFile(join(repo, "LICENSES/MIT.txt"), "fixture for MIT.txt\n"), "license text missing");
		const pins = await readFile(join(repo, "PINS.md"), "utf8");
		await refuseKeepsStage("a short senpi pin is refused", () => writeFile(join(repo, "PINS.md"), pins.replace(senpiPin, "aaaaaaa")), () => writeFile(join(repo, "PINS.md"), pins), "senpi pin");
		check("refused inputs leave the existing stage intact", (await readFile(join(out, CLI_BINARY_NAME))).equals(preserved) && (await verify(out)).ok);
		await expectReject("a stage without --omowright-runtime is refused", stage({ binary: base.binary, repoRoot: repo, output: join(root, "no-runtime") }), "--omowright-runtime");
		await rm(join(binDir, LAUNCHER_NAME));
		await expectReject("a missing launcher is diagnosed", stage({ ...base, output: join(root, "missing-launcher") }), "launcher");
		check("no placeholder launcher is written", !existsSync(join(root, "missing-launcher", LAUNCHER_NAME)));
		await writeFile(join(binDir, LAUNCHER_NAME), "#!/bin/sh\necho launcher\n");
		await expectReject("a missing binary is refused", stage({ ...base, binary: join(binDir, "nope"), output: join(root, "no-binary") }), "not a regular file");
		await expectReject("a non-empty staging directory is refused", stage({ ...base, output: out }), "not empty");
		await expectReject("staging into the filesystem root is refused", stage({ ...base, output: "/" }), "refusing to stage");
		await expectReject("staging into the home directory is refused", stage({ ...base, output: homedir() }), "refusing to stage");
		const spaced = join(root, "with space", "out");
		check("a staging directory may contain spaces", (await stage({ ...base, output: spaced })).ok && (await verify(spaced)).ok);
		check("--force restages a non-empty directory", (await stage({ ...base, output: out, force: true })).ok && (await verify(out)).ok);
	} finally {
		await rm(root, { recursive: true, force: true });
	}
	const failed = results.filter((result) => !result.ok);
	console.log(`self-test: ${results.length - failed.length}/${results.length} passed`);
	return failed.length === 0;
}

// ---------- entry point ----------

async function main(argv) {
	const options = {};
	let verifyOnly = null;
	let self = false;
	const valueFlags = new Map([
		["--binary", "binary"],
		["--output", "output"],
		["--launcher-binary", "launcherBinary"],
		["--build-version", "buildVersion"],
		["--repo-root", "repoRoot"],
		["--omowright-runtime", "omowrightRuntime"],
		...SIBLINGS.map((spec) => [spec.flag, spec.option]),
	]);
	for (let cursor = 0; cursor < argv.length; cursor += 1) {
		const argument = argv[cursor];
		if (valueFlags.has(argument) || argument === "--verify-only") {
			cursor += 1;
			if (cursor >= argv.length) usageError(`${argument} needs a value`);
			if (argument === "--verify-only") verifyOnly = argv[cursor];
			else options[valueFlags.get(argument)] = argv[cursor];
		} else if (argument === "--force") options.force = true;
		else if (argument === "--self-test") self = true;
		else if (argument === "--help" || argument === "-h") {
			usage();
			process.exit(0);
		} else usageError(`unknown argument: ${argument}`);
	}

	if (self) process.exit((await selfTest()) ? 0 : 1);
	if (verifyOnly !== null) {
		const result = await verify(resolve(verifyOnly));
		if (result.ok) {
			console.log(`package-native: ${resolve(verifyOnly)} verified (${manifestFileCount(result.manifest)} files)`);
			process.exit(0);
		}
		console.error(`package-native: verification failed for ${resolve(verifyOnly)}`);
		for (const error of result.errors) console.error(`  ${error}`);
		process.exit(1);
	}
	if (options.binary === undefined || options.output === undefined) usageError("--binary and --output are required");
	const result = await stage(options);
	const siblings = SIBLINGS.filter((spec) => result.manifest.layout[spec.key] !== undefined).map((spec) => spec.name);
	console.log(`package-native: staged ${result.manifest.executables.length} executable(s) (siblings: ${siblings.length > 0 ? siblings.join(", ") : "none"}), ${result.manifest.skills.names.length} skill(s) (${result.manifest.skills.files.length} files) and ${result.manifest.licenses.length} license(s) into ${result.output}`);
	console.log(`package-native: manifest ${result.manifestPath}`);
}

try {
	await main(process.argv.slice(2));
} catch (error) {
	console.error(`package-native: ${error.message}`);
	process.exit(1);
}
