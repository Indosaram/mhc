#!/usr/bin/env bun
// Parity audit: checks that every senpi TypeScript source file owned by a crate is mapped in that
// crate's parity ledger, that no row is still `todo`, and that ported Rust test counts are not
// lower than the TS test-case counts (unless the row gives an N/A reason).
//
//   bun tools/parity-audit.mjs --crate <crate> [--only 'e1,e2,!e3'] [--repo <checkout>]
//   bun tools/parity-audit.mjs --all [--repo <checkout>]
//   bun tools/parity-audit.mjs --self-test
//
// The audited checkout is --repo, else $PARITY_REPO, else the nearest ancestor of the current
// directory holding Cargo.toml + crates/, else the checkout this script lives in. So the script can
// be run from one lane against another: `cd lane-4 && bun ../lane-3/tools/parity-audit.mjs ...`.
//
// `--crate X` without --only audits the remainder of X: its source roots minus every file an
// `--only`-scoped todo owns per tools/parity-owners.json (plan: "Source roots" rule). `--all`
// audits every file of every crate.
//
// Ledger layout: crates/<X>/parity.d/<todo>.md fragments, merged in ascending todo order; a later
// todo's row for the same TS path replaces the earlier one. The table format is documented in
// tools/PARITY-FORMAT.md. Exit 0 = pass, 1 = audit failure, 2 = usage error.
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const SENPI_PIN = "fe8c564bf33a2cbbdbbba99c9bd8b45b21e37407";

const BUILTIN_DIR = "packages/coding-agent/src/core/extensions/builtin";
// The 40 senpi builtin extension directories (plan IS-5); each maps to crates/builtins/maho-ext-<dir>.
const BUILTIN_DIRS = [
	"account", "anthropic-bash", "anthropic-subscription", "anthropic-web-search", "ask-user", "bash-timeout", "btw", "cache-keepalive",
	"compaction", "config-reload", "cursor-cli-oauth", "goal", "gpt-apply-patch", "help", "herdr", "history-search", "hooks", "imagegen",
	"look-at", "loop", "loop-guard", "mcp", "model-fallback", "nested-agents-md", "openai-image-gen", "openai-web-search", "permission-system",
	"prompt-preset", "reasoning", "recommended-models", "rule-activation", "rules", "terminal", "todotools", "tool-pair-guard", "tool-search",
	"ttsr", "video-in", "webfetch", "websearch",
];

const OMO_COMPONENT_DIR = "packages/omo-senpi/src/components";
// The 19 omo-senpi components (plan IS-8); each maps to crates/omo/components/maho-omo-<dir>. Roots marked
// `omo` are read from OMO_SRC (oh-my-openagent) instead of SENPI_SRC.
const OMO_COMPONENTS = [
	"agent-home", "ast-grep", "comment-checker", "config-resolution", "config-startup", "config-watch", "fallback-architect",
	"init-deep-advisor", "lsp", "mass-ulw", "memory", "native-badge", "onboarding", "start-work-continuation", "task", "telemetry",
	"todo-fanout-reminder", "ultrawork", "ulw-loop",
];

