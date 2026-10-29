#!/usr/bin/env bun
// Generate and check the published `assets/omo.schema.json` -- the native projection of the
// harness-neutral config schema.
//
//   bun tools/omo-schema.mjs --generate [--output <path>]
//   bun tools/omo-schema.mjs --check    [--output <path>]
//   bun tools/omo-schema.mjs --self-test
//
// `--generate` runs the crate's `omo_schema` example -- the build-time projection over
// `omo_config_core::omo_config_json_schema()`, the native counterpart of the TypeScript
// `script/build-omo-schema.ts` -- and writes its stdout verbatim to the published asset (default
// `assets/omo.schema.json`). `--check` runs the same projection and fails when the committed asset
// has drifted from it, so the published schema cannot silently diverge from the native config
// schema. Neither path is needed by an installed user: the packaged consumer
// (`tools/package-native.mjs`) stages the committed asset into the distribution, so no cargo is
// required at runtime.
//
// Exit 0 = pass, 1 = failure, 2 = usage error.
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const SCHEMA_OUTPUT_PATH = "assets/omo.schema.json";
const CRATE = "maho-omo-config-core";
const EXAMPLE = "omo_schema";
const DRAFT = "http://json-schema.org/draft-07/schema#";
const SCHEMA_ID = "https://raw.githubusercontent.com/code-yeongyu/oh-my-openagent/dev/assets/omo.schema.json";

function usage() {
	console.log(`usage: bun tools/omo-schema.mjs --generate [--output <path>]
       bun tools/omo-schema.mjs --check    [--output <path>]
       bun tools/omo-schema.mjs --self-test`);
}

function usageError(message) {
	console.error(`omo-schema: ${message}`);
	usage();
	process.exit(2);
}

/** The repository root: the nearest ancestor holding both Cargo.toml and crates/. */
export function resolveRepoRoot() {
	for (let directory = here; ; directory = dirname(directory)) {
		if (existsSync(join(directory, "Cargo.toml")) && existsSync(join(directory, "crates"))) return directory;
		const parent = dirname(directory);
		if (parent === directory) break;
	}
	throw new Error("could not locate the repository root (Cargo.toml + crates/)");
}

/**
 * Runs the native projection -- the crate's `omo_schema` example -- and returns its stdout. The
 * example is the only writer of the asset, so the committed bytes are exactly what the native
 * config schema projects, never a hand-edited or TypeScript-bridged copy.
 */
export async function runProjection(repoRoot) {
	const proc = Bun.spawn(["cargo", "run", "--quiet", "--example", EXAMPLE, "-p", CRATE], {
		cwd: repoRoot,
		stdout: "pipe",
		stderr: "pipe",
	});
	const [stdout, stderr] = await Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text()]);
	const code = await proc.exited;
	if (code !== 0) throw new Error(`cargo run --example ${EXAMPLE} -p ${CRATE} failed (exit ${code}):\n${stderr.trim()}`);
	return stdout;
}

/** Structural checks a published document must satisfy, independent of the projection that made it. */
export function documentErrors(text) {
	let document;
	try {
		document = JSON.parse(text);
	} catch (error) {
		return [`not valid JSON: ${error.message}`];
	}
	if (document === null || typeof document !== "object" || Array.isArray(document)) return ["not a JSON object"];
	const errors = [];
	if (document.$schema !== DRAFT) errors.push(`$schema must be ${DRAFT} (found ${JSON.stringify(document.$schema)})`);
	if (document.$id !== SCHEMA_ID) errors.push(`$id must be ${SCHEMA_ID} (found ${JSON.stringify(document.$id)})`);
	if (document.type !== "object") errors.push(`type must be "object" (found ${JSON.stringify(document.type)})`);
	if (typeof document.title !== "string" || document.title.length === 0) errors.push("title must be a non-empty string");
	const properties = document.properties;
	if (properties === null || typeof properties !== "object" || Array.isArray(properties)) errors.push("properties must be an object");
	else if (Object.keys(properties).length === 0) errors.push("properties must not be empty");
	return errors;
}

/** Byte drift plus identity checks between a committed asset and a freshly generated projection. */
export function compareToProjection(committed, generated) {
	const errors = [];
	for (const error of documentErrors(generated)) errors.push(`projection: ${error}`);
	for (const error of documentErrors(committed)) errors.push(`committed asset: ${error}`);
	if (committed !== generated) {
		errors.push("the committed asset differs from the native projection");
		const committedLines = committed.split("\n");
		const generatedLines = generated.split("\n");
		for (let index = 0; index < Math.max(committedLines.length, generatedLines.length); index += 1) {
			if (committedLines[index] !== generatedLines[index]) {
				errors.push(`  first difference at line ${index + 1}: committed=${JSON.stringify(committedLines[index])} projection=${JSON.stringify(generatedLines[index])}`);
				break;
			}
		}
	}
	return errors;
}

