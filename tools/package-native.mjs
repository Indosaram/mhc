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
//   <staging>/omo-git-bash    the git-bash MCP sibling (required by --skill-source latest, optional in pinned)
//   <staging>/skills/<name>/  the builtin skills (builtin_skills_root: exe parent + "skills")
//   <staging>/skills/browser/runtime/omowright/  the browser skill's materialized engine runtime
//                            (latest only: staged from --omowright-runtime, never committed)
//   <staging>/omo.schema.json the published config JSON Schema (native projection of the config schema)
//   <staging>/licenses/       retained license texts
//   <staging>/manifest.json   per-file sha256, sizes, modes, versions, pins and the staged skill source
//
// The ast-grep MCP entry is the workspace's own `ast-grep-mcp` binary (crate `maho-ast-grep-mcp`,
// bin `ast-grep-mcp`), which `cargo build --workspace --bins` emits beside `mhc`; nothing is
// downloaded and no placeholder is written.
//
// The staged skill set comes from one explicit source, never a mixture:
//
//   pinned (default)  the runtime's own `BUILTIN_SKILL_NAMES`
//                     (crates/omo/components/maho-omo-telemetry/src/product_identity.rs),
//                     cross-checked against the pinned oh-my-openagent list, then copied from the
//                     pinned skill roots (`packages/omo-senpi/skills` overrides
//                     `packages/shared-skills/skills`, as sync-skills.mjs does).
//   latest            the repository-owned overlay `assets/omo-latest` (or --latest-assets): it
//                     declares its own latest commit, its complete skill selection and each owned
//                     skill's exact file list. The pinned checkout is NOT consulted for skills, so a
//                     host path cannot silently contribute latest assets; PINS.md is never updated.
//
// The git-bash MCP sibling is staged from --git-bash-binary, the single git-bash flag (no aliases).
// In the selected product staging (`--skill-source latest`) it is REQUIRED: an absent sibling refuses
// the selected delivery, and verify() enforces the declared sibling in the staged manifest. In the
// pinned baseline it stays optional (absent means no sibling and no placeholder). Either way its
// source path and sha256 are recorded.
//
// The browser skill's engine runtime (upstream `skills/browser/runtime/omowright/`: the bundled
// omowright entry, its page bundle and the materializer manifest) is build-materialized and never
// committed, so the selected latest staging REQUIRES --omowright-runtime <dir> and records the
// runtime's own manifest identity (omowright version, source digest, immutable commit) plus the
// sha256 of every staged runtime file. A content-only `browser` tree is refused: without the
// runtime, `skills/browser/scripts/omowright.mjs` throws "omowright is not staged in this skill".
// The pinned baseline stages no browser runtime (its pinned pool has no `browser` skill).
//
// Options:
//   --binary <path>        the built `mhc` executable (required unless --verify-only/--self-test)
//   --output <dir>         the staging directory to create (required unless --verify-only/--self-test)
//   --ast-grep-mcp <path>  native ast-grep MCP binary (default: <binary dir>/ast-grep-mcp)
//   --omo-root <dir>       pinned oh-my-openagent checkout (default: $OMO_SRC, else $HOME/code/oh-my-openagent); unused by --skill-source latest
//   --repo-root <dir>      this checkout (default: the tree holding Cargo.toml + crates/)
//   --skill-source <mode>  skills source: pinned (default) or latest (repository-owned overlay)
//   --latest-assets <dir>  repository-owned latest overlay root (default: <repo-root>/assets/omo-latest)
//   --git-bash-binary <p>  git-bash MCP sibling to stage as `omo-git-bash` (required by --skill-source latest)
//   --omowright-runtime <p>  materialized browser engine runtime directory to stage (required by --skill-source latest)
//   --schema <path>        published config JSON Schema to stage (default: <repo-root>/assets/omo.schema.json)
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

// The published config JSON Schema. `assets/omo.schema.json` is the native projection of the
// harness-neutral config schema (generated by `tools/omo-schema.mjs`, which runs the crate's
// `omo_schema` example); it is staged beside the binary so an installed distribution ships the
// schema without needing cargo. `--schema` overrides the source path for a build that regenerates it.
const SCHEMA_ASSET_NAME = "omo.schema.json";
const SCHEMA_ASSET_SOURCE = `assets/${SCHEMA_ASSET_NAME}`;

// Binary sibling: the git-bash MCP server. Upstream ships it as `packages/git-bash-mcp` with the bin
// key `omo-git-bash`, and the git-bash crate owner returned that same binary name natively. It is
// staged only from `--git-bash-binary` (the single git-bash flag, no aliases): the selected product
// staging (`--skill-source latest`) requires it, the pinned baseline stages it optionally, and its
// source path + sha256 are recorded in the manifest either way (no guess, no placeholder).
const GIT_BASH_MCP_NAME = "omo-git-bash";

// The browser skill's engine runtime. Upstream materializes it at build/prepack time from the root
// `omowright` devDependency into `skills/browser/runtime/omowright/` (gitignored, shipped through the
// skill's .npmignore); the repository holds no committed copy. The selected latest staging therefore
// takes it from an explicit `--omowright-runtime <dir>` and refuses a content-only browser tree.
const BROWSER_SKILL_NAME = "browser";
const BROWSER_RUNTIME_REL_DIR = "runtime/omowright";
const BROWSER_RUNTIME_MANIFEST = "manifest.json";
const BROWSER_RUNTIME_FILES = ["index.js", "page-bundle.js", BROWSER_RUNTIME_MANIFEST];
const BROWSER_RUNTIME_TARGET_DIR = `${SKILLS_DIR}/${BROWSER_SKILL_NAME}/${BROWSER_RUNTIME_REL_DIR}`;

// The repository-owned latest asset overlay, relative to the repository root. It is read only by
// `--skill-source latest`; the pinned checkout and this overlay are never mixed.
const LATEST_ASSETS_DIR = "assets/omo-latest";
const LATEST_MANIFEST_NAME = "manifest.json";
const SKILL_SOURCE_MODES = ["pinned", "latest"];
const DEFAULT_SKILL_SOURCE = "pinned";