// Crates whose parity is measured against senpi TypeScript source roots (plan: "Source roots").
// Paths are relative to SENPI_SRC. `exclude` entries use the same matching as --only.
export const SOURCE_ROOTS = {
	"maho-ai": [{ root: "packages/ai/src" }],
	"maho-tui": [{ root: "packages/tui/src" }],
	"maho-agent": [{ root: "packages/agent/src" }],
	"maho-tools": [
		{ root: "packages/coding-agent/src/core/tools", exclude: ["renderers/", "render-utils.ts", "diff-render.ts", "write-result.ts"] },
		{ root: "packages/coding-agent/src/core", files: ["bash-executor.ts"] },
	],
	"maho-core": [{ root: "packages/coding-agent/src/core", exclude: ["extensions/", "tools/", "bash-executor.ts"] }],
	"maho-interactive": [{ root: "packages/coding-agent/src/modes/interactive" }],
	"maho-rpc": [
		{ root: "packages/coding-agent/src/modes", files: ["print-mode.ts", "json-event.ts", "provider-native-rendering.ts"] },
		{ root: "packages/coding-agent/src/modes/rpc", prefix: "rpc/" },
	],
	"maho-server": [
		{ root: "packages/protocol/src", prefix: "packages/protocol/src/" },
		{ root: "packages/client/src", prefix: "packages/client/src/" },
		{ root: "packages/server/src", prefix: "packages/server/src/" },
		{ root: "packages/session-backends", prefix: "packages/session-backends/", exclude: ["benchmark/", "scripts/", "dist/"] },
		{ root: "packages/coding-agent/src/modes/app-server", prefix: "packages/coding-agent/src/modes/app-server/" },
	],
	"maho-codemode": [{ root: "packages/senpi-codemode/src" }],
	"maho-cli": [{ root: "packages/coding-agent/src", exclude: ["core/", "modes/"] }],
	"maho-ext-builtin-loose": [
		{
			root: BUILTIN_DIR,
			files: ["account-display-name.ts", "diff.ts", "eval-only-routing.ts", "files.ts", "gpt-account.ts", "import-repro.ts", "monitor-state-event.ts", "oauth-login-interaction.ts", "prompt-url-widget.ts", "redraws.ts", "service-tier.ts", "tps.ts"],
		},
	],
	...Object.fromEntries(BUILTIN_DIRS.map((name) => [`maho-ext-${name}`, [{ root: `${BUILTIN_DIR}/${name}` }]])),
	...Object.fromEntries(OMO_COMPONENTS.map((name) => [`maho-omo-${name}`, [{ omo: true, root: `${OMO_COMPONENT_DIR}/${name}` }]])),
};

function usage(message) {
	console.error(`parity-audit: ${message}\nusage: bun tools/parity-audit.mjs (--crate <crate> [--only <list>] | --all | --self-test)`);
	process.exit(2);
}

// ---------- matching ----------

/** Parses an --only list. Entries prefixed with `!` exclude. */
export function parseOnly(list) {
	if (list === undefined || list === null) return null;
	const entries = list
		.split(",")
		.map((e) => e.trim())
		.filter(Boolean);
	if (entries.length === 0) throw new Error("--only list is empty");
	return {
		include: entries.filter((e) => !e.startsWith("!")),
		exclude: entries.filter((e) => e.startsWith("!")).map((e) => e.slice(1)),
	};
}

/** An entry matches a relative path that starts with it or contains `/<entry>`. */
export function entryMatches(entry, rel) {
	return rel.startsWith(entry) || rel.includes(`/${entry}`);
}

/** A list of only `!` entries starts from every file. */
export function onlyMatches(only, rel) {
	if (!only) return true;
	const included = only.include.length === 0 || only.include.some((e) => entryMatches(e, rel));
	return included && !only.exclude.some((e) => entryMatches(e, rel));
}

// ---------- senpi source files ----------

function isTestFile(rel) {
	return /\.test\.ts$|\.spec\.ts$|\.d\.ts$/.test(rel) || rel.split("/").some((part) => part === "test" || part === "tests" || part === "__tests__");
}

function walkTs(dir) {
	const out = [];
	if (!existsSync(dir)) return out;
	for (const name of readdirSync(dir).sort()) {
		if (name === "node_modules") continue;
		const abs = join(dir, name);
		if (statSync(abs).isDirectory()) out.push(...walkTs(abs));
		else if (name.endsWith(".ts")) out.push(abs);
	}
	return out;
}

