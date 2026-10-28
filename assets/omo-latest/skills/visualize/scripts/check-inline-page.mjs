#!/usr/bin/env node
// check-inline-page.mjs: static checks for a self-contained inline HTML page.
// MIT License (c) 2026 Indo Yoon.
//
// Usage: node check-inline-page.mjs [--json] <page.html>...
// Exit 0 clean, 1 findings, 2 usage/unreadable.
//
// Checks:
//   network   - no external src/href/url()/@import/fetch targets (http:, https:, protocol-relative //)
//   colour    - no literal colour outside a var() fallback; only theme variables from the allowed list
//   fallback  - every var(--x) carries a fallback
//   font      - font-family values come from var(--font-sans|--font-mono, ...)
//   howto     - a "how computed" block exists (element with data-role="method")
//   nojs      - the main figure (data-role="figure") contains static content (svg/table/img/text), not only an empty mount
import { readFileSync } from "node:fs";

export const ALLOWED_VARS = new Set([
	"--background", "--foreground", "--muted", "--muted-foreground", "--card", "--border",
	"--primary", "--accent", "--destructive", "--warning", "--success",
	"--chart-1", "--chart-2", "--chart-3", "--chart-4", "--chart-5", "--chart-6",
	"--font-sans", "--font-mono", "--radius",
]);

const COLOUR_RE = /#[0-9a-fA-F]{3,8}\b|\b(?:rgba?|hsla?|oklch|oklab|lab|lch|color)\(|(?<![\w-])(?:white|black|red|green|blue|gray|grey|orange|yellow|purple|pink|navy|teal|silver)(?![\w-])/g;

/** Return [start,end) ranges covered by var(...) fallbacks (the part after the first comma). */
function fallbackRanges(css) {
	const ranges = [];
	const re = /var\(\s*(--[\w-]+)\s*(,)?/g;
	let m;
	while ((m = re.exec(css))) {
		let depth = 1;
		let i = re.lastIndex;
		const fbStart = m[2] ? i : -1;
		while (i < css.length && depth > 0) {
			if (css[i] === "(") depth++;
			else if (css[i] === ")") depth--;
			i++;
		}
		if (fbStart >= 0) ranges.push([fbStart, i - 1]);
	}
	return ranges;
}

function lineOf(text, idx) {
	return text.slice(0, idx).split("\n").length;
}

/** Extract CSS-bearing text: <style> blocks and style="" attributes, plus SVG presentation attributes. */
function cssChunks(html) {
	const chunks = [];
	for (const m of html.matchAll(/<style\b[^>]*>([\s\S]*?)<\/style>/gi)) chunks.push({ text: m[1], at: m.index + m[0].indexOf(m[1]) });
	for (const m of html.matchAll(/\sstyle\s*=\s*"([^"]*)"/gi)) chunks.push({ text: m[1], at: m.index + m[0].indexOf(m[1]) });
	for (const m of html.matchAll(/\s(?:fill|stroke|stop-color|color)\s*=\s*"([^"]*)"/gi)) chunks.push({ text: m[1], at: m.index + m[0].indexOf(m[1]), attr: true });
	return chunks;
}

export function checkPage(html, file = "<page>") {
	const out = [];
	const add = (rule, idx, msg) => out.push({ file, line: lineOf(html, idx), rule, message: msg });

	// network
	const net = [
		/\s(?:src|href|action|poster|data)\s*=\s*["']\s*(?:https?:)?\/\//gi,
		/url\(\s*["']?\s*(?:https?:)?\/\//gi,
		/@import\b/gi,
		/\bfetch\s*\(|XMLHttpRequest|new\s+WebSocket|EventSource\s*\(/g,
	];
	for (const re of net) for (const m of html.matchAll(re)) add("network", m.index, `external resource or request: ${m[0].trim()}`);

	for (const { text, at, attr } of cssChunks(html)) {
		const fb = fallbackRanges(text);
		const inFb = (i) => fb.some(([a, b]) => i >= a && i < b);
		for (const m of text.matchAll(COLOUR_RE)) {
			if (!inFb(m.index)) add("colour", at + m.index, `literal colour outside a var() fallback: ${m[0]}`);
		}
		for (const m of text.matchAll(/var\(\s*(--[\w-]+)\s*(,)?/g)) {
			if (!ALLOWED_VARS.has(m[1])) add("colour", at + m.index, `variable ${m[1]} is not in the host theme list`);
			if (!m[2]) add("fallback", at + m.index, `var(${m[1]}) has no fallback`);
		}
		if (!attr) {
			for (const m of text.matchAll(/font-family\s*:\s*([^;}"]+)/gi)) {
				if (!/^\s*var\(\s*--font-(sans|mono)\s*,/.test(m[1])) add("font", at + m.index, `font-family must use var(--font-sans|--font-mono, fallback): ${m[1].trim()}`);
			}
		}
	}
	// SVG/HTML attributes with plain colours that aren't var() — e.g. fill="#f00"
	if (!/data-role\s*=\s*["']method["']/.test(html)) add("howto", 0, 'no "how computed" block (an element with data-role="method")');
	const fig = html.match(/<([a-z0-9]+)[^>]*data-role\s*=\s*["']figure["'][^>]*>([\s\S]*?)<\/\1>/i);
	if (!fig) add("nojs", 0, 'no main figure (an element with data-role="figure")');
	else if (!/<(svg|table|img|figure|p|ol|ul|div|span)\b/i.test(fig[2]) && !fig[2].replace(/<[^>]+>/g, "").trim()) {
		add("nojs", fig.index, "figure has no static markup; it would be empty with scripts disabled");
	}
	return out;
}

function main(argv) {
	const json = argv.includes("--json");
	const files = argv.filter((a) => a !== "--json");
	if (!files.length) {
		process.stderr.write("usage: check-inline-page.mjs [--json] <page.html>...\n");
		return 2;
	}
	const all = [];
	for (const f of files) {
		let html;
		try {
			html = readFileSync(f, "utf8");
		} catch {
			process.stderr.write(`check-inline-page: cannot read ${f}\n`);
			return 2;
		}
		all.push(...checkPage(html, f));
	}
	if (json) process.stdout.write(`${JSON.stringify(all, null, 2)}\n`);
	else {
		for (const x of all) process.stdout.write(`${x.file}:${x.line}  ${x.rule}  ${x.message}\n`);
		process.stdout.write(`check-inline-page: ${files.length} file(s), ${all.length ? `${all.length} finding(s)` : "clean"}\n`);
	}
	return all.length ? 1 : 0;
}

if (process.argv[1] && import.meta.url.endsWith(process.argv[1].split("/").pop())) process.exitCode = main(process.argv.slice(2));
