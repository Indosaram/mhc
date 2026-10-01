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
//   function: {crate, module, calls}
//     each call {export, args} records module.export(...args); a call {export, codepoints: [from, to]}
//     records module.export(String.fromCodePoint(cp)) for every scalar value in range as run-length
//     [start, end, result] triples -> <case>.json
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

/**
 * `markdown` cases render the Markdown component with a named theme from markdown-themes.mjs:
 *
 *   { name, kind: "markdown", crate, theme: "plain"|"chalk", widths: [...],
 *     docs: [{ id, text, paddingX?, paddingY?, options? }] }
 *
 * One `<case>.<id>.<width>.ansi` fixture per document and width.
 */
async function renderMarkdown(senpi, spec) {
	const { Markdown } = await importSenpi(senpi, "packages/tui/src/components/markdown.ts");
	const { setCapabilities } = await importSenpi(senpi, "packages/tui/src/terminal-image.ts");
	const { markdownThemes } = await import("./markdown-themes.mjs");
	const theme = markdownThemes[spec.theme];
	if (!theme) usage(`case ${spec.name}: unknown theme ${spec.theme}`);
	if (!Array.isArray(spec.docs) || spec.docs.length === 0) usage(`case ${spec.name}: docs required`);
	if (!Array.isArray(spec.widths) || spec.widths.length === 0) usage(`case ${spec.name}: widths required`);
	// Pin capabilities so the fixture cannot depend on the terminal that generated it.
	setCapabilities({ images: null, trueColor: false, hyperlinks: false });
	const outputs = [];
	for (const doc of spec.docs) {
		for (const width of spec.widths) {
			const markdown = new Markdown(
				doc.text,
				doc.paddingX ?? 0,
				doc.paddingY ?? 0,
				theme,
				undefined,
				doc.options,
			);
			const lines = markdown.render(width);
			outputs.push({ file: `${spec.name}.${doc.id}.${width}.ansi`, content: lines.join("\n") });
		}
	}
	return outputs;
}

/**
 * `tui-screen` cases drive senpi's own renderer over a recording VirtualTerminal and capture
 * (a) the raw ANSI byte stream the renderer wrote and (b) the resulting screen. The Rust side
 * replays the same steps and must produce the identical stream, so the fixture is both the
 * byte-equal reference and the screen oracle.
 *
 *   { name, kind: "tui-screen", crate, mode: "main"|"alt", cols, rows, steps: [...] }
 *
 * Steps: {op:"text", text, paddingX, paddingY} | {op:"render"} | {op:"resize", cols, rows}
 *      | {op:"wheel", direction} | {op:"key", data} | {op:"stop", preserveScreen}
 * `alt` cases start the screen before the steps (and stop at a `stop` step or at the end).
 */
async function renderTuiScreen(senpi, spec) {
	// The alt screen's mouse sequence and several render branches read the environment; pin it so a
	// fixture cannot depend on the machine that generated it.
	const savedEnv = { TMUX: process.env.TMUX, ZELLIJ: process.env.ZELLIJ, STY: process.env.STY, TERM: process.env.TERM };
	delete process.env.TMUX;
	delete process.env.ZELLIJ;
	delete process.env.STY;
	process.env.TERM = "xterm-256color";
	try {
		return await renderTuiScreenInner(senpi, spec);
	} finally {
		for (const [key, value] of Object.entries(savedEnv)) {
			if (value === undefined) delete process.env[key];
			else process.env[key] = value;
		}
	}
}

async function renderTuiScreenInner(senpi, spec) {
	const { VirtualTerminal } = await importSenpi(senpi, "packages/tui/test/virtual-terminal.ts");
	const { Text } = await importSenpi(senpi, "packages/tui/src/components/text.ts");
	const Tui = (
		await importSenpi(
			senpi,
			spec.mode === "alt" ? "packages/tui/src/tui-alt-screen.ts" : "packages/tui/src/tui-main-screen.ts",
		)
	)[spec.mode === "alt" ? "TuiAltScreen" : "TuiMainScreen"];

	const term = new VirtualTerminal(spec.cols, spec.rows);
	let stream = "";
	const originalWrite = term.write.bind(term);
	term.write = (data) => {
		stream += data;
		originalWrite(data);
	};

	const tui = new Tui(term);
	if (spec.mode === "alt") tui.start();
	for (const step of spec.steps ?? []) {
		if (step.op === "text") {
			const component = new Text(step.text ?? "", step.paddingX ?? 1, step.paddingY ?? 1);
			if (spec.mode === "alt") tui.setLayoutRoot(component);
			else tui.addChild(component);
		} else if (step.op === "render") {
			tui.renderNow(true);
		} else if (step.op === "resize") {
			term.resize(step.cols, step.rows);
		} else if (step.op === "wheel") {
			term.sendInput(`\x1b[<${step.direction < 0 ? 64 : 65};1;1M`);
		} else if (step.op === "key") {
			term.sendInput(step.data);
		} else if (step.op === "stop") {
			tui.stop({ preserveScreen: Boolean(step.preserveScreen) });
		} else {
			usage(`case ${spec.name}: unknown step op ${step.op}`);
		}
	}
	await term.flush();
	return [
		{ file: `${spec.name}.ansi`, content: stream },
		{
			file: `${spec.name}.${term.columns}x${term.rows}.json`,
			content: `${JSON.stringify(serializeScreen(term), null, 1)}\n`,
		},
	];
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

async function renderFunction(senpi, spec) {
	const mod = await importSenpi(senpi, spec.module);
	if (!Array.isArray(spec.calls) || spec.calls.length === 0) usage(`case ${spec.name}: calls required`);
	const results = spec.calls.map((call) => {
		const fn = mod[call.export];
		if (typeof fn !== "function") usage(`case ${spec.name}: ${spec.module} has no export ${call.export}`);
		if (!call.codepoints) return { export: call.export, args: call.args, result: fn(...call.args) };
		const [from, to] = call.codepoints;
		const runs = [];
		for (let cp = from; cp <= to; cp++) {
			if (cp >= 0xd800 && cp <= 0xdfff) continue;
			const result = fn(String.fromCodePoint(cp));
			const last = runs.at(-1);
			if (last && last[1] === cp - 1 && last[2] === result) last[1] = cp;
			else runs.push([cp, cp, result]);
		}
		return { export: call.export, codepoints: call.codepoints, runs };
	});
	return [{ file: `${spec.name}.json`, content: `${JSON.stringify(results, null, 1)}\n` }];
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
			: spec.kind === "markdown"
				? await renderMarkdown(senpi, spec)
				: spec.kind === "screen"
				? await renderScreen(senpi, spec)
				: spec.kind === "tui-screen"
					? await renderTuiScreen(senpi, spec)
					: spec.kind === "function"
						? await renderFunction(senpi, spec)
						: usage(`case ${name}: unknown kind ${spec.kind}`);
	const dir = goldenDir(spec);
	mkdirSync(dir, { recursive: true });
	for (const { file, content } of outputs) {
		const path = join(dir, file);
		writeFileSync(path, content);
		console.log(`wrote ${path.slice(repoRoot.length + 1)} (${Buffer.byteLength(content)} bytes)`);
	}
}