// Provenance the manifest must record: the two upstream revisions, each a full 40-hex commit read
// from PINS.md at stage time. verify() enforces these keys and the commit format from the manifest
// alone, so a staged directory verifies without resolving any host path.
const PIN_KEYS = ["senpi", "omo"];
const COMMIT_PATTERN = /^[0-9a-f]{40}$/;

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
	console.log(`usage: bun tools/package-native.mjs --binary <mhc> --output <dir> [--ast-grep-mcp <path>] [--omo-root <dir>] [--repo-root <dir>] [--schema <path>] [--force]
       bun tools/package-native.mjs --binary <mhc> --output <dir> --skill-source latest --git-bash-binary <path> --omowright-runtime <dir> [--latest-assets <dir>]
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

/**
 * Provenance errors for a set of pins, the recorded staging source and the staged skill source. Both
 * expected pins must be full commits in every mode. `pinned` additionally requires the recorded
 * checkout commit to equal the OMO pin. `latest` instead requires the repository-owned overlay's
 * declared latest commit to be a full commit that is NOT the pinned omo commit, plus the overlay
 * root and manifest names: the pin must not be mistaken for the latest source. Shared by stage
 * (checked before the destination is cleared, so a bad pin cannot destroy an existing stage) and
 * verify (checked from the manifest alone, without resolving any host path).
 */
function provenanceErrors(pins, sources, skillsSource) {
	const errors = [];
	for (const key of PIN_KEYS) {
		const pin = pins?.[key];
		if (typeof pin !== "string" || !COMMIT_PATTERN.test(pin)) errors.push(`pin ${key} must be a full 40-hex commit (found ${JSON.stringify(pin)})`);
	}
	if (skillsSource === "pinned") {
		const omoCommit = sources?.omoCommit;
		if (typeof omoCommit !== "string" || !COMMIT_PATTERN.test(omoCommit)) {
			errors.push(`checkout commit (sources.omoCommit) must be a full 40-hex commit (found ${JSON.stringify(omoCommit)})`);
		} else if (typeof pins?.omo === "string" && omoCommit !== pins.omo) {
			errors.push(`checkout commit ${omoCommit} does not match the omo pin ${pins.omo}`);
		}
	} else if (skillsSource === "latest") {
		const latest = sources?.latestAssets;
		if (latest === null || typeof latest !== "object") {
			errors.push("sources.latestAssets must record the repository-owned latest asset overlay");
		} else {
			if (typeof latest.commit !== "string" || !COMMIT_PATTERN.test(latest.commit)) {
				errors.push(`latestAssets.commit must be a full 40-hex commit (found ${JSON.stringify(latest.commit)})`);
			} else if (typeof pins?.omo === "string" && latest.commit === pins.omo) {
				errors.push(`latestAssets.commit equals the pinned omo commit ${pins.omo}: the overlay must record the latest source, not the pin`);
			}
			if (typeof latest.root !== "string" || latest.root.length === 0) errors.push("latestAssets.root must name the repository-owned overlay root");
			if (typeof latest.manifest !== "string" || latest.manifest.length === 0) errors.push("latestAssets.manifest must name the repository-owned overlay manifest");
		}
	} else {
		errors.push(`skills.source must be one of ${SKILL_SOURCE_MODES.join("|")} (found ${JSON.stringify(skillsSource)})`);
	}
	return errors;
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

/** Resolves each builtin skill name to a pinned-checkout directory, senpi-native root first. */
async function resolvePinnedSkillSources(omoRoot, names) {
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

/**
 * Reads the repository-owned latest overlay manifest. The overlay is the only latest skill source, so
 * it must prove its own identity: the latest commit, the complete selection, the names it retires and
 * each owned skill's exact file list. Nothing here reads the pinned checkout, so a host path cannot
 * silently contribute latest assets.
 */
async function readLatestOverlay(latestAssetsRoot) {
	const manifestPath = join(latestAssetsRoot, LATEST_MANIFEST_NAME);
	if (!existsSync(manifestPath)) throw new Error(`repository-owned latest overlay manifest not found: ${manifestPath}`);
	let overlay;
	try {
		overlay = JSON.parse(await readFile(manifestPath, "utf8"));
	} catch (error) {
		throw new Error(`latest overlay manifest is not valid JSON: ${error.message}`);
	}
	if (overlay === null || typeof overlay !== "object" || Array.isArray(overlay)) throw new Error(`latest overlay manifest is not an object: ${manifestPath}`);
	if (typeof overlay.commit !== "string" || !COMMIT_PATTERN.test(overlay.commit)) {
		throw new Error(`latest overlay manifest commit must be a full 40-hex commit (found ${JSON.stringify(overlay.commit)})`);
	}
	const selection = Array.isArray(overlay.selection) ? overlay.selection.filter((name) => typeof name === "string") : [];
	if (selection.length === 0) throw new Error(`latest overlay manifest declares no selection: ${manifestPath}`);
	const retired = Array.isArray(overlay.retired) ? overlay.retired.filter((name) => typeof name === "string") : [];
	const overlap = retired.filter((name) => selection.includes(name));
	if (overlap.length > 0) throw new Error(`latest overlay selection and retired list overlap: ${overlap.join(", ")}`);
	const declared = overlay.skills !== null && typeof overlay.skills === "object" && !Array.isArray(overlay.skills) ? overlay.skills : {};
	// The browser skill's engine runtime is build-materialized, so the overlay must declare its
	// immutable upstream identity (package + commit) and the exact runtime files; staging then
	// requires an explicit runtime directory that proves that same identity.
	let browserRuntime = null;
	if (overlay.browserRuntime !== undefined && overlay.browserRuntime !== null) {
		const runtime = overlay.browserRuntime;
		if (typeof runtime !== "object" || Array.isArray(runtime)) throw new Error(`latest overlay browserRuntime is not an object: ${manifestPath}`);
		if (runtime.skill !== BROWSER_SKILL_NAME) throw new Error(`latest overlay browserRuntime.skill must be ${JSON.stringify(BROWSER_SKILL_NAME)} (found ${JSON.stringify(runtime.skill)})`);
		if (runtime.targetDir !== BROWSER_RUNTIME_TARGET_DIR) throw new Error(`latest overlay browserRuntime.targetDir must be ${JSON.stringify(BROWSER_RUNTIME_TARGET_DIR)} (found ${JSON.stringify(runtime.targetDir)})`);
		if (typeof runtime.materializer !== "string" || runtime.materializer.length === 0) throw new Error("latest overlay browserRuntime must name the repository-owned materializer that produced the runtime");
		const dependency = runtime.dependency;
		if (dependency === null || typeof dependency !== "object" || dependency.name !== "omowright" || typeof dependency.commit !== "string" || !COMMIT_PATTERN.test(dependency.commit)) {
			throw new Error("latest overlay browserRuntime.dependency must record the omowright package and a full 40-hex commit");
		}
		browserRuntime = {
			skill: runtime.skill,
			targetDir: runtime.targetDir,
			materializer: runtime.materializer,
			dependency: {
				name: dependency.name,
				spec: typeof dependency.spec === "string" ? dependency.spec : null,
				commit: dependency.commit,
				version: typeof dependency.version === "string" ? dependency.version : null,
			},
		};
	}
	return {
		root: latestAssetsRoot,
		manifestPath,
		commit: overlay.commit,
		names: [...new Set(selection)].sort(),
		retired,
		declared,
		browserRuntime,
		versions: {
			omoSenpi: typeof overlay.versions?.omoSenpi === "string" ? overlay.versions.omoSenpi : null,
			sharedSkills: typeof overlay.versions?.sharedSkills === "string" ? overlay.versions.sharedSkills : null,
		},
	};
}

/**
 * Resolves each selected latest skill to its repository-owned overlay directory. The on-disk file set
 * must equal the overlay manifest's declaration for that skill: a partial or extra tree is refused
 * rather than shipped short, so the overlay can never publish a silently truncated skill.
 */
async function resolveLatestSkillSources(overlay) {
	const resolved = new Map();
	const errors = [];
	for (const name of overlay.names) {
		const directory = join(overlay.root, SKILLS_DIR, name);
		if (!existsSync(join(directory, "SKILL.md"))) {
			errors.push(`missing skill tree: ${LATEST_ASSETS_DIR}/${SKILLS_DIR}/${name}`);
			continue;
		}
		const declared = overlay.declared[name];
		if (!Array.isArray(declared) || declared.length === 0) {
			errors.push(`skill ${name} has no declared file list in ${LATEST_MANIFEST_NAME}`);
			continue;
		}
		const declaredPaths = declared.map((entry) => (entry !== null && typeof entry === "object" ? entry.path : null));
		if (declaredPaths.some((path) => typeof path !== "string" || path.length === 0)) {
			errors.push(`skill ${name} declares an entry without a path`);
			continue;
		}
		const onDisk = [...(await listFiles(directory))].sort();
		const declaredSorted = [...declaredPaths].sort();
		if (declaredSorted.join("\n") !== onDisk.join("\n")) {
			errors.push(`skill ${name} file set differs from its declaration: on-disk=[${onDisk.join(", ")}] declared=[${declaredSorted.join(", ")}]`);
			continue;
		}
		// A declared sha256 binds the shipped bytes to the recorded identity. A mismatch is a
		// corrupted overlay and refuses the run; a null hash is simply not yet bound.
		const mismatched = [];
		for (const entry of declared) {
			if (typeof entry.sha256 !== "string" || !/^[0-9a-f]{64}$/.test(entry.sha256)) continue;
			if ((await sha256(join(directory, entry.path))) !== entry.sha256) mismatched.push(entry.path);
		}
		if (mismatched.length > 0) {
			errors.push(`skill ${name} files disagree with their recorded sha256: ${mismatched.join(", ")}`);
			continue;
		}
		resolved.set(name, { directory, root: `${LATEST_ASSETS_DIR}/${SKILLS_DIR}` });
	}
	if (errors.length > 0) throw new Error(`latest overlay is incomplete:\n  ${errors.join("\n  ")}`);
	return resolved;
}

/**
 * Resolves the browser skill's materialized engine runtime. The repository holds no committed copy
 * (upstream gitignores `skills/browser/runtime/omowright`), so the selected latest staging requires
 * an explicit `--omowright-runtime <dir>` and refuses a content-only browser tree. The runtime's own
 * manifest must carry the immutable identity the overlay declares (upstream package + commit), so a
 * foreign or stale runtime cannot be staged silently.
 */
async function resolveBrowserRuntime(overlay, explicit) {
	const declared = overlay.browserRuntime;
	if (declared === null) return null;
	if (!overlay.names.includes(declared.skill)) {
		throw new Error(`latest overlay declares a ${declared.skill} runtime but does not select the ${declared.skill} skill`);
	}
	if (explicit === undefined || explicit === null) {
		throw new Error(`--skill-source latest ships the ${declared.skill} skill engine and requires --omowright-runtime <materialized runtime dir>; materialize it with \`node ${declared.materializer} --source <omowright checkout> --target <dir>\``);
	}
	const directory = resolve(explicit);
	if (!existsSync(directory) || !(await stat(directory)).isDirectory()) throw new Error(`--omowright-runtime is not a directory: ${directory}`);
	for (const file of BROWSER_RUNTIME_FILES) {
		const candidate = join(directory, file);
		if (!existsSync(candidate) || !(await stat(candidate)).isFile()) throw new Error(`--omowright-runtime is missing ${file}: ${candidate}`);
	}
	let runtimeManifest;
	try {
		runtimeManifest = JSON.parse(await readFile(join(directory, BROWSER_RUNTIME_MANIFEST), "utf8"));
	} catch (error) {
		throw new Error(`--omowright-runtime manifest is not valid JSON: ${error.message}`);
	}
	if (runtimeManifest === null || typeof runtimeManifest !== "object" || Array.isArray(runtimeManifest)) {
		throw new Error(`--omowright-runtime manifest is not an object: ${directory}`);
	}
	if (typeof runtimeManifest.version !== "string" || runtimeManifest.version.length === 0) throw new Error(`--omowright-runtime manifest records no omowright version: ${directory}`);
	if (typeof runtimeManifest.sourceDigest !== "string" || !/^[0-9a-f]{64}$/.test(runtimeManifest.sourceDigest)) throw new Error(`--omowright-runtime manifest records no 64-hex sourceDigest: ${directory}`);
	if (typeof runtimeManifest.commit !== "string" || !COMMIT_PATTERN.test(runtimeManifest.commit)) throw new Error(`--omowright-runtime manifest records no full omowright commit: ${directory}`);
	if (runtimeManifest.commit !== declared.dependency.commit) {
		throw new Error(`--omowright-runtime was materialized from omowright ${runtimeManifest.commit}, not the overlay-declared commit ${declared.dependency.commit}`);
	}
	return { directory, version: runtimeManifest.version, sourceDigest: runtimeManifest.sourceDigest };
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
	// The staging source is explicit and never inferred. `latest` reads only the repository-owned
	// overlay, so the pinned checkout can never silently contribute latest assets.
	const skillSource = options.skillSource ?? DEFAULT_SKILL_SOURCE;
	if (!SKILL_SOURCE_MODES.includes(skillSource)) {
		throw new Error(`--skill-source must be one of ${SKILL_SOURCE_MODES.join("|")} (found ${JSON.stringify(skillSource)})`);
	}
	// Sibling: staged only when an operator names an existing file, never guessed.
	const gitBashMcp = options.gitBashBinary === undefined || options.gitBashBinary === null ? null : resolve(options.gitBashBinary);
	if (gitBashMcp !== null && (!existsSync(gitBashMcp) || !(await stat(gitBashMcp)).isFile())) {
		throw new Error(`--git-bash-binary is not a regular file: ${gitBashMcp}`);
	}
	// The selected product staging ships the git-bash MCP sibling, so `latest` requires the actual
	// built sibling by path: an absent sibling refuses the selected delivery instead of silently
	// staging the baseline. The pinned baseline keeps it optional.
	if (skillSource === "latest" && gitBashMcp === null) {
		throw new Error(`--skill-source latest is the selected product staging and requires --git-bash-binary <built ${GIT_BASH_MCP_NAME}>; the pinned baseline stages the sibling optionally`);
	}
	// Refuse an unsafe or non-empty destination before anything is written. The destination is not
	// removed here: every input is resolved and read first, so a refused run leaves an existing
	// stage intact.
	const output = resolve(options.output);
	if (output === sep || output === resolve(homedir())) throw new Error(`refusing to stage into ${output}`);
	const outputExists = existsSync(output);
	if (outputExists && (await readdir(output)).length > 0 && !options.force) throw new Error(`staging directory is not empty: ${output} (pass --force to overwrite)`);

	// Resolve, read and validate every input before the destination is cleared: the staged skill source
	// (pinned checkout or repository-owned overlay), the builtin skill set and its sources, the license
	// sources, and the manifest provenance (versions, pins and the staged-source identity, whose shape
	// and match are checked here). A bad source, a missing skill, a missing license text or invalid
	// provenance refuses the run without destroying an existing stage. The destination is removed only
	// once those inputs are in hand; a copy failure after that point can still leave a partial
	// directory, which verify() then reports.
	const repoRoot = resolveRepoRoot(options.repoRoot);
	let omoRoot = null;
	let overlay = null;
	let browserRuntime = null;
	let skillNames = [];
	let sources = new Map();
	let stagedSource;
	if (skillSource === "pinned") {
		omoRoot = resolveOmoRoot(options.omoRoot);
		skillNames = await parseBuiltinSkillNames(repoRoot, omoRoot);
		sources = await resolvePinnedSkillSources(omoRoot, skillNames);
		stagedSource = { skillsSource: "pinned", omoRoot, omoCommit: await gitHead(omoRoot) };
	} else {
		overlay = await readLatestOverlay(resolve(options.latestAssets ?? join(repoRoot, LATEST_ASSETS_DIR)));
		skillNames = overlay.names;
		sources = await resolveLatestSkillSources(overlay);
		browserRuntime = await resolveBrowserRuntime(overlay, options.omowrightRuntime);
		// The installed loader binds the staged tree by this root/env pair; recording it keeps the
		// overlay source identity, the complete staged content and the loader that resolves it together.
		const latestAssets = {
			root: overlay.root,
			commit: overlay.commit,
			manifest: LATEST_MANIFEST_NAME,
			loader: { skillsRoot: SKILLS_DIR, env: "OMO_SENPI_SKILLS_ROOT" },
		};
		// The staged engine runtime, with the immutable upstream identity it was materialized from.
		// Recorded only when the overlay declares one, so a pinned-shaped overlay cannot claim a runtime.
		if (browserRuntime !== null) {
			latestAssets.browserRuntime = {
				skill: BROWSER_SKILL_NAME,
				targetDir: BROWSER_RUNTIME_TARGET_DIR,
				materializer: overlay.browserRuntime.materializer,
				dependency: overlay.browserRuntime.dependency,
				version: browserRuntime.version,
				sourceDigest: browserRuntime.sourceDigest,
				files: BROWSER_RUNTIME_FILES.map((file) => `${BROWSER_RUNTIME_TARGET_DIR}/${file}`),
			};
		}
		stagedSource = { skillsSource: "latest", latestAssets };
	}
	const licenseSources = new Map();
	for (const license of LICENSE_FILES) {
		const source = join(repoRoot, license.source);
		if (!existsSync(source)) throw new Error(`license text missing from the checkout: ${license.source}`);
		licenseSources.set(license.name, source);
	}
	// The published config schema is a required shipped asset: it is resolved and validated here,
	// before the destination is cleared, so a missing asset refuses the run instead of destroying an
	// existing stage. --schema overrides the source path; the default is the committed asset.
	const schemaSource = resolve(options.schema ?? join(repoRoot, SCHEMA_ASSET_SOURCE));
	if (!existsSync(schemaSource) || !(await stat(schemaSource)).isFile()) {
		throw new Error(`published config schema not found at ${schemaSource}: run 'bun tools/omo-schema.mjs --generate' (or pass --schema)`);
	}
	const workspaceVersion = await parseWorkspaceVersion(repoRoot);
	const omoSenpiVersion = overlay === null ? await parsePackageVersion(join(omoRoot, "packages/omo-senpi/package.json")) : overlay.versions.omoSenpi;
	const sharedSkillsVersion = overlay === null ? await parsePackageVersion(join(omoRoot, "packages/shared-skills/package.json")) : overlay.versions.sharedSkills;
	const pins = await parsePins(repoRoot);
	const provenance = provenanceErrors(pins, stagedSource, skillSource);
	if (provenance.length > 0) throw new Error(`refusing to stage with invalid provenance:\n  ${provenance.join("\n  ")}`);

	if (outputExists) await rm(output, { recursive: true, force: true });

	await mkdir(join(output, SKILLS_DIR), { recursive: true });
	await mkdir(join(output, LICENSES_DIR), { recursive: true });

	const executables = [];
	await copyExecutable(binary, join(output, CLI_BINARY_NAME));
	executables.push(await fileEntry(output, CLI_BINARY_NAME, binary));
	await copyExecutable(astGrepMcp, join(output, AST_GREP_MCP_NAME));
	executables.push(await fileEntry(output, AST_GREP_MCP_NAME, astGrepMcp));
	if (gitBashMcp !== null) {
		await copyExecutable(gitBashMcp, join(output, GIT_BASH_MCP_NAME));
		executables.push(await fileEntry(output, GIT_BASH_MCP_NAME, gitBashMcp));
	}

	const licenses = [];
	for (const license of LICENSE_FILES) {
		const relPath = `${LICENSES_DIR}/${license.name}`;
		await copyFile(licenseSources.get(license.name), join(output, relPath));
		licenses.push(await fileEntry(output, relPath, license.source));
	}

	await copyFile(schemaSource, join(output, SCHEMA_ASSET_NAME));
	const schema = await fileEntry(output, SCHEMA_ASSET_NAME, options.schema ? schemaSource : SCHEMA_ASSET_SOURCE);

	const stagedNames = [...sources.keys()].sort();
	const skillFiles = [];
	for (const name of stagedNames) {
		const { directory, root } = sources.get(name);
		for (const rel of await copyTree(directory, join(output, SKILLS_DIR, name))) {
			skillFiles.push(await fileEntry(output, `${SKILLS_DIR}/${name}/${rel}`, `${root}/${name}/${rel}`));
		}
	}

	// The browser engine runtime is shipped beside its skill but is not part of the overlay's declared
	// per-skill file set (the repository holds no committed copy), so it is copied here and hashed
	// into the same manifest the runtime files land in.
	if (browserRuntime !== null) {
		for (const file of BROWSER_RUNTIME_FILES) {
			const relPath = `${BROWSER_RUNTIME_TARGET_DIR}/${file}`;
			await mkdir(dirname(join(output, relPath)), { recursive: true });
			await copyFile(join(browserRuntime.directory, file), join(output, relPath));
			skillFiles.push(await fileEntry(output, relPath, `${LATEST_ASSETS_DIR}.browserRuntime/${file}`));
		}
	}

	const layout = { binary: CLI_BINARY_NAME, astGrepMcp: AST_GREP_MCP_NAME, skills: SKILLS_DIR, licenses: LICENSES_DIR, manifest: MANIFEST_NAME };
	if (gitBashMcp !== null) layout.gitBashMcp = GIT_BASH_MCP_NAME;

	const manifest = {
		schemaVersion: SCHEMA_VERSION,
		generator: "tools/package-native.mjs",
		layout,
		versions: {
			mhc: workspaceVersion,
			astGrepMcp: workspaceVersion,
			omoSenpi: omoSenpiVersion,
			sharedSkills: sharedSkillsVersion,
		},
		pins,
		sources: stagedSource,
		executables,
		licenses,
		schema,
		skills: { source: skillSource, names: stagedNames, files: skillFiles },
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
	// The optional git-bash sibling is absent when the key is absent; when the key is present it must
	// name the declared sibling, so a manifest cannot claim a sibling under a foreign name.
	if (manifest.layout?.gitBashMcp !== undefined && manifest.layout.gitBashMcp !== GIT_BASH_MCP_NAME) {
		errors.push(`layout.gitBashMcp must be ${JSON.stringify(GIT_BASH_MCP_NAME)} (found ${JSON.stringify(manifest.layout.gitBashMcp)})`);
	}
	// Provenance: the manifest must record both expected upstream pins as full commits, and the
	// staging source it actually used must prove its identity. `pinned` requires the recorded checkout
	// commit to equal the OMO pin; `latest` requires the repository-owned overlay's declared latest
	// commit and never compares the pin, because the pin is not the latest source. This is read from
	// the manifest alone, so a staged directory verifies without resolving any host path.
	for (const error of provenanceErrors(manifest.pins, manifest.sources, manifest.skills?.source)) errors.push(error);
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
	const schemaEntries = manifest.schema !== null && typeof manifest.schema === "object" && !Array.isArray(manifest.schema) ? [manifest.schema] : [];
	const skillFiles = Array.isArray(manifest.skills?.files) ? manifest.skills.files : [];
	for (const entry of executables) await check(entry);
	for (const entry of licenses) await check(entry);
	for (const entry of schemaEntries) await check(entry);
	for (const entry of skillFiles) await check(entry);
	// Required entries: the two executables the runtime resolves as binary siblings and both
	// retained license texts must be present, so a manifest cannot omit a shipped asset.
	for (const required of [CLI_BINARY_NAME, AST_GREP_MCP_NAME]) {
		if (!executables.some((entry) => entry?.path === required)) errors.push(`required executable missing from the manifest: ${required}`);
	}
	// The published config schema is a required shipped asset: the manifest must carry it, it must be
	// hashed, and the staged file must be present.
	if (manifest.schema === undefined) {
		errors.push(`required published schema missing from the manifest: ${SCHEMA_ASSET_NAME}`);
	} else {
		if (manifest.schema.path !== SCHEMA_ASSET_NAME) errors.push(`schema.path must be ${JSON.stringify(SCHEMA_ASSET_NAME)} (found ${JSON.stringify(manifest.schema.path)})`);
		if (!existsSync(join(directory, SCHEMA_ASSET_NAME))) errors.push(`missing published schema: ${SCHEMA_ASSET_NAME}`);
	}
	// The optional git-bash sibling is required exactly when the manifest declares it.
	if (manifest.layout?.gitBashMcp !== undefined && !executables.some((entry) => entry?.path === GIT_BASH_MCP_NAME)) {
		errors.push(`declared git-bash sibling is missing from the manifest executables: ${GIT_BASH_MCP_NAME}`);
	}
	// The browser engine runtime is required exactly when the manifest declares it: a declared
	// runtime whose files are not hashed would ship a `browser` skill whose loader throws.
	const declaredRuntime = manifest.sources?.latestAssets?.browserRuntime;
	if (declaredRuntime !== undefined) {
		if (declaredRuntime === null || typeof declaredRuntime !== "object" || Array.isArray(declaredRuntime)) {
			errors.push("sources.latestAssets.browserRuntime must record the staged browser engine runtime");
		} else {
			if (declaredRuntime.skill !== BROWSER_SKILL_NAME) errors.push(`browserRuntime.skill must be ${JSON.stringify(BROWSER_SKILL_NAME)} (found ${JSON.stringify(declaredRuntime.skill)})`);
			if (declaredRuntime.targetDir !== BROWSER_RUNTIME_TARGET_DIR) errors.push(`browserRuntime.targetDir must be ${JSON.stringify(BROWSER_RUNTIME_TARGET_DIR)} (found ${JSON.stringify(declaredRuntime.targetDir)})`);
			if (typeof declaredRuntime.dependency?.commit !== "string" || !COMMIT_PATTERN.test(declaredRuntime.dependency.commit)) errors.push("browserRuntime.dependency.commit must be a full 40-hex commit");
			if (typeof declaredRuntime.version !== "string" || declaredRuntime.version.length === 0) errors.push("browserRuntime.version must record the materialized omowright version");
			if (typeof declaredRuntime.sourceDigest !== "string" || !/^[0-9a-f]{64}$/.test(declaredRuntime.sourceDigest)) errors.push("browserRuntime.sourceDigest must be a 64-hex digest");
			const runtimeFiles = Array.isArray(declaredRuntime.files) ? declaredRuntime.files : [];
			if (runtimeFiles.length === 0) errors.push("browserRuntime.files must list the staged runtime files");
			for (const relPath of runtimeFiles) {
				if (!skillFiles.some((entry) => entry?.path === relPath)) errors.push(`declared browser runtime file is not in the hashed manifest: ${relPath}`);
				if (typeof relPath === "string" && !existsSync(join(directory, relPath))) errors.push(`missing browser runtime file: ${relPath}`);
			}
		}
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
		const skillMd = `${skillsDir}/${name}/SKILL.md`;
		if (!skillFiles.some((entry) => entry?.path === skillMd)) errors.push(`declared skill SKILL.md is not in the hashed manifest: ${skillMd}`);
		if (!existsSync(join(directory, skillsDir, name, "SKILL.md"))) errors.push(`missing skill: ${name}`);
	}
	return { ok: errors.length === 0, errors, manifest };
}

