#!/usr/bin/env bun
// Render a terminal grid to a DERIVED PNG at the same geometry as the PTY capture. This is a
// derived rasterization of the authoritative raw stream (terminal-ansi.txt) / the xterm cell grid,
// NOT a literal desktop screenshot.
//
//   bun png-render.mjs <grid.txt|grid.json> <cols> <rows> <out-png> [out-json]
//
// Input:
//   * a `.json` cell grid: { "cols":N, "rows":N, "cells":[ {"ch","fg","bg","w"} ... ] } where each
//     entry is one CELL in row-major order; `w` is the cell's column width (2 for a wide glyph whose
//     trailing cell is empty). Colors are "#rrggbb" or null for the terminal default. This preserves
//     per-cell positions AND colors (theme-faithful).
//   * otherwise plain text: each line is rendered left-to-right at a fixed 8x16 monospace cell,
//     advancing 2 columns for East-Asian wide code points.
//
// Fixed metrics: CELL_W=8, CELL_H=16, monospace font-size 14 — the same geometry used for the
// width/overflow check, so a cell's x = column*8 always.
import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const CELL_W = 8;
const CELL_H = 16;
const FONT_SIZE = 14;
const DEFAULT_FG = "#d4d4d4";
const DEFAULT_BG = "#1e1e1e";

const ESC = String.fromCharCode(0x1b);
const CSI = String.fromCharCode(0x9b);
const ANSI = new RegExp("[".concat(ESC, CSI, "][[()#;?]*(?:[0-9]{1,4}(?:;[0-9]{0,4})*)?[0-9A-ORZcf-nqry=><]"), "g");
const stripAnsi = (s) => s.replace(ANSI, "");

const WIDE_RANGES = [
	[0x1100, 0x115f], [0x2e80, 0x303e], [0x3041, 0x33ff], [0x3400, 0x4dbf], [0x4e00, 0x9fff],
	[0xa000, 0xa4cf], [0xac00, 0xd7a3], [0xf900, 0xfaff], [0xfe30, 0xfe6f], [0xff00, 0xff60],
	[0xffe0, 0xffe6], [0x1f300, 0x1faff], [0x20000, 0x3fffd],
];
const charWidth = (cp) => (WIDE_RANGES.some(([a, b]) => cp >= a && cp <= b) ? 2 : 1);
const escapeXml = (t) => t.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

function cellsFromJson(parsed) {
	const out = [];
	for (const cell of parsed.cells ?? []) {
		out.push({ ch: cell.ch ?? " ", fg: cell.fg ?? null, bg: cell.bg ?? null, w: cell.w === 2 ? 2 : 1, col: cell.col ?? null, row: cell.row ?? null });
	}
	return out;
}

function cellsFromText(text, cols, rows) {
	const lines = text.split(/\r?\n/);
	const cells = [];
	for (let row = 0; row < rows; row++) {
		let col = 0;
		for (const ch of [...stripAnsi(lines[row] ?? "")]) {
			if (col >= cols) break;
			const w = charWidth(ch.codePointAt(0) ?? 0);
			cells.push({ ch, fg: null, bg: null, w, col, row });
			col += w;
		}
	}
	return cells;
}

const [, , inputPath, colsArg, rowsArg, outPng, outJsonArg] = process.argv;
if (!inputPath || !colsArg || !rowsArg || !outPng) {
	console.error("usage: bun png-render.mjs <grid.txt|grid.json> <cols> <rows> <out-png> [out-json]");
	process.exit(2);
}
const cols = Number(colsArg);
const rows = Number(rowsArg);
if (!Number.isInteger(cols) || cols <= 0 || !Number.isInteger(rows) || rows <= 0) {
	console.error("png-render: cols/rows must be positive integers");
	process.exit(2);
}
const outJson = outJsonArg ?? outPng + ".json";

let sourceText;
try {
	sourceText = readFileSync(inputPath, "utf8");
} catch (error) {
	console.error("png-render: cannot read input: " + error.message);
	process.exit(1);
}
let cells;
let colored = false;
if (inputPath.endsWith(".json")) {
	try {
		const parsed = JSON.parse(sourceText);
		cells = cellsFromJson(parsed);
		colored = true;
	} catch (error) {
		console.error("png-render: invalid cells json: " + error.message);
		process.exit(1);
	}
} else {
	cells = cellsFromText(sourceText, cols, rows);
}

const width = cols * CELL_W;
const height = rows * CELL_H;
const parts = [];
parts.push(`<desc>derived render of the terminal grid (not a literal desktop screenshot); cell ${CELL_W}x${CELL_H}</desc>`);
parts.push(`<rect x="0" y="0" width="${width}" height="${height}" fill="${DEFAULT_BG}"/>`);
// Row-major: assign explicit column/row when the input lacks them.
let idx = 0;
for (const cell of cells) {
	const row = cell.row ?? Math.floor(idx / cols);
	const col = cell.col ?? (idx % cols);
	idx += 1;
	const x = col * CELL_W;
	const y = row * CELL_H;
	if (cell.bg && cell.bg !== DEFAULT_BG) parts.push(`<rect x="${x}" y="${y}" width="${cell.w * CELL_W}" height="${CELL_H}" fill="${cell.bg}"/>`);
	if (cell.ch && cell.ch !== " ") {
		parts.push(
			`<text x="${x + 1}" y="${y + CELL_H - 4}" font-family="monospace" font-size="${FONT_SIZE}" fill="${cell.fg ?? DEFAULT_FG}" xml:space="preserve">${escapeXml(cell.ch)}</text>`,
		);
	}
}
const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}">${parts.join("")}</svg>`;

const scratch = mkdtempSync(join(tmpdir(), "session2-residual-png-"));
const svgPath = join(scratch, "grid.svg");
writeFileSync(svgPath, svg);
try {
	const child = Bun.spawn(["rsvg-convert", "-w", String(width), "-h", String(height), "-o", outPng, svgPath], { stdout: "pipe", stderr: "pipe" });
	const [stderr, code] = await Promise.all([new Response(child.stderr).text(), child.exited]);
	if (code !== 0) {
		console.error("png-render: rsvg-convert exited " + code + ": " + stderr.trim());
		process.exit(1);
	}
} catch (error) {
	console.error("png-render: rsvg-convert unavailable: " + error.message);
	process.exit(1);
} finally {
	rmSync(scratch, { recursive: true, force: true });
}

const png = readFileSync(outPng);
writeFileSync(
	outJson,
	JSON.stringify(
		{
			schema: "session2-residual-png/v1",
			cols,
			rows,
			cellW: CELL_W,
			cellH: CELL_H,
			width,
			height,
			renderer: "rsvg-convert",
			derived: true,
			colored,
			cellCount: cells.length,
			pngSha256: createHash("sha256").update(png).digest("hex"),
			sourceSha256: createHash("sha256").update(sourceText).digest("hex"),
		},
		null,
		2,
	) + "\n",
);
console.log(outJson);
process.exit(0);

