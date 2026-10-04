#!/usr/bin/env bun
// Package a native distribution: stage the built `mhc` binary beside its binary-sibling runtime
// resources, retain the licenses, and write a hash manifest that can be re-verified.
//
//   bun tools/package-native.mjs --binary <path/to/mhc> --output <staging-dir>
//   bun tools/package-native.mjs --verify-only <staging-dir>
//   bun tools/package-native.mjs --self-test
//
// The staged layout is exactly what the native runtime resolves at startup:
//
//   <staging>/mhc             the CLI binary
//   <staging>/ast-grep-mcp    the native ast-grep MCP server (AstGrepComponent entry: exe sibling)
//   <staging>/skills/<name>/  the pinned builtin skills (builtin_skills_root: exe parent + "skills")
//   <staging>/licenses/       retained license texts
//   <staging>/manifest.json   per-file sha256, sizes, modes, versions and source pins
//
// The ast-grep MCP entry is the workspace's own `ast-grep-mcp` binary (crate `maho-ast-grep-mcp`,
// bin `ast-grep-mcp`), which `cargo build --workspace --bins` emits beside `mhc`; nothing is
// downloaded and no placeholder is written. The staged skill set is the runtime's own
// `BUILTIN_SKILL_NAMES` (crates/omo/components/maho-omo-telemetry/src/product_identity.rs),
// cross-checked against the pinned oh-my-openagent list, then copied from the pinned skill roots
// (`packages/omo-senpi/skills` overrides `packages/shared-skills/skills`, as sync-skills.mjs does).
//
// Options:
//   --binary <path>        the built `mhc` executable (required unless --verify-only/--self-test)
//   --output <dir>         the staging directory to create (required unless --verify-only/--self-test)
//   --ast-grep-mcp <path>  native ast-grep MCP binary (default: <binary dir>/ast-grep-mcp)
//   --omo-root <dir>       pinned oh-my-openagent checkout (default: $OMO_SRC, else $HOME/code/oh-my-openagent)
//   --repo-root <dir>      this checkout (default: the tree holding Cargo.toml + crates/)
//   --force                overwrite an existing non-empty staging directory
//   --verify-only <dir>    verify an existing staging directory against its manifest
//   --self-test            run the deterministic fixture self-test
//
// The script never writes to a real install path: --output is mandatory and must be named
// explicitly, so nothing is staged unless an operator asks for it. Exit 0 = pass, 1 = failure,
// 2 = usage error.
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
const CLI_BINARY_NAME = "mhc";
const AST_GREP_MCP_NAME = "ast-grep-mcp";
const SCHEMA_VERSION = 1;

// License texts retained with the staged runtime: MIT covers the senpi-derived port, SUL-1.0 the
// omo-derived components, skills and the native ast-grep MCP server.
const LICENSE_FILES = [
	{ name: "MIT.txt", source: "LICENSES/MIT.txt" },
	{ name: "SUL-1.0.md", source: "LICENSES/SUL-1.0.md" },
];

// Pinned oh-my-openagent skill roots, in override order: the senpi-native skills win over the
// shared pool, exactly as packages/omo-senpi/plugin/scripts/sync-skills.mjs does.
const SKILL_SOURCE_ROOTS = ["packages/omo-senpi/skills", "packages/shared-skills/skills"];

// Mirrors sync-skills.mjs `shouldCopySkillSource`: build and test debris never reaches a distribution.
const IGNORED_DIR_NAMES = new Set([".git", ".omo", ".mypy_cache", ".pytest_cache", ".ruff_cache", "__pycache__", "node_modules"]);
const IGNORED_FILE_NAMES = new Set([".gitignore", ".npmignore", "pyrightconfig.json", "openai.yaml"]);