/** Non-test .ts files of a crate, as paths relative to their declared root (first root wins). */
const OMO_PIN = "77f3067f157a4f88e6d8ed48b3a6c338654402ed";
let omoChecked = false;
/** oh-my-openagent checkout for `omo` roots: $OMO_SRC (default /Users/indo/code/oh-my-openagent), pinned like senpi. */
function omoSrc() {
	const dir = resolve(process.env.OMO_SRC ?? "/Users/indo/code/oh-my-openagent");
	if (!omoChecked) {
		const head = execFileSync("git", ["-C", dir, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
		if (head !== OMO_PIN) usage(`OMO_SRC ${dir} is at ${head}, expected ${OMO_PIN}`);
		omoChecked = true;
	}
	return dir;
}

export function sourceFiles(senpi, roots) {
	const files = new Map();
	for (const spec of roots) {
		const rootAbs = join(spec.omo ? omoSrc() : senpi, spec.root);
		const candidates = spec.files ? spec.files.map((f) => join(rootAbs, f)).filter(existsSync) : walkTs(rootAbs);
		for (const abs of candidates) {
			const rel = (spec.prefix ?? "") + relative(rootAbs, abs);
			if (isTestFile(rel)) continue;
			if (spec.exclude?.some((e) => entryMatches(e, rel))) continue;
			if (!files.has(rel)) files.set(rel, abs);
		}
	}
	return files;
}

/** Counts `it(` / `test(` calls (including .only/.skip/.each/.todo variants) in a TS test file. */
export function countTsTests(text) {
	const matches = text.match(/(?<![\w.$])(?:it|test)(?:\.(?:only|skip|todo|concurrent|each\([^)]*\)))*\s*\(/g);
	return matches ? matches.length : 0;
}

// ---------- ledger ----------

const STATUS = new Set(["done", "todo", "n/a", "partial"]);

/**
 * Parses one fragment. Returns rows keyed by normalized TS path. Only tables whose header starts
 * with `TS path` are parsed; other tables in the file (e.g. imported COMP notes) are ignored.
 */
export function parseFragment(text, source) {
	const rows = [];
	const lines = text.split("\n");
	for (let i = 0; i < lines.length; i++) {
		const header = splitRow(lines[i]);
		if (!header || header[0].toLowerCase() !== "ts path") continue;
		const cols = header.map((h) => h.toLowerCase());
		const need = ["ts path", "rust path", "ts tests", "rust tests", "status"];
		for (const n of need) if (!cols.includes(n)) throw new Error(`${source}:${i + 1}: table header lacks column "${n}"`);
		i += 2; // skip separator row
		for (; i < lines.length; i++) {
			const cells = splitRow(lines[i]);
			if (!cells) break;
			const get = (name) => cells[cols.indexOf(name)] ?? "";
			const tsPath = stripTicks(get("ts path"));
			const status = get("status").toLowerCase().split(/[\s:(]/)[0];
			if (!STATUS.has(status)) throw new Error(`${source}:${i + 1}: status "${get("status")}" is not one of done, todo, partial, n/a`);
			rows.push({
				tsPath,
				rustPath: stripTicks(get("rust path")),
				tsTests: parseCount(get("ts tests"), source, i + 1),
				rustTests: parseCount(get("rust tests"), source, i + 1),
				status,
				reason: cols.includes("n/a reason") ? get("n/a reason") : naReasonFrom(get("status")),
				source: `${source}:${i + 1}`,
			});
		}
	}
	return rows;
}

function splitRow(line) {
	const t = line.trim();
	if (!t.startsWith("|") || !t.endsWith("|")) return null;
	return t
		.slice(1, -1)
		.split("|")
		.map((c) => c.trim());
}

function stripTicks(s) {
	return s.replace(/^`|`$/g, "").trim();
}

function parseCount(s, source, line) {
	const t = stripTicks(s);
	if (t === "" || t === "-") return 0;
	if (!/^\d+$/.test(t)) throw new Error(`${source}:${line}: test count "${s}" is not a number`);
	return Number(t);
}

function naReasonFrom(status) {
	const m = status.match(/^n\/a\s*[:(]\s*(.+?)\)?$/i);
	return m ? m[1].trim() : "";
}

/** Merges crates/<crate>/parity.d/*.md in ascending todo order; later todos win per TS path. */
export function loadLedger(repo, crate) {
	const dir = join(repo, "crates", crateDir(repo, crate), "parity.d");
	const merged = new Map();
	if (!existsSync(dir)) return merged;
	const fragments = readdirSync(dir)
		.filter((f) => /^\d+\.md$/.test(f))
		.sort((a, b) => Number.parseInt(a, 10) - Number.parseInt(b, 10));
	for (const f of fragments) {
		for (const row of parseFragment(readFileSync(join(dir, f), "utf8"), `${relative(repo, dir)}/${f}`)) merged.set(row.tsPath, row);
	}
	return merged;
}

function fragmentCount(repo, crate) {
	const dir = join(repo, "crates", crateDir(repo, crate), "parity.d");
	return existsSync(dir) ? readdirSync(dir).filter((f) => /^\d+\.md$/.test(f)).length : 0;
}

/** Resolves a crate package name to its directory under crates/ (searches one level of groups). */
export function crateDir(repo, crate) {
	const base = join(repo, "crates");
	const search = (dir, rel, depth) => {
		for (const name of readdirSync(dir)) {
			const abs = join(dir, name);
			if (!statSync(abs).isDirectory()) continue;
			const manifest = join(abs, "Cargo.toml");
			if (existsSync(manifest)) {
				const pkg = readFileSync(manifest, "utf8").match(/^name\s*=\s*"([^"]+)"/m)?.[1];
				if (pkg === crate || name === crate) return rel ? `${rel}/${name}` : name;
			} else if (depth < 3) {
				const found = search(abs, rel ? `${rel}/${name}` : name, depth + 1);
				if (found) return found;
			}
		}
		return null;
	};
	const found = search(base, "", 0);
	if (!found) throw new Error(`crate ${crate} not found under crates/`);
	return found;
}

// ---------- audit ----------

/**
 * Audits one crate. `roots` may be undefined for crates without senpi TS roots (vendored/imported
 * crates): then the merged ledger rows themselves are checked (status, counts) and at least the
 * crate must exist.
 */
export function auditCrate({ repo, senpi, crate, only, roots = SOURCE_ROOTS[crate], scopedOthers = [] }) {
	const problems = [];
	let ledger;
	try {
		ledger = loadLedger(repo, crate);
	} catch (error) {
		return { problems: [error.message], checked: 0 };
	}
	let checked = 0;
	// Without --only, files owned by --only-scoped todos belong to those todos, not to this audit.
	const inScope = only ? (rel) => onlyMatches(only, rel) : (rel) => !scopedOthers.some((s) => onlyMatches(s, rel));
	if (roots) {
		const files = sourceFiles(senpi, roots);
		for (const [rel] of files) {
			if (!inScope(rel)) continue;
			checked++;
			if (!ledger.has(rel)) problems.push(`unmapped: ${rel} has no ledger row`);
		}
		for (const [ts, row] of ledger) {
			if (!inScope(ts)) continue;
			if (isTestFile(ts)) {
				const abs = findTestFile(senpi, roots, ts);
				if (abs) {
					const tsCount = countTsTests(readFileSync(abs, "utf8"));
					if (tsCount > row.rustTests && !row.reason) problems.push(`tests: ${ts} has ${tsCount} TS cases but ${row.rustTests} Rust tests and no N/A reason (${row.source})`);
				}
			}
		}
	}
	for (const [ts, row] of ledger) {
		if (!inScope(ts)) continue;
		if (!roots) checked++;
		if (row.status === "todo") problems.push(`todo: ${ts} is still status todo (${row.source})`);
		if (row.tsTests > row.rustTests && !row.reason) problems.push(`tests: ${ts} lists ${row.tsTests} TS tests but ${row.rustTests} Rust tests and no N/A reason (${row.source})`);
		if (row.status === "n/a" && !row.reason) problems.push(`n/a: ${ts} is N/A without a reason (${row.source})`);
	}
	// Crates without senpi TS roots (vendored Rust, imported COMP crates) keep free-form fragments;
	// they must at least carry one.
	if (!roots && fragmentCount(repo, crate) === 0) problems.push(`empty: crate ${crate} has no parity.d/<todo>.md fragment`);
	return { problems, checked };
}

function findTestFile(senpi, roots, rel) {
	for (const spec of roots) {
		const prefix = spec.prefix ?? "";
		if (!rel.startsWith(prefix)) continue;
		const src = spec.omo ? omoSrc() : senpi;
		for (const base of [join(src, spec.root), join(src, spec.root, "..", "test")]) {
			const abs = join(base, rel.slice(prefix.length));
			if (existsSync(abs)) return abs;
		}
	}
	return null;
}

/**
 * Parsed --only lists of the scoped todos for a crate (tools/parity-owners.json). Used when a crate
 * is audited without --only, to compute the remainder owned by the crate's unscoped todo.
 */
export function scopedOwnerLists(owners, crate) {
	const entry = owners?.crates?.[crate];
	if (!entry) return [];
	return Object.values(entry.scoped ?? {}).map((list) => parseOnly(list));
}

function loadOwners() {
	const path = join(here, "parity-owners.json");
	return existsSync(path) ? JSON.parse(readFileSync(path, "utf8")) : { crates: {} };
}

/** --repo, then $PARITY_REPO, then the nearest workspace ancestor of cwd, then this script's checkout. */
export function resolveRepo(explicit, cwd = process.cwd()) {
	if (explicit) return resolve(explicit);
	if (process.env.PARITY_REPO) return resolve(process.env.PARITY_REPO);
	for (let dir = resolve(cwd); ; dir = dirname(dir)) {
		if (existsSync(join(dir, "Cargo.toml")) && existsSync(join(dir, "crates"))) return dir;
		if (dirname(dir) === dir) break;
	}
	return resolve(here, "..");
}

/** Every workspace crate that has a parity.d directory, plus every crate with a senpi root. */
export function allAuditedCrates(repo) {
	const names = new Set(Object.keys(SOURCE_ROOTS));
	const walk = (dir, depth) => {
		for (const name of readdirSync(dir)) {
			const abs = join(dir, name);
			if (!statSync(abs).isDirectory()) continue;
			if (existsSync(join(abs, "Cargo.toml")) && existsSync(join(abs, "parity.d"))) {
				const pkg = readFileSync(join(abs, "Cargo.toml"), "utf8").match(/^name\s*=\s*"([^"]+)"/m)?.[1];
				if (pkg) names.add(pkg);
			} else if (depth < 2) walk(abs, depth + 1);
		}
	};
	walk(join(repo, "crates"), 0);
	return [...names].sort();
}

function checkSenpiPin(senpi) {
	let head;
	try {
		head = execFileSync("git", ["-C", senpi, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
	} catch {
		usage(`SENPI_SRC ${senpi} is not a git checkout`);
	}
	if (head !== SENPI_PIN) usage(`SENPI_SRC ${senpi} is at ${head}, expected ${SENPI_PIN}`);
}

function report(crate, only, result) {
	const scope = only ? ` --only ${[...only.include, ...only.exclude.map((e) => `!${e}`)].join(",")}` : "";
	if (result.problems.length === 0) {
		console.log(`PASS ${crate}${scope}: ${result.checked} files checked`);
		return true;
	}
	console.log(`FAIL ${crate}${scope}: ${result.problems.length} problem(s)`);
	for (const p of result.problems) console.log(`  ${p}`);
	return false;
}

// ---------- self-test ----------

function selfTest() {
	const tmp = mkdtempSync(join(tmpdir(), "parity-audit-self-"));
	const results = [];
	const check = (name, cond) => {
		results.push({ name, ok: Boolean(cond) });
		console.log(`${cond ? "ok  " : "FAIL"} ${name}`);
	};
	try {
		const senpi = join(tmp, "senpi");
		const repo = join(tmp, "repo");
		const put = (path, text) => {
			mkdirSync(dirname(path), { recursive: true });
			writeFileSync(path, text);
		};
		const root = join(senpi, "packages/ai/src");
		put(join(root, "api/openai-completions.ts"), "export {}\n");
		put(join(root, "api/anthropic-messages.ts"), "export {}\n");
		put(join(root, "types.ts"), "export {}\n");
		put(join(root, "api/openai-completions.test.ts"), 'it("a", () => {});\ntest("b", () => {});\nit.skip("c", () => {});\n');
		put(join(repo, "crates/maho-ai/Cargo.toml"), '[package]\nname = "maho-ai"\n');
		const roots = [{ root: "packages/ai/src" }];
		const header = "| TS path | Rust path | TS tests | Rust tests | status |\n|---|---|---:|---:|---|\n";
		const row = (ts, rust, tsT, rustT, status) => `| ${ts} | ${rust} | ${tsT} | ${rustT} | ${status} |\n`;
		const ledger = (fragments) => {
			rmSync(join(repo, "crates/maho-ai/parity.d"), { recursive: true, force: true });
			for (const [todo, body] of Object.entries(fragments)) put(join(repo, `crates/maho-ai/parity.d/${todo}.md`), `# todo ${todo}\n\n${header}${body}`);
		};
		const run = (onlyList) => auditCrate({ repo, senpi, crate: "maho-ai", only: parseOnly(onlyList), roots });

		const full =
			row("api/openai-completions.ts", "src/api/openai_completions.rs", 0, 0, "done") +
			row("api/anthropic-messages.ts", "src/api/anthropic_messages.rs", 0, 0, "done") +
			row("types.ts", "src/types.rs", 0, 0, "done") +
			row("api/openai-completions.test.ts", "src/api/openai_completions_tests.rs", 3, 3, "done");
		ledger({ 10: full });
		check("complete ledger passes", run().problems.length === 0);
		check("counts it(/test(/it.skip( as 3 cases", countTsTests('it("a", () => {});\ntest("b", () => {});\nit.skip("c", () => {});\n') === 3);

		ledger({ 10: full.replace(/\| types\.ts .*\n/, "") });
		const missing = run();
		check("missing row fails", missing.problems.some((p) => p.includes("unmapped: types.ts")));

		ledger({ 10: full.replace("| 0 | 0 | done |\n| types.ts", "| 0 | 0 | done |\n| types.ts").replace("src/types.rs | 0 | 0 | done", "src/types.rs | 0 | 0 | todo") });
		check("status todo fails", run().problems.some((p) => p.startsWith("todo: types.ts")));

		ledger({ 10: full.replace("openai_completions_tests.rs | 3 | 3 | done", "openai_completions_tests.rs | 3 | 2 | done") });
		check("lower Rust test count fails", run().problems.some((p) => p.startsWith("tests: api/openai-completions.test.ts")));
		ledger({ 10: full.replace("openai_completions_tests.rs | 3 | 3 | done", "openai_completions_tests.rs | 3 | 2 | n/a: one case covers a node-only API") });
		check("lower count with N/A reason passes", run().problems.length === 0);

		ledger({ 5: full.replace("src/types.rs | 0 | 0 | done", "src/types.rs | 0 | 0 | todo"), 12: row("types.ts", "src/types.rs", 0, 0, "done") });
		check("higher-numbered fragment wins", run().problems.length === 0);
		ledger({ 12: full.replace("src/types.rs | 0 | 0 | done", "src/types.rs | 0 | 0 | todo"), 5: row("types.ts", "src/types.rs", 0, 0, "done") });
		check("fragments merge by todo number, not file order", run().problems.some((p) => p.startsWith("todo: types.ts")));

		const noAnthropic = full.replace(/\| api\/anthropic-messages\.ts .*\n/, "");
		ledger({ 10: noAnthropic });
		check("--only api/openai- passes while unowned anthropic module is unmapped", run("api/openai-").problems.length === 0);
		check("unscoped audit still fails on the unmapped anthropic module", run().problems.length > 0);
		ledger({ 10: full.replace(/\| api\/openai-completions\.ts .*\n/, "") });
		check("--only api/openai- fails when an owned openai row is missing", run("api/openai-").problems.some((p) => p.includes("api/openai-completions.ts")));

		ledger({ 10: noAnthropic });
		check("! exclusion: --only '!api/anthropic-' passes", run("!api/anthropic-").problems.length === 0);
		check("! exclusion combined with includes", run("api/,!api/anthropic-").problems.length === 0);
		check("! exclusion does not hide other gaps", run("!api/openai-").problems.some((p) => p.includes("anthropic-messages.ts")));
		check("entry matches /<entry> inside a path", entryMatches("openai-", "api/openai-completions.ts") && !entryMatches("pi/openai", "xapi/openai.ts"));

		put(join(repo, "crates/vendor/maho-grep/Cargo.toml"), '[package]\nname = "maho-grep"\n');
		check("non-TS crate without a fragment fails", auditCrate({ repo, senpi, crate: "maho-grep", only: null, roots: undefined }).problems.some((p) => p.startsWith("empty:")));
		put(join(repo, "crates/vendor/maho-grep/parity.d/4.md"), "| Rust source | Rust tests |\n|---|---:|\n| src/lib.rs | 3 |\n");
		check("non-TS crate with a free-form fragment passes", auditCrate({ repo, senpi, crate: "maho-grep", only: null, roots: undefined }).problems.length === 0);

		ledger({ 10: "| TS path | Rust path | TS tests | Rust tests | status |\n|---|---|---|---|---|\n| types.ts | x | 0 | 0 | maybe |\n" });
		check("unknown status is a parse failure", run().problems.some((p) => p.includes("is not one of")));

		// Remainder: the unscoped owner audits the root minus every --only-scoped todo's files.
		const owners = { crates: { "maho-ai": { scoped: { 10: "api/openai-", 11: "api/anthropic-" }, remainder: 5 } } };
		const runRemainder = () => auditCrate({ repo, senpi, crate: "maho-ai", only: null, roots, scopedOthers: scopedOwnerLists(owners, "maho-ai") });
		ledger({ 5: row("types.ts", "src/types.rs", 0, 0, "done") });
		check("remainder passes while scoped todos' files are unmapped", runRemainder().problems.length === 0 && runRemainder().checked === 1);
		ledger({ 5: row("api/openai-completions.ts", "src/api/openai_completions.rs", 0, 0, "done") });
		check("remainder fails when a remainder file is unmapped", runRemainder().problems.some((p) => p.includes("unmapped: types.ts")));
		check("remainder ignores scoped files it happens to list", !runRemainder().problems.some((p) => p.includes("openai")));

		// Repo resolution from cwd: a nested directory of another checkout resolves to that checkout.
		mkdirSync(join(repo, "crates/maho-ai/src"), { recursive: true });
		put(join(repo, "Cargo.toml"), "[workspace]\n");
		const saved = process.env.PARITY_REPO;
		delete process.env.PARITY_REPO;
		try {
			check("repo resolves from cwd of another checkout", resolveRepo(undefined, join(repo, "crates/maho-ai/src")) === repo);
			check("--repo overrides cwd", resolveRepo(join(tmp, "elsewhere"), join(repo, "crates")) === join(tmp, "elsewhere"));
		} finally {
			if (saved !== undefined) process.env.PARITY_REPO = saved;
		}
	} finally {
		rmSync(tmp, { recursive: true, force: true });
	}
	const failed = results.filter((r) => !r.ok);
	console.log(`self-test: ${results.length - failed.length}/${results.length} passed`);
	return failed.length === 0;
}

// ---------- main ----------

if (import.meta.main) {
	const argv = process.argv.slice(2);
	let crate;
	let onlyList;
	let all = false;
	let self = false;
	let repoArg;
	for (let i = 0; i < argv.length; i++) {
		if (argv[i] === "--crate") crate = argv[++i];
		else if (argv[i] === "--only") onlyList = argv[++i];
		else if (argv[i] === "--repo") repoArg = argv[++i];
		else if (argv[i] === "--all") all = true;
		else if (argv[i] === "--self-test") self = true;
		else usage(`unknown argument ${argv[i]}`);
	}
	if (self) process.exit(selfTest() ? 0 : 1);
	if (onlyList !== undefined && !crate) usage("--only needs --crate");
	if (Boolean(crate) === all) usage("give exactly one of --crate or --all");
	const repo = resolveRepo(repoArg);
	const senpi = resolve(process.env.SENPI_SRC ?? "/Users/indo/code/senpi");
	checkSenpiPin(senpi);
	let only;
	try {
		only = parseOnly(onlyList);
	} catch (error) {
		usage(error.message);
	}
	const crates = all ? allAuditedCrates(repo) : [crate];
	const owners = loadOwners();
	let ok = true;
	for (const c of crates) {
		// --all checks every file; a single unscoped --crate audit checks only the remainder.
		const scopedOthers = all || only ? [] : scopedOwnerLists(owners, c);
		ok = report(c, only, auditCrate({ repo, senpi, crate: c, only, scopedOthers })) && ok;
	}
	process.exit(ok ? 0 : 1);
}