function manifestFileCount(manifest) {
	return (manifest?.executables?.length ?? 0) + (manifest?.licenses?.length ?? 0) + (manifest?.schema ? 1 : 0) + (manifest?.skills?.files?.length ?? 0);
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
		const schemaFixture = `${JSON.stringify({ $schema: "http://json-schema.org/draft-07/schema#", $id: "https://raw.githubusercontent.com/code-yeongyu/oh-my-openagent/dev/assets/omo.schema.json", title: "OmO Configuration", type: "object", properties: { a: { type: "string" } }, required: ["a"], additionalProperties: false }, null, 2)}`;

		await mkdir(join(repo, "crates/omo/components/maho-omo-telemetry/src"), { recursive: true });
		await mkdir(join(repo, "LICENSES"), { recursive: true });
		await writeFile(join(repo, "Cargo.toml"), `[workspace]\n\n[workspace.package]\nversion = "9.9.9"\n`);
		await writeFile(join(repo, "crates/omo/components/maho-omo-telemetry/src/product_identity.rs"), `pub const BUILTIN_SKILL_NAMES: &[&str] = &[${names.map((name) => `"${name}"`).join(", ")}];\n`);
		await writeFile(join(repo, "LICENSES/MIT.txt"), "MIT fixture\n");
		await writeFile(join(repo, "LICENSES/SUL-1.0.md"), "SUL fixture\n");

		await mkdir(join(repo, "assets"), { recursive: true });
		await writeFile(join(repo, "assets/omo.schema.json"), schemaFixture);
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

		// The pinned checkout is a real git repository, so the manifest records concrete provenance:
		// commit the fixture and pin PINS.md to its revision, which verify() then requires to agree.
		const gitRun = async (args) => await Bun.spawn(["git", ...args], { stdout: "ignore", stderr: "ignore" }).exited;
		await gitRun(["-C", omo, "init", "-q"]);
		await gitRun(["-C", omo, "-c", "user.email=fixture@example.com", "-c", "user.name=fixture", "-c", "commit.gpgsign=false", "commit", "--allow-empty", "-q", "-m", "fixture"]);
		const omoCommit = await gitHead(omo);
		await writeFile(
			join(repo, "PINS.md"),
			`# Source pins\n\n| Source | Short | Full commit | Location |\n| --- | --- | --- | --- |\n| senpi | aaaaaaa | aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa | /Users/indo/code/senpi |\n| oh-my-openagent | bbbbbbb | ${omoCommit} | /Users/indo/code/oh-my-openagent |\n`,
		);

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
		check("source pins recorded", result.manifest.pins.omo === omoCommit && result.manifest.pins.senpi === "a".repeat(40));
		check("staged executables carry the exec bit", ((await stat(join(out, "mhc"))).mode & 0o111) !== 0 && ((await stat(join(out, "ast-grep-mcp"))).mode & 0o111) !== 0);
		check("manifest verifies the staged tree", (await verify(out)).ok);
		check("published schema is staged beside the binary", existsSync(join(out, "omo.schema.json")) && result.manifest.schema.path === "omo.schema.json");
		check("published schema is a hashed manifest entry", result.manifest.schema.sha256 === (await sha256(join(out, "omo.schema.json"))));

		// Verifier negatives: the manifest is the package proof, so an empty object, an omitted
		// required executable, or a file whose mode disagrees with the manifest must all fail.
		const schemaDroppedDir = join(root, "schema-dropped");
		await copyTree(out, schemaDroppedDir);
		const schemaDropped = JSON.parse(await readFile(join(schemaDroppedDir, MANIFEST_NAME), "utf8"));
		delete schemaDropped.schema;
		await writeFile(join(schemaDroppedDir, MANIFEST_NAME), `${JSON.stringify(schemaDropped, null, 2)}`);
		const schemaDroppedResult = await verify(schemaDroppedDir);
		check("a manifest without the published schema is rejected", !schemaDroppedResult.ok && schemaDroppedResult.errors.some((error) => error.includes("published schema missing from the manifest")));

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

		const omittedPinsDir = join(root, "omitted-pins");
		await copyTree(out, omittedPinsDir);
		const omittedPinsManifest = JSON.parse(await readFile(join(omittedPinsDir, MANIFEST_NAME), "utf8"));
		delete omittedPinsManifest.pins;
		await writeFile(join(omittedPinsDir, MANIFEST_NAME), `${JSON.stringify(omittedPinsManifest, null, 2)}\n`);
		const omittedPins = await verify(omittedPinsDir);
		check("manifest omitting pins is rejected", !omittedPins.ok && omittedPins.errors.some((error) => error.includes("pin senpi")));

		const unmappedSkillDir = join(root, "unmapped-skill");
		await copyTree(out, unmappedSkillDir);
		const unmappedManifest = JSON.parse(await readFile(join(unmappedSkillDir, MANIFEST_NAME), "utf8"));
		unmappedManifest.skills.files = unmappedManifest.skills.files.filter((entry) => entry.path !== "skills/alpha/SKILL.md");
		await writeFile(join(unmappedSkillDir, MANIFEST_NAME), `${JSON.stringify(unmappedManifest, null, 2)}\n`);
		const unmappedSkill = await verify(unmappedSkillDir);
		check("declared skill without a hashed mapping is rejected", !unmappedSkill.ok && unmappedSkill.errors.some((error) => error.includes("not in the hashed manifest") && error.includes("skills/alpha/SKILL.md")));

		// A mismatched pin is a rejected input: it must be refused before the existing stage is
		// touched, so a bad PINS.md cannot destroy a good stage and only then fail self-verification.
		const originalPins = await readFile(join(repo, "PINS.md"), "utf8");
		await writeFile(join(repo, "PINS.md"), originalPins.replace(omoCommit, "c".repeat(40)));
		const pinsPreserved = await readFile(join(out, CLI_BINARY_NAME));
		await expectReject("mismatched pin is refused before restage", stage({ binary: join(binDir, "mhc"), output: out, omoRoot: omo, repoRoot: repo, force: true }), "does not match the omo pin");
		check("existing stage survives a rejected pin mismatch", existsSync(join(out, CLI_BINARY_NAME)) && (await readFile(join(out, CLI_BINARY_NAME))).equals(pinsPreserved));
		await writeFile(join(repo, "PINS.md"), originalPins);

		const second = await stage({ binary: join(binDir, "mhc"), output: join(root, "out2"), omoRoot: omo, repoRoot: repo });
		check("manifest is deterministic across runs", JSON.stringify(result.manifest) === JSON.stringify(second.manifest));

		const spaced = join(root, "with space", "out");
		check("staging directory may contain spaces", (await stage({ binary: join(binDir, "mhc"), output: spaced, omoRoot: omo, repoRoot: repo })).ok && (await verify(spaced)).ok);
		check("--force restages a non-empty directory", (await stage({ binary: join(binDir, "mhc"), output: out, omoRoot: omo, repoRoot: repo, force: true })).ok && (await verify(out)).ok);

		await writeFile(join(out, "skills/beta/SKILL.md"), "tampered and longer than the staged file\n");
		const tampered = await verify(out);
		check("tampered asset fails verification", !tampered.ok && tampered.errors.some((error) => error.includes("skills/beta/SKILL.md")));
		check("tampered asset reports a size or hash mismatch", tampered.errors.some((error) => error.includes("mismatch")));

		await rm(join(repo, "assets/omo.schema.json"));
		await expectReject("a missing published schema is refused", stage({ binary: join(binDir, "mhc"), output: join(root, "no-schema"), omoRoot: omo, repoRoot: repo }), "published config schema not found");
		await writeFile(join(repo, "assets/omo.schema.json"), schemaFixture);

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

		// The pinned lists were mutated above; restore them so the source-selection cases below can
		// stage the pinned default again and prove the two modes are independent.
		await writeFile(join(repo, "crates/omo/components/maho-omo-telemetry/src/product_identity.rs"), `pub const BUILTIN_SKILL_NAMES: &[&str] = &[${names.map((name) => `"${name}"`).join(", ")}];\n`);
		await writeFile(join(omo, "packages/omo-senpi/src/components/telemetry/product-identity.ts"), `export const BUILTIN_SKILL_NAMES = Object.freeze([\n  ${names.map((name) => `"${name}"`).join(", ")},\n] as const)\n`);

		// The selected product staging (`--skill-source latest`) requires the git-bash sibling, so the
		// built sibling is present before the latest cases run.
		await writeFile(join(binDir, GIT_BASH_MCP_NAME), "#!/bin/sh\necho git-bash\n");

		// --- latest source selection: the repository-owned overlay is the only latest skill source ---
		// The overlay declares its own commit, its selection, the names it retires and each owned
		// skill's complete file list; staging must prove that set rather than scan a directory.
		const overlayRoot = join(repo, "assets/omo-latest");
		const overlaySkills = join(overlayRoot, SKILLS_DIR);
		const putOverlaySkill = async (name, marker) => {
			await mkdir(join(overlaySkills, name, "references"), { recursive: true });
			await writeFile(join(overlaySkills, name, "SKILL.md"), `# ${name}\n${marker}\n`);
			await writeFile(join(overlaySkills, name, "references/note.md"), `note ${marker}\n`);
			return [{ path: "SKILL.md", sha256: null }, { path: "references/note.md", sha256: null }];
		};
		const overlayFiles = { alpha: await putOverlaySkill("alpha", "latest"), delta: await putOverlaySkill("delta", "latest") };
		const overlayCommit = "d".repeat(40);
		const writeOverlay = async (extra = {}) => {
			await writeFile(
				join(overlayRoot, MANIFEST_NAME),
				`${JSON.stringify({ schemaVersion: 1, name: "omo-latest", commit: overlayCommit, versions: { omoSenpi: "6.0.0", sharedSkills: "0.2.0" }, selection: ["alpha", "delta"], retired: ["start-work"], skills: overlayFiles, ...extra }, null, 2)}\n`,
			);
		};
		await writeOverlay();

		const latestOut = join(root, "latest-out");
		const latest = await stage({ binary: join(binDir, "mhc"), output: latestOut, repoRoot: repo, skillSource: "latest", gitBashBinary: join(binDir, GIT_BASH_MCP_NAME) });
		check("latest source stages the overlay selection", latest.manifest.skills.source === "latest" && JSON.stringify(latest.manifest.skills.names) === JSON.stringify(["alpha", "delta"]));
		check("latest source records the overlay identity", latest.manifest.sources.latestAssets.commit === overlayCommit && latest.manifest.sources.latestAssets.manifest === MANIFEST_NAME);
		check("latest source records no pinned checkout", latest.manifest.sources.omoRoot === undefined && latest.manifest.sources.omoCommit === undefined);
		check("latest source retires the declared and unselected names", !existsSync(join(latestOut, SKILLS_DIR, "start-work", "SKILL.md")) && !existsSync(join(latestOut, SKILLS_DIR, "beta", "SKILL.md")));
		check("latest staged bytes equal the overlay bytes", (await readFile(join(latestOut, SKILLS_DIR, "alpha", "SKILL.md"), "utf8")) === (await readFile(join(overlaySkills, "alpha", "SKILL.md"), "utf8")));
		check("latest overlay versions are recorded", latest.manifest.versions.omoSenpi === "6.0.0" && latest.manifest.versions.sharedSkills === "0.2.0");
		check("latest manifest verifies", (await verify(latestOut)).ok);
		check("selected latest staging stages the git-bash sibling", existsSync(join(latestOut, GIT_BASH_MCP_NAME)) && latest.manifest.layout.gitBashMcp === GIT_BASH_MCP_NAME);
		check("latest records the installed loader binding", latest.manifest.sources.latestAssets.loader?.env === "OMO_SENPI_SKILLS_ROOT" && latest.manifest.sources.latestAssets.loader?.skillsRoot === SKILLS_DIR);

		const pinnedDefault = await stage({ binary: join(binDir, "mhc"), output: join(root, "pinned-default"), omoRoot: omo, repoRoot: repo });
		check("pinned remains the default source", pinnedDefault.manifest.skills.source === "pinned" && JSON.stringify(pinnedDefault.manifest.skills.names) === JSON.stringify(["alpha", "beta", "gamma"]));
		check("pinned and latest select independently", (await verify(join(root, "pinned-default"))).ok && latest.manifest.skills.names.length !== pinnedDefault.manifest.skills.names.length);

		await writeOverlay({ commit: omoCommit });
		await expectReject("latest overlay reusing the pin is refused", stage({ binary: join(binDir, "mhc"), output: join(root, "latest-pin"), repoRoot: repo, skillSource: "latest", gitBashBinary: join(binDir, GIT_BASH_MCP_NAME) }), "must record the latest source, not the pin");
		await writeOverlay();
		await expectReject("an unknown skill source is refused", stage({ binary: join(binDir, "mhc"), output: join(root, "bad-source"), repoRoot: repo, skillSource: "nightly" }), "--skill-source");
		await writeOverlay({ selection: ["alpha", "delta", "missing-tree"] });
		await expectReject("latest selection without a tree is refused", stage({ binary: join(binDir, "mhc"), output: join(root, "latest-missing"), repoRoot: repo, skillSource: "latest", gitBashBinary: join(binDir, GIT_BASH_MCP_NAME) }), "missing skill tree");
		await writeOverlay();
		await rm(join(overlaySkills, "delta/references/note.md"));
		await expectReject("latest skill whose file set differs from its declaration is refused", stage({ binary: join(binDir, "mhc"), output: join(root, "latest-short"), repoRoot: repo, skillSource: "latest", gitBashBinary: join(binDir, GIT_BASH_MCP_NAME) }), "file set differs from its declaration");
		await writeFile(join(overlaySkills, "delta/references/note.md"), "note latest\n");
		await writeOverlay({ skills: { ...overlayFiles, delta: [{ path: "SKILL.md", sha256: "0".repeat(64) }, { path: "references/note.md", sha256: null }] } });
		await expectReject("a declared overlay sha256 that disagrees with the bytes is refused", stage({ binary: join(binDir, "mhc"), output: join(root, "latest-hash"), repoRoot: repo, skillSource: "latest", gitBashBinary: join(binDir, GIT_BASH_MCP_NAME) }), "disagree with their recorded sha256");
		await writeOverlay();
		await rm(join(overlayRoot, MANIFEST_NAME));
		await expectReject("a missing latest overlay manifest is refused", stage({ binary: join(binDir, "mhc"), output: join(root, "latest-no-manifest"), repoRoot: repo, skillSource: "latest", gitBashBinary: join(binDir, GIT_BASH_MCP_NAME) }), "overlay manifest not found");
		await writeOverlay();

		// --- git-bash MCP sibling: required in the selected latest staging, optional in pinned ---
		await expectReject("selected latest staging without the git-bash sibling is refused", stage({ binary: join(binDir, "mhc"), output: join(root, "latest-no-gitbash"), repoRoot: repo, skillSource: "latest" }), "--git-bash-binary");
		await writeFile(join(binDir, GIT_BASH_MCP_NAME), "#!/bin/sh\necho git-bash\n");
		const gitBashOut = join(root, "with-git-bash");
		const withGitBash = await stage({ binary: join(binDir, "mhc"), output: gitBashOut, omoRoot: omo, repoRoot: repo, gitBashBinary: join(binDir, GIT_BASH_MCP_NAME) });
		check("git-bash sibling is staged when named", existsSync(join(gitBashOut, GIT_BASH_MCP_NAME)) && withGitBash.manifest.layout.gitBashMcp === GIT_BASH_MCP_NAME);
		check("git-bash sibling is a hashed manifest executable", withGitBash.manifest.executables.some((entry) => entry.path === GIT_BASH_MCP_NAME) && (await verify(gitBashOut)).ok);
		check("git-bash sibling is absent when not named", !existsSync(join(out, GIT_BASH_MCP_NAME)) && result.manifest.layout.gitBashMcp === undefined);
		const gitBashDroppedDir = join(root, "git-bash-dropped");
		await copyTree(gitBashOut, gitBashDroppedDir);
		const gitBashDropped = JSON.parse(await readFile(join(gitBashDroppedDir, MANIFEST_NAME), "utf8"));
		gitBashDropped.executables = gitBashDropped.executables.filter((entry) => entry.path !== GIT_BASH_MCP_NAME);
		await writeFile(join(gitBashDroppedDir, MANIFEST_NAME), `${JSON.stringify(gitBashDropped, null, 2)}\n`);
		const gitBashDroppedResult = await verify(gitBashDroppedDir);
		check("a declared git-bash sibling missing from the manifest is rejected", !gitBashDroppedResult.ok && gitBashDroppedResult.errors.some((error) => error.includes("git-bash sibling is missing") && error.includes(GIT_BASH_MCP_NAME)));
		await expectReject("a named git-bash sibling that is not a file is refused", stage({ binary: join(binDir, "mhc"), output: join(root, "git-bash-missing"), omoRoot: omo, repoRoot: repo, gitBashBinary: join(binDir, "nope") }), "not a regular file");

		// --- browser engine runtime: the selected latest staging ships a materialized runtime ---
		// The overlay declares the runtime's immutable upstream identity; staging requires an explicit
		// runtime directory whose own manifest proves that same identity, and refuses a content-only
		// browser tree. The runtime is not part of the overlay's declared per-skill file set.
		const runtimeOverlayRoot = join(root, "overlay-browser");
		const runtimeOverlaySkills = join(runtimeOverlayRoot, SKILLS_DIR);
		await mkdir(join(runtimeOverlaySkills, BROWSER_SKILL_NAME), { recursive: true });
		await writeFile(join(runtimeOverlaySkills, BROWSER_SKILL_NAME, "SKILL.md"), `# ${BROWSER_SKILL_NAME}\nlatest\n`);
		const omowrightCommit = "293ca5002cbd4c8b0c104c5934683385ef7e0d3a";
		const runtimeOverlayCommit = "e".repeat(40);
		const writeRuntimeOverlay = async (extra = {}) => {
			await writeFile(
				join(runtimeOverlayRoot, MANIFEST_NAME),
				`${JSON.stringify({
					schemaVersion: 1,
					name: "omo-latest",
					commit: runtimeOverlayCommit,
					selection: [BROWSER_SKILL_NAME],
					retired: [],
					skills: { [BROWSER_SKILL_NAME]: [{ path: "SKILL.md", sha256: null }] },
					browserRuntime: {
						skill: BROWSER_SKILL_NAME,
						targetDir: BROWSER_RUNTIME_TARGET_DIR,
						materializer: `${LATEST_ASSETS_DIR}/stage-omowright-runtime.mjs`,
						dependency: { name: "omowright", spec: `github:code-yeongyu/omowright#${omowrightCommit}`, commit: omowrightCommit, version: "0.0.0-fixture" },
						files: BROWSER_RUNTIME_FILES,
					},
					...extra,
				}, null, 2)}\n`,
			);
		};
		const runtimeDir = join(root, "omowright-runtime");
		await mkdir(runtimeDir, { recursive: true });
		const writeRuntime = async (extra = {}) => {
			await writeFile(join(runtimeDir, "index.js"), "export const connectBrowserSkill = () => 'fixture-session';\n");
			await writeFile(join(runtimeDir, "page-bundle.js"), "(function(){ globalThis.__omowright = { fixture: true } })();\n");
			await writeFile(
				join(runtimeDir, BROWSER_RUNTIME_MANIFEST),
				`${JSON.stringify({ name: "omowright", version: "0.0.0-fixture", commit: omowrightCommit, sourceDigest: "a".repeat(64), files: { "index.js": "b".repeat(64), "page-bundle.js": "c".repeat(64) }, ...extra }, null, 2)}\n`,
			);
		};
		await writeRuntimeOverlay();
		await writeRuntime();

		const runtimeOut = join(root, "latest-runtime-out");
		const withRuntime = await stage({ binary: join(binDir, "mhc"), output: runtimeOut, repoRoot: repo, latestAssets: runtimeOverlayRoot, skillSource: "latest", gitBashBinary: join(binDir, GIT_BASH_MCP_NAME), omowrightRuntime: runtimeDir });
		check("selected latest staging stages the browser engine runtime", BROWSER_RUNTIME_FILES.every((file) => existsSync(join(runtimeOut, BROWSER_RUNTIME_TARGET_DIR, file))));
		check("the staged runtime is hashed into the manifest", BROWSER_RUNTIME_FILES.every((file) => withRuntime.manifest.skills.files.some((entry) => entry.path === `${BROWSER_RUNTIME_TARGET_DIR}/${file}`)));
		check("the staged runtime records the immutable omowright identity", withRuntime.manifest.sources.latestAssets.browserRuntime.dependency.commit === omowrightCommit && withRuntime.manifest.sources.latestAssets.browserRuntime.version === "0.0.0-fixture" && withRuntime.manifest.sources.latestAssets.browserRuntime.sourceDigest === "a".repeat(64));
		check("the staged runtime bytes equal the materialized runtime", (await readFile(join(runtimeOut, BROWSER_RUNTIME_TARGET_DIR, "index.js"), "utf8")) === (await readFile(join(runtimeDir, "index.js"), "utf8")));
		check("the latest manifest with a runtime verifies", (await verify(runtimeOut)).ok);

		await expectReject("selected latest staging without the browser runtime is refused", stage({ binary: join(binDir, "mhc"), output: join(root, "latest-no-runtime"), repoRoot: repo, latestAssets: runtimeOverlayRoot, skillSource: "latest", gitBashBinary: join(binDir, GIT_BASH_MCP_NAME) }), "--omowright-runtime");
		await rm(join(runtimeDir, "page-bundle.js"));
		await expectReject("a runtime directory missing a staged file is refused", stage({ binary: join(binDir, "mhc"), output: join(root, "latest-short-runtime"), repoRoot: repo, latestAssets: runtimeOverlayRoot, skillSource: "latest", gitBashBinary: join(binDir, GIT_BASH_MCP_NAME), omowrightRuntime: runtimeDir }), "missing page-bundle.js");
		await writeRuntime();
		await writeRuntime({ commit: "f".repeat(40) });
		await expectReject("a runtime materialized from a different omowright commit is refused", stage({ binary: join(binDir, "mhc"), output: join(root, "latest-foreign-runtime"), repoRoot: repo, latestAssets: runtimeOverlayRoot, skillSource: "latest", gitBashBinary: join(binDir, GIT_BASH_MCP_NAME), omowrightRuntime: runtimeDir }), "not the overlay-declared commit");
		await writeRuntime();

		const runtimeDroppedDir = join(root, "runtime-dropped");
		await copyTree(runtimeOut, runtimeDroppedDir);
		const runtimeDropped = JSON.parse(await readFile(join(runtimeDroppedDir, MANIFEST_NAME), "utf8"));
		runtimeDropped.skills.files = runtimeDropped.skills.files.filter((entry) => entry.path !== `${BROWSER_RUNTIME_TARGET_DIR}/index.js`);
		await writeFile(join(runtimeDroppedDir, MANIFEST_NAME), `${JSON.stringify(runtimeDropped, null, 2)}\n`);
		const runtimeDroppedResult = await verify(runtimeDroppedDir);
		check("a declared runtime file missing from the manifest is rejected", !runtimeDroppedResult.ok && runtimeDroppedResult.errors.some((error) => error.includes("browser runtime file is not in the hashed manifest") && error.includes("index.js")));
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
	let skillSource = null;
	let latestAssets = null;
	let gitBashBinary = null;
	let omowrightRuntime = null;
	let schema = null;
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
		else if (argument === "--skill-source") skillSource = next(argument);
		else if (argument === "--latest-assets") latestAssets = next(argument);
		else if (argument === "--git-bash-binary") gitBashBinary = next(argument);
		else if (argument === "--omowright-runtime") omowrightRuntime = next(argument);
		else if (argument === "--schema") schema = next(argument);
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
	const result = await stage({ binary, output, astGrepMcp, omoRoot, repoRoot, force, skillSource, latestAssets, gitBashBinary, omowrightRuntime, schema });
	console.log(`package-native: staged ${result.manifest.executables.length} executable(s), ${result.manifest.skills.names.length} skill(s) (${result.manifest.skills.files.length} files) and ${result.manifest.licenses.length} license(s) into ${result.output}`);
	console.log(`package-native: skill source ${result.manifest.skills.source}`);
	console.log(`package-native: manifest ${result.manifestPath}`);
}

try {
	await main(process.argv.slice(2));
} catch (error) {
	console.error(`package-native: ${error.message}`);
	process.exit(1);
}