function usage() {
	console.log(`usage: bun tools/package-native.mjs --binary <mhc> --output <dir> [--ast-grep-mcp <path>] [--omo-root <dir>] [--repo-root <dir>] [--force]
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

function shouldSkipEntry(relPath, name, isDirectory) {
	if (isDirectory) {
		if (IGNORED_DIR_NAMES.has(name)) return true;
		const segments = relPath.split("/");
		const scriptsIndex = segments.lastIndexOf("scripts");
		return scriptsIndex !== -1 && segments[scriptsIndex + 1] === "tests";
	}
	if (IGNORED_FILE_NAMES.has(name)) return true;
	return name.endsWith(".test.ts") || name.endsWith(".pyc");
}

/** Every copied regular file under `root`, as sorted POSIX-style relative paths (symlinks are skipped). */
async function listFiles(root) {
	const files = [];
	async function visit(rel) {
		const entries = await readdir(join(root, rel), { withFileTypes: true });
		entries.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
		for (const entry of entries) {
			const child = rel === "" ? entry.name : `${rel}/${entry.name}`;
			if (entry.isDirectory()) {
				if (shouldSkipEntry(child, entry.name, true)) continue;
				await visit(child);
			} else if (entry.isFile() && !shouldSkipEntry(child, entry.name, false)) {
				files.push(child);
			}
		}
	}
	await visit("");
	return files;
}

/** Copies the filtered tree, preserving each file's mode; returns the copied relative paths. */
async function copyTree(sourceRoot, destRoot) {
	const files = await listFiles(sourceRoot);
	for (const rel of files) {
		const source = join(sourceRoot, rel);
		const destination = join(destRoot, rel);
		await mkdir(dirname(destination), { recursive: true });
		await copyFile(source, destination);
		await chmod(destination, (await stat(source)).mode & 0o777);
	}
	return files;
}

async function copyExecutable(source, destination) {
	await copyFile(source, destination);
	await chmod(destination, 0o755);
}

// ---------- pinned-source readers ----------

async function parseNames(path, pattern) {
	const match = (await readFile(path, "utf8")).match(pattern);
	if (match === null) return [];
	return [...match[1].matchAll(/"([^"]+)"/g)].map((entry) => entry[1]);
}

/**
 * The staged skill set is the runtime's own contract, not a guessed list: read
 * `BUILTIN_SKILL_NAMES` from the native telemetry crate and require the pinned oh-my-openagent
 * checkout to agree, so a drifted pin fails loudly instead of staging a short set.
 */
export async function parseBuiltinSkillNames(repoRoot, omoRoot) {
	const rustPath = join(repoRoot, "crates/omo/components/maho-omo-telemetry/src/product_identity.rs");
	const tsPath = join(omoRoot, "packages/omo-senpi/src/components/telemetry/product-identity.ts");
	const rustNames = await parseNames(rustPath, /BUILTIN_SKILL_NAMES\s*:\s*&\[&str\]\s*=\s*&\[([\s\S]*?)\]\s*;/);
	const tsNames = await parseNames(tsPath, /BUILTIN_SKILL_NAMES\s*=\s*Object\.freeze\(\[([\s\S]*?)\]\s*as const\)/);
	if (rustNames.length === 0) throw new Error(`could not read BUILTIN_SKILL_NAMES from ${rustPath}`);
	if (tsNames.length === 0) throw new Error(`could not read BUILTIN_SKILL_NAMES from ${tsPath}`);
	const tsSet = new Set(tsNames);
	const rustSet = new Set(rustNames);
	const onlyRust = rustNames.filter((name) => !tsSet.has(name));
	const onlyTs = tsNames.filter((name) => !rustSet.has(name));
	if (onlyRust.length > 0 || onlyTs.length > 0) {
		throw new Error(`builtin skill list drifted between the native runtime and the pinned checkout: native-only=[${onlyRust.join(", ")}] omo-only=[${onlyTs.join(", ")}]`);
	}
	return [...new Set(rustNames)].sort();
}

async function parseWorkspaceVersion(repoRoot) {
	const cargo = await readFile(join(repoRoot, "Cargo.toml"), "utf8");
	const match = cargo.match(/\[workspace\.package\][\s\S]*?\nversion\s*=\s*"([^"]+)"/);
	return match === null ? null : match[1];
}

async function parsePackageVersion(path) {
	if (!existsSync(path)) return null;
	try {
		const pkg = JSON.parse(await readFile(path, "utf8"));
		return typeof pkg.version === "string" ? pkg.version : null;
	} catch {
		return null;
	}
}

/** Reads the expected source pins from PINS.md (`| senpi | short | full | location |`). */
async function parsePins(repoRoot) {
	const pins = { senpi: null, omo: null };
	const path = join(repoRoot, "PINS.md");
	if (!existsSync(path)) return pins;
	for (const line of (await readFile(path, "utf8")).split("\n")) {
		const cells = line.split("|").map((cell) => cell.trim());
		if (cells.length < 5) continue;
		if (cells[1] === "senpi") pins.senpi = cells[3];
		if (cells[1] === "oh-my-openagent") pins.omo = cells[3];
	}
	return pins;
}

/** The checkout's actual commit, for provenance; absent (null) when it is not a git checkout. */
async function gitHead(directory) {
	try {
		const process_ = Bun.spawn(["git", "-C", directory, "rev-parse", "HEAD"], { stdout: "pipe", stderr: "ignore" });
		if ((await process_.exited) !== 0) return null;
		const sha = (await new Response(process_.stdout).text()).trim();
		return /^[0-9a-f]{40}$/.test(sha) ? sha : null;
	} catch {
		return null;
	}
}

// ---------- path resolution ----------

function resolveOmoRoot(explicit) {
	const candidates = [explicit, process.env.OMO_SRC, process.env.OMO_PIN_ROOT, join(homedir(), "code", "oh-my-openagent"), "/Users/indo/code/oh-my-openagent"];
	for (const candidate of candidates) {
		if (!candidate) continue;
		if (existsSync(join(candidate, "packages/omo-senpi")) && existsSync(join(candidate, "packages/shared-skills"))) return resolve(candidate);
	}
	throw new Error(`oh-my-openagent checkout not found (looked at: ${candidates.filter(Boolean).join(", ")}); pass --omo-root or set OMO_SRC`);
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

/** Resolves each builtin skill name to a source directory, senpi-native root first. */
async function resolveSkillSources(omoRoot, names) {
	const resolved = new Map();
	for (const root of SKILL_SOURCE_ROOTS) {
		for (const name of names) {
			if (resolved.has(name)) continue;
			const directory = join(omoRoot, root, name);
			if (existsSync(join(directory, "SKILL.md"))) resolved.set(name, { directory, root });
		}
	}
	const missing = names.filter((name) => !resolved.has(name));
	if (missing.length > 0) throw new Error(`builtin skills missing from the pinned checkout: ${missing.join(", ")}`);
	return resolved;
}

async function fileEntry(output, relPath, source) {
	const absolute = join(output, relPath);
	const info = await stat(absolute);
	return { path: relPath, source, sha256: await sha256(absolute), size: info.size, mode: modeString(info.mode) };
}

// ---------- staging ----------

export async function stage(options) {
	const binary = resolve(options.binary);
	if (!existsSync(binary) || !(await stat(binary)).isFile()) throw new Error(`--binary is not a regular file: ${binary}`);
	const astGrepMcp = resolve(options.astGrepMcp ?? join(dirname(binary), AST_GREP_MCP_NAME));
	if (!existsSync(astGrepMcp) || !(await stat(astGrepMcp)).isFile()) {
		throw new Error(`native ast-grep MCP server not found at ${astGrepMcp}: build the workspace bins (cargo build --workspace --bins) so ${AST_GREP_MCP_NAME} sits beside ${CLI_BINARY_NAME}, or pass --ast-grep-mcp`);
	}
	// Refuse an unsafe or non-empty destination before anything is written. The destination is not
	// removed here: every input is validated first so a rejected run leaves an existing stage intact.
	const output = resolve(options.output);
	if (output === sep || output === resolve(homedir())) throw new Error(`refusing to stage into ${output}`);
	const outputExists = existsSync(output);
	if (outputExists && (await readdir(output)).length > 0 && !options.force) throw new Error(`staging directory is not empty: ${output} (pass --force to overwrite)`);

	// Resolve and validate every input before the destination is cleared, so a bad checkout, a
	// missing skill or a missing license text refuses the run without destroying an existing stage.
	// Only once the inputs are known good is the destination removed, so a failed run never leaves a
	// half-written directory behind.
	const omoRoot = resolveOmoRoot(options.omoRoot);
	const repoRoot = resolveRepoRoot(options.repoRoot);
	const skillNames = await parseBuiltinSkillNames(repoRoot, omoRoot);
	const sources = await resolveSkillSources(omoRoot, skillNames);
	const licenseSources = new Map();
	for (const license of LICENSE_FILES) {
		const source = join(repoRoot, license.source);
		if (!existsSync(source)) throw new Error(`license text missing from the checkout: ${license.source}`);
		licenseSources.set(license.name, source);
	}

	if (outputExists) await rm(output, { recursive: true, force: true });

	await mkdir(join(output, SKILLS_DIR), { recursive: true });
	await mkdir(join(output, LICENSES_DIR), { recursive: true });

	const executables = [];
	await copyExecutable(binary, join(output, CLI_BINARY_NAME));
	executables.push(await fileEntry(output, CLI_BINARY_NAME, binary));
	await copyExecutable(astGrepMcp, join(output, AST_GREP_MCP_NAME));
	executables.push(await fileEntry(output, AST_GREP_MCP_NAME, astGrepMcp));

	const licenses = [];
	for (const license of LICENSE_FILES) {
		const relPath = `${LICENSES_DIR}/${license.name}`;
		await copyFile(licenseSources.get(license.name), join(output, relPath));
		licenses.push(await fileEntry(output, relPath, license.source));
	}

	const stagedNames = [...sources.keys()].sort();
	const skillFiles = [];
	for (const name of stagedNames) {
		const { directory, root } = sources.get(name);
		for (const rel of await copyTree(directory, join(output, SKILLS_DIR, name))) {
			skillFiles.push(await fileEntry(output, `${SKILLS_DIR}/${name}/${rel}`, `${root}/${name}/${rel}`));
		}
	}

	const workspaceVersion = await parseWorkspaceVersion(repoRoot);
	const manifest = {
		schemaVersion: SCHEMA_VERSION,
		generator: "tools/package-native.mjs",
		layout: { binary: CLI_BINARY_NAME, astGrepMcp: AST_GREP_MCP_NAME, skills: SKILLS_DIR, licenses: LICENSES_DIR, manifest: MANIFEST_NAME },
		versions: {
			mhc: workspaceVersion,
			astGrepMcp: workspaceVersion,
			omoSenpi: await parsePackageVersion(join(omoRoot, "packages/omo-senpi/package.json")),
			sharedSkills: await parsePackageVersion(join(omoRoot, "packages/shared-skills/package.json")),
		},
		pins: await parsePins(repoRoot),
		sources: { omoRoot, omoCommit: await gitHead(omoRoot) },
		executables,
		licenses,
		skills: { names: stagedNames, files: skillFiles },
	};
	await writeFile(join(output, MANIFEST_NAME), `${JSON.stringify(manifest, null, 2)}\n`, "utf8");

	const verification = await verify(output);
	if (!verification.ok) throw new Error(`staged manifest failed self-verification:\n  ${verification.errors.join("\n  ")}`);
	return { ok: true, output, manifestPath: join(output, MANIFEST_NAME), manifest };
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
	const errors = [];
	if (manifest === null || typeof manifest !== "object" || Array.isArray(manifest)) {
		return { ok: false, errors: [`${MANIFEST_NAME} is not a manifest object`], manifest };
	}
	// The manifest is the package proof, so verification requires its concrete identity: the exact
	// schema version and the exact layout the runtime resolves. An empty or foreign object must not
	// pass just because its optional arrays are absent.
	if (manifest.schemaVersion !== SCHEMA_VERSION) errors.push(`unsupported schemaVersion: ${JSON.stringify(manifest.schemaVersion)} (expected ${SCHEMA_VERSION})`);
	const expectedLayout = { binary: CLI_BINARY_NAME, astGrepMcp: AST_GREP_MCP_NAME, skills: SKILLS_DIR, licenses: LICENSES_DIR, manifest: MANIFEST_NAME };
	for (const [key, value] of Object.entries(expectedLayout)) {
		if (manifest.layout?.[key] !== value) errors.push(`layout.${key} must be ${JSON.stringify(value)} (found ${JSON.stringify(manifest.layout?.[key])})`);
	}
	const check = async (entry) => {
		if (entry === null || typeof entry !== "object" || typeof entry.path !== "string") {
			errors.push("manifest entry without a path");
			return;
		}
		const absolute = join(directory, entry.path);
		if (!existsSync(absolute)) {
			errors.push(`missing: ${entry.path}`);
			return;
		}
		const info = await stat(absolute);
		if (!info.isFile()) {
			errors.push(`not a regular file: ${entry.path}`);
			return;
		}
		if (info.size !== entry.size) errors.push(`size mismatch: ${entry.path} (${info.size} != ${entry.size})`);
		if ((await sha256(absolute)) !== entry.sha256) errors.push(`sha256 mismatch: ${entry.path}`);
		if (modeString(info.mode) !== entry.mode) errors.push(`mode mismatch: ${entry.path} (${modeString(info.mode)} != ${entry.mode})`);
	};
	const executables = Array.isArray(manifest.executables) ? manifest.executables : [];
	const licenses = Array.isArray(manifest.licenses) ? manifest.licenses : [];
	const skillFiles = Array.isArray(manifest.skills?.files) ? manifest.skills.files : [];
	for (const entry of executables) await check(entry);
	for (const entry of licenses) await check(entry);
	for (const entry of skillFiles) await check(entry);
	// Required entries: the two executables the runtime resolves as binary siblings and both
	// retained license texts must be present, so a manifest cannot omit a shipped asset.
	for (const required of [CLI_BINARY_NAME, AST_GREP_MCP_NAME]) {
		if (!executables.some((entry) => entry?.path === required)) errors.push(`required executable missing from the manifest: ${required}`);
	}
	for (const license of LICENSE_FILES) {
		const relPath = `${LICENSES_DIR}/${license.name}`;
		if (!licenses.some((entry) => entry?.path === relPath)) errors.push(`required license missing from the manifest: ${relPath}`);
	}
	const skillNames = Array.isArray(manifest.skills?.names) ? manifest.skills.names : [];
	if (skillNames.length === 0) errors.push("manifest declares no builtin skills");
	if (skillFiles.length === 0) errors.push("manifest declares no staged skill files");
	const skillsDir = manifest.layout?.skills ?? SKILLS_DIR;
	for (const name of skillNames) {
		if (!existsSync(join(directory, skillsDir, name, "SKILL.md"))) errors.push(`missing skill: ${name}`);
	}
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
			check(name, typeof includes === "string" ? String(error.message).includes(includes) : true);
		}
	};
	try {
		const repo = join(root, "repo");
		const omo = join(root, "omo");
		const binDir = join(root, "bin");
		const names = ["alpha", "beta", "gamma"];

		await mkdir(join(repo, "crates/omo/components/maho-omo-telemetry/src"), { recursive: true });
		await mkdir(join(repo, "LICENSES"), { recursive: true });
		await writeFile(join(repo, "Cargo.toml"), `[workspace]\n\n[workspace.package]\nversion = "9.9.9"\n`);
		await writeFile(
			join(repo, "PINS.md"),
			`# Source pins\n\n| Source | Short | Full commit | Location |\n| --- | --- | --- | --- |\n| senpi | aaaaaaa | aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa | /Users/indo/code/senpi |\n| oh-my-openagent | bbbbbbb | bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb | /Users/indo/code/oh-my-openagent |\n`,
		);
		await writeFile(join(repo, "crates/omo/components/maho-omo-telemetry/src/product_identity.rs"), `pub const BUILTIN_SKILL_NAMES: &[&str] = &[${names.map((name) => `"${name}"`).join(", ")}];\n`);
		await writeFile(join(repo, "LICENSES/MIT.txt"), "MIT fixture\n");
		await writeFile(join(repo, "LICENSES/SUL-1.0.md"), "SUL fixture\n");

		await mkdir(join(omo, "packages/omo-senpi/src/components/telemetry"), { recursive: true });
		await mkdir(join(omo, "packages/omo-senpi/skills"), { recursive: true });
		await mkdir(join(omo, "packages/shared-skills/skills"), { recursive: true });
		await writeFile(join(omo, "packages/omo-senpi/package.json"), JSON.stringify({ name: "@oh-my-opencode/omo-senpi", version: "5.0.0-beta.9" }, null, 2));
		await writeFile(join(omo, "packages/shared-skills/package.json"), JSON.stringify({ name: "@oh-my-opencode/shared-skills", version: "0.1.0" }, null, 2));
		await writeFile(join(omo, "packages/omo-senpi/src/components/telemetry/product-identity.ts"), `export const BUILTIN_SKILL_NAMES = Object.freeze([\n  ${names.map((name) => `"${name}"`).join(", ")},\n] as const)\n`);

		// alpha lives in both roots (the native override wins), beta only in the shared pool,
		// gamma only in the native root.
		const putSkill = async (base, name, marker) => {
			await mkdir(join(base, name, "references"), { recursive: true });
			await writeFile(join(base, name, "SKILL.md"), `# ${name}\n${marker}\n`);
			await writeFile(join(base, name, "references/note.md"), `note ${marker}\n`);
			await writeFile(join(base, name, ".gitignore"), "ignored\n");
			await writeFile(join(base, name, "junk.test.ts"), "ignored\n");
		};
		await putSkill(join(omo, "packages/omo-senpi/skills"), "alpha", "native");
		await putSkill(join(omo, "packages/omo-senpi/skills"), "gamma", "native");
		await putSkill(join(omo, "packages/shared-skills/skills"), "alpha", "shared");
		await putSkill(join(omo, "packages/shared-skills/skills"), "beta", "shared");

		await mkdir(binDir, { recursive: true });
		await writeFile(join(binDir, "mhc"), "#!/bin/sh\necho mhc\n");
		await writeFile(join(binDir, "ast-grep-mcp"), "#!/bin/sh\necho ast-grep-mcp\n");

		const out = join(root, "out");
		const result = await stage({ binary: join(binDir, "mhc"), output: out, omoRoot: omo, repoRoot: repo });
		check("stage writes a schema-versioned manifest", result.ok && result.manifest.schemaVersion === 1);
		check("mhc staged beside a skills directory", existsSync(join(out, "mhc")) && existsSync(join(out, "skills/alpha/SKILL.md")));
		check("ast-grep-mcp staged as a binary sibling", existsSync(join(out, "ast-grep-mcp")));
		check("staged skills are the sorted builtin set", JSON.stringify(result.manifest.skills.names) === JSON.stringify(["alpha", "beta", "gamma"]));
		check("native skill overrides the shared pool", (await readFile(join(out, "skills/alpha/SKILL.md"), "utf8")).includes("native"));
		check("shared-only skill staged", (await readFile(join(out, "skills/beta/SKILL.md"), "utf8")).includes("shared"));
		check("skill debris excluded", !existsSync(join(out, "skills/alpha/.gitignore")) && !existsSync(join(out, "skills/alpha/junk.test.ts")));
		check("licenses retained", existsSync(join(out, "licenses/MIT.txt")) && existsSync(join(out, "licenses/SUL-1.0.md")));
		check("versions recorded", result.manifest.versions.mhc === "9.9.9" && result.manifest.versions.omoSenpi === "5.0.0-beta.9");
		check("source pins recorded", result.manifest.pins.omo === "b".repeat(40) && result.manifest.pins.senpi === "a".repeat(40));
		check("staged executables carry the exec bit", ((await stat(join(out, "mhc"))).mode & 0o111) !== 0 && ((await stat(join(out, "ast-grep-mcp"))).mode & 0o111) !== 0);
		check("manifest verifies the staged tree", (await verify(out)).ok);

		// Verifier negatives: the manifest is the package proof, so an empty object, an omitted
		// required executable, or a file whose mode disagrees with the manifest must all fail.
		const emptyManifestDir = join(root, "empty-manifest");
		await mkdir(emptyManifestDir, { recursive: true });
		await writeFile(join(emptyManifestDir, MANIFEST_NAME), "{}\n");
		const emptyManifest = await verify(emptyManifestDir);
		check("empty manifest object is rejected", !emptyManifest.ok && emptyManifest.errors.some((error) => error.includes("schemaVersion")));

		const omittedExecutableDir = join(root, "omitted-executable");
		await copyTree(out, omittedExecutableDir);
		const omittedManifest = JSON.parse(await readFile(join(omittedExecutableDir, MANIFEST_NAME), "utf8"));
		omittedManifest.executables = omittedManifest.executables.filter((entry) => entry.path !== AST_GREP_MCP_NAME);
		await writeFile(join(omittedExecutableDir, MANIFEST_NAME), `${JSON.stringify(omittedManifest, null, 2)}\n`);
		const omittedExecutable = await verify(omittedExecutableDir);
		check("manifest omitting a required executable is rejected", !omittedExecutable.ok && omittedExecutable.errors.some((error) => error.includes("required executable missing") && error.includes(AST_GREP_MCP_NAME)));

		const changedModeDir = join(root, "changed-mode");
		await copyTree(out, changedModeDir);
		await chmod(join(changedModeDir, CLI_BINARY_NAME), 0o644);
		const changedMode = await verify(changedModeDir);
		check("changed file mode fails verification", !changedMode.ok && changedMode.errors.some((error) => error.includes("mode mismatch") && error.includes(CLI_BINARY_NAME)));

		const second = await stage({ binary: join(binDir, "mhc"), output: join(root, "out2"), omoRoot: omo, repoRoot: repo });
		check("manifest is deterministic across runs", JSON.stringify(result.manifest) === JSON.stringify(second.manifest));

		const spaced = join(root, "with space", "out");
		check("staging directory may contain spaces", (await stage({ binary: join(binDir, "mhc"), output: spaced, omoRoot: omo, repoRoot: repo })).ok && (await verify(spaced)).ok);
		check("--force restages a non-empty directory", (await stage({ binary: join(binDir, "mhc"), output: out, omoRoot: omo, repoRoot: repo, force: true })).ok && (await verify(out)).ok);

		await writeFile(join(out, "skills/beta/SKILL.md"), "tampered and longer than the staged file\n");
		const tampered = await verify(out);
		check("tampered asset fails verification", !tampered.ok && tampered.errors.some((error) => error.includes("skills/beta/SKILL.md")));
		check("tampered asset reports a size or hash mismatch", tampered.errors.some((error) => error.includes("mismatch")));

		await rm(join(binDir, "ast-grep-mcp"));
		const missingHelper = join(root, "missing-helper");
		await expectReject("missing ast-grep-mcp helper is diagnosed", stage({ binary: join(binDir, "mhc"), output: missingHelper, omoRoot: omo, repoRoot: repo }), "ast-grep-mcp");
		check("no placeholder helper is written", !existsSync(join(missingHelper, "ast-grep-mcp")));
		await writeFile(join(binDir, "ast-grep-mcp"), "#!/bin/sh\necho ast-grep-mcp\n");

		await writeFile(join(repo, "crates/omo/components/maho-omo-telemetry/src/product_identity.rs"), `pub const BUILTIN_SKILL_NAMES: &[&str] = &["alpha", "missing-skill"];\n`);
		await writeFile(join(omo, "packages/omo-senpi/src/components/telemetry/product-identity.ts"), `export const BUILTIN_SKILL_NAMES = Object.freeze([\n  "alpha", "missing-skill",\n] as const)\n`);
		await expectReject("builtin skill absent from both roots is diagnosed", stage({ binary: join(binDir, "mhc"), output: join(root, "missing-skill"), omoRoot: omo, repoRoot: repo }), "missing-skill");

		await writeFile(join(omo, "packages/omo-senpi/src/components/telemetry/product-identity.ts"), `export const BUILTIN_SKILL_NAMES = Object.freeze([\n  "alpha",\n] as const)\n`);
		await expectReject("drifted builtin skill lists are diagnosed", stage({ binary: join(binDir, "mhc"), output: join(root, "drift"), omoRoot: omo, repoRoot: repo }), "drifted");

		await expectReject("non-empty staging directory is refused", stage({ binary: join(binDir, "mhc"), output: out, omoRoot: omo, repoRoot: repo }), "not empty");
		await expectReject("staging into the filesystem root is refused", stage({ binary: join(binDir, "mhc"), output: "/", omoRoot: omo, repoRoot: repo }), "refusing to stage");
		await expectReject("staging into the home directory is refused", stage({ binary: join(binDir, "mhc"), output: homedir(), omoRoot: omo, repoRoot: repo }), "refusing to stage");
		await expectReject("missing binary is refused", stage({ binary: join(binDir, "nope"), output: join(root, "no-binary"), omoRoot: omo, repoRoot: repo }), "not a regular file");

		// Rejected input must not destroy an existing stage: the skill lists are still drifted above,
		// so a --force restage over the existing `out` must be refused and leave the staged tree intact.
		const preserved = await readFile(join(out, CLI_BINARY_NAME));
		await expectReject("rejected input preserves the existing stage", stage({ binary: join(binDir, "mhc"), output: out, omoRoot: omo, repoRoot: repo, force: true }), "drifted");
		check("existing stage survives a rejected restage", existsSync(join(out, CLI_BINARY_NAME)) && (await readFile(join(out, CLI_BINARY_NAME))).equals(preserved));
	} finally {
		await rm(root, { recursive: true, force: true });
	}
	const failed = results.filter((result) => !result.ok);
	console.log(`self-test: ${results.length - failed.length}/${results.length} passed`);
	return failed.length === 0;
}