/** Deterministic fixture self-test: the pure checks plus the committed asset's own validity. */
export async function selfTest() {
	const results = [];
	const check = (name, condition) => {
		results.push({ name, ok: Boolean(condition) });
		console.log(`${condition ? "ok  " : "FAIL"} ${name}`);
	};
	const fixture = JSON.stringify({ $schema: DRAFT, $id: SCHEMA_ID, title: "OmO Configuration", description: "x", type: "object", properties: { a: { type: "string" } }, required: ["a"], additionalProperties: false }, null, 2);
	check("a well-formed document validates", documentErrors(fixture).length === 0);
	check("a non-JSON document is rejected", documentErrors("{").some((error) => error.includes("not valid JSON")));
	check("a wrong $id is rejected", documentErrors(fixture.replace(SCHEMA_ID, "https://example.com/omo.schema.json")).some((error) => error.includes("$id")));
	check("an empty properties object is rejected", documentErrors(JSON.stringify({ $schema: DRAFT, $id: SCHEMA_ID, title: "t", type: "object", properties: {} })).some((error) => error.includes("properties must not be empty")));
	check("a non-object document is rejected", documentErrors("[]").some((error) => error.includes("not a JSON object")));
	check("identical documents do not drift", compareToProjection(fixture, fixture).length === 0);
	check("a changed document drifts", compareToProjection(fixture, fixture.replace("OmO", "Omo")).some((error) => error.includes("differs")));
	check("the published asset path is assets/omo.schema.json", SCHEMA_OUTPUT_PATH === "assets/omo.schema.json");
	const repoRoot = resolveRepoRoot();
	const assetPath = join(repoRoot, SCHEMA_OUTPUT_PATH);
	check("the committed asset exists", existsSync(assetPath));
	if (existsSync(assetPath)) {
		const errors = documentErrors(readFileSync(assetPath, "utf8"));
		check("the committed asset is a valid published schema", errors.length === 0);
		for (const error of errors) console.error(`  ${error}`);
	}
	const failed = results.filter((result) => !result.ok);
	console.log(`self-test: ${results.length - failed.length}/${results.length} passed`);
	return failed.length === 0;
}

async function main(argv) {
	let output = null;
	let generate = false;
	let check = false;
	let self = false;
	let cursor = 0;
	const next = (flag) => {
		cursor += 1;
		if (cursor >= argv.length) usageError(`${flag} needs a value`);
		return argv[cursor];
	};
	for (cursor = 0; cursor < argv.length; cursor += 1) {
		const argument = argv[cursor];
		if (argument === "--generate") generate = true;
		else if (argument === "--check") check = true;
		else if (argument === "--self-test") self = true;
		else if (argument === "--output") output = next(argument);
		else if (argument === "--help" || argument === "-h") {
			usage();
			process.exit(0);
		} else usageError(`unknown argument: ${argument}`);
	}
	if (self) process.exit((await selfTest()) ? 0 : 1);
	if (generate === check) usageError("exactly one of --generate or --check is required");
	const repoRoot = resolveRepoRoot();
	const target = resolve(output ?? join(repoRoot, SCHEMA_OUTPUT_PATH));
	const generated = await runProjection(repoRoot);
	const projectionErrors = documentErrors(generated);
	if (projectionErrors.length > 0) {
		console.error("omo-schema: the native projection is not a valid published schema:");
		for (const error of projectionErrors) console.error(`  ${error}`);
		process.exit(1);
	}
	if (generate) {
		mkdirSync(dirname(target), { recursive: true });
		writeFileSync(target, generated);
		console.log(`omo-schema: wrote ${target} (${Buffer.byteLength(generated)} bytes)`);
		process.exit(0);
	}
	if (!existsSync(target)) {
		console.error(`omo-schema: published asset not found: ${target} (run 'bun tools/omo-schema.mjs --generate')`);
		process.exit(1);
	}
	const drift = compareToProjection(readFileSync(target, "utf8"), generated);
	if (drift.length > 0) {
		console.error(`omo-schema: ${target} has drifted from the native projection:`);
		for (const error of drift) console.error(`  ${error}`);
		process.exit(1);
	}
	console.log(`omo-schema: ${target} matches the native projection (${Buffer.byteLength(generated)} bytes)`);
	process.exit(0);
}

if (import.meta.main) {
	try {
		await main(process.argv.slice(2));
	} catch (error) {
		console.error(`omo-schema: ${error.message}`);
		process.exit(1);
	}
}
