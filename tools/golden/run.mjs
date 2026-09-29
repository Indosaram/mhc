#!/usr/bin/env bun
// Golden fixture generator. Renders cases/*.json with the pinned senpi source and writes fixtures
// into crates/<crate>/tests/golden/. Fixtures are only ever produced here; never hand-edit them.
//
//   bun tools/golden/run.mjs --case <name>   generate one case
//   bun tools/golden/run.mjs --all           generate every case
//
// Case kinds:
//   component: {crate, module, export, props, widths, theme}
//     new (module.export)(...props).render(width) -> <case>.<width>.ansi (lines joined by "\n")
//   screen: {crate, cols, rows, writes}
//     writes are fed to senpi's VirtualTerminal (@xterm/headless 6.0.0) -> <case>.<cols>x<rows>.json
import { mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { pinnedSenpiRoot } from "./pin.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..");
const casesDir = join(here, "cases");

function usage(message) {
	console.error(`run.mjs: ${message}\nusage: bun tools/golden/run.mjs (--case <name> | --all)`);
	process.exit(2);
}

function parseArgs(argv) {
	const args = { cases: [], all: false };
	for (let i = 0; i < argv.length; i++) {
		if (argv[i] === "--case") {
			const name = argv[++i];
			if (!name) usage("--case needs a name");
			args.cases.push(name);
		} else if (argv[i] === "--all") {
			args.all = true;
		} else {
			usage(`unknown argument ${argv[i]}`);
		}
	}
	if (!args.all && args.cases.length === 0) usage("nothing to do");
	return args;
}

function loadCase(name) {
	const path = join(casesDir, `${name}.json`);
	let spec;
	try {
		spec = JSON.parse(readFileSync(path, "utf8"));
	} catch (error) {
		usage(`cannot read case ${path}: ${error.message}`);
	}
	if (spec.name !== name) usage(`case ${path} declares name ${spec.name}`);
	if (!/^[a-z0-9][a-z0-9-]*$/.test(spec.crate ?? "")) usage(`case ${name}: invalid crate`);
	return spec;
}

function goldenDir(spec) {
	return join(repoRoot, "crates", spec.crate, "tests", "golden");
}

async function importSenpi(senpi, modulePath) {
	const abs = resolve(senpi, modulePath);
	if (!abs.startsWith(senpi + "/")) usage(`module ${modulePath} escapes SENPI_SRC`);
	return import(pathToFileURL(abs).href);
}

async function renderComponent(senpi, spec) {
	const mod = await importSenpi(senpi, spec.module);
	const Ctor = mod[spec.export];
	if (typeof Ctor !== "function") usage(`case ${spec.name}: ${spec.module} has no export ${spec.export}`);
	if (!Array.isArray(spec.widths) || spec.widths.length === 0) usage(`case ${spec.name}: widths required`);
	const outputs = [];
	for (const width of spec.widths) {
		// A fresh instance per width keeps senpi's render cache out of the picture.
		const lines = new Ctor(...(spec.props ?? [])).render(width);
		outputs.push({ file: `${spec.name}.${width}.ansi`, content: lines.join("\n") });
	}
	return outputs;
}

function colorOf(cell, which) {
	const isDefault = which === "fg" ? cell.isFgDefault() : cell.isBgDefault();
	if (isDefault) return "default";
	const value = which === "fg" ? cell.getFgColor() : cell.getBgColor();
	const isPalette = which === "fg" ? cell.isFgPalette() : cell.isBgPalette();
	if (isPalette) return `p${value}`;
	return `#${value.toString(16).padStart(6, "0")}`;
}

/** JSON cell format shared with maho-test-support's vterm module. */
function serializeScreen(term) {
	const xterm = term.xterm;
	const buffer = xterm.buffer.active;
	const lines = [];
	for (let y = 0; y < xterm.rows; y++) {
		const line = buffer.getLine(buffer.viewportY + y);
		const cells = [];
		for (let x = 0; x < xterm.cols; x++) {
			const cell = line?.getCell(x);
			if (!cell) {
				cells.push({ ch: "", fg: "default", bg: "default", attrs: [] });
				continue;
			}
			const attrs = [];
			if (cell.isBold()) attrs.push("bold");
			if (cell.isDim()) attrs.push("dim");
			if (cell.isItalic()) attrs.push("italic");
			if (cell.isUnderline()) attrs.push("underline");
			if (cell.isInverse()) attrs.push("inverse");
			cells.push({ ch: cell.getChars(), fg: colorOf(cell, "fg"), bg: colorOf(cell, "bg"), attrs });
		}
		lines.push(cells);
	}
	return { cols: xterm.cols, rows: xterm.rows, cursor: term.getCursorPosition(), viewport: term.getViewport(), cells: lines };
}

async function renderScreen(senpi, spec) {
	const { VirtualTerminal } = await importSenpi(senpi, "packages/tui/test/virtual-terminal.ts");
	const term = new VirtualTerminal(spec.cols, spec.rows);
	for (const chunk of spec.writes ?? []) term.write(chunk);
	await term.flush();
	return [{ file: `${spec.name}.${spec.cols}x${spec.rows}.json`, content: `${JSON.stringify(serializeScreen(term), null, 1)}\n` }];
}

const args = parseArgs(process.argv.slice(2));
const senpi = pinnedSenpiRoot();
const names = args.all
	? readdirSync(casesDir).filter((f) => f.endsWith(".json")).map((f) => f.slice(0, -5)).sort()
	: args.cases;
for (const name of names) {
	const spec = loadCase(name);
	const outputs =
		spec.kind === "component"
			? await renderComponent(senpi, spec)
			: spec.kind === "screen"
				? await renderScreen(senpi, spec)
				: usage(`case ${name}: unknown kind ${spec.kind}`);
	const dir = goldenDir(spec);
	mkdirSync(dir, { recursive: true });
	for (const { file, content } of outputs) {
		const path = join(dir, file);
		writeFileSync(path, content);
		console.log(`wrote ${path.slice(repoRoot.length + 1)} (${Buffer.byteLength(content)} bytes)`);
	}
}