// ---------- entry point ----------

async function main(argv) {
	let binary = null;
	let output = null;
	let astGrepMcp = null;
	let omoRoot = null;
	let repoRoot = null;
	let verifyOnly = null;
	let force = false;
	let self = false;
	let cursor = 0;
	const next = (flag) => {
		cursor += 1;
		if (cursor >= argv.length) usageError(`${flag} needs a value`);
		return argv[cursor];
	};
	for (cursor = 0; cursor < argv.length; cursor += 1) {
		const argument = argv[cursor];
		if (argument === "--binary") binary = next(argument);
		else if (argument === "--output") output = next(argument);
		else if (argument === "--ast-grep-mcp") astGrepMcp = next(argument);
		else if (argument === "--omo-root") omoRoot = next(argument);
		else if (argument === "--repo-root") repoRoot = next(argument);
		else if (argument === "--verify-only") verifyOnly = next(argument);
		else if (argument === "--force") force = true;
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
	if (binary === null || output === null) usageError("--binary and --output are required");
	const result = await stage({ binary, output, astGrepMcp, omoRoot, repoRoot, force });
	console.log(`package-native: staged ${result.manifest.executables.length} executable(s), ${result.manifest.skills.names.length} skill(s) (${result.manifest.skills.files.length} files) and ${result.manifest.licenses.length} license(s) into ${result.output}`);
	console.log(`package-native: manifest ${result.manifestPath}`);
}

try {
	await main(process.argv.slice(2));
} catch (error) {
	console.error(`package-native: ${error.message}`);
	process.exit(1);
}
