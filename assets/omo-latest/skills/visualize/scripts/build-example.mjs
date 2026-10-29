#!/usr/bin/env node
// build-example.mjs: reference implementation of an inline chart page that follows the visualize rules.
// MIT License (c) 2026 Indo Yoon.
// Usage: node build-example.mjs <data.csv> <out.html> [--empty]
// The figure is static SVG + HTML (works with scripts off); a tiny script only adds hover titles.
import { readFileSync, writeFileSync } from "node:fs";

const [csvPath, outPath, flag] = process.argv.slice(2);
if (!csvPath || !outPath) {
	process.stderr.write("usage: build-example.mjs <data.csv> <out.html> [--empty]\n");
	process.exit(2);
}

// 1. Compute (reproducible; this exact text is printed in the "how computed" block).
const METHOD = `rows = csv(${csvPath.split("/").pop()}); keep week, median_build_s;
change = (mean(last 3 weeks) - mean(first 3 weeks)) / mean(first 3 weeks)`;
const rows = flag === "--empty" ? [] : readFileSync(csvPath, "utf8").trim().split(/\r?\n/).slice(1).map((l) => {
	const [week, v] = l.split(",");
	return { week, v: Number(v) };
});
const mean = (a) => a.reduce((s, x) => s + x, 0) / a.length;
const change = rows.length >= 6 ? (mean(rows.slice(-3).map((r) => r.v)) - mean(rows.slice(0, 3).map((r) => r.v))) / mean(rows.slice(0, 3).map((r) => r.v)) : null;

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
const max = rows.length ? Math.ceil(Math.max(...rows.map((r) => r.v)) / 100) * 100 : 100;
const ticks = [0, 0.25, 0.5, 0.75, 1].map((f) => Math.round(max * f));

const title = rows.length
	? `Median build time fell ${Math.abs(Math.round(change * 100))}% after week W38`
	: "No build data for the selected weeks";

const bars = rows.map((r, i) => {
	const w = 100 / rows.length;
	const h = (r.v / max) * 100;
	const series = i < 3 ? "var(--chart-2, light-dark(#8a94a6, #6b7486))" : "var(--chart-1, light-dark(#2563eb, #60a5fa))";
	return `<rect x="${(i * w + w * 0.18).toFixed(2)}" y="${(100 - h).toFixed(2)}" width="${(w * 0.64).toFixed(2)}" height="${h.toFixed(2)}" style="fill:${series}"><title>${esc(r.week)}: ${r.v} s</title></rect>`;
}).join("");

const valueLabels = rows.map((r, i) => {
	const w = 100 / rows.length;
	return `<span class="val" style="left:${(i * w + w / 2).toFixed(2)}%;bottom:${((r.v / max) * 100).toFixed(2)}%">${r.v}</span>`;
}).join("");

const figure = rows.length
	? `<div class="plot" role="img" aria-label="${esc(title)}">
  <div class="yaxis">${ticks.map((t) => `<span style="bottom:${((t / max) * 100).toFixed(2)}%">${t}</span>`).join("")}</div>
  <div class="area">
    <svg viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
      ${ticks.map((t) => `<line x1="0" x2="100" y1="${(100 - (t / max) * 100).toFixed(2)}" y2="${(100 - (t / max) * 100).toFixed(2)}" vector-effect="non-scaling-stroke" style="stroke:var(--border, light-dark(#e2e5ea, #2c313a))"/>`).join("")}
      ${bars}
    </svg>
    ${valueLabels}
  </div>
</div>
<div class="xaxis">${rows.map((r) => `<span>${esc(r.week)}</span>`).join("")}</div>
<p class="unit">Median build time, seconds, by ISO week</p>`
	: `<div class="empty"><p class="empty-h">Nothing to chart yet</p><p>The source returned zero rows for this range. Widen the date range or check that the build log export ran.</p></div>`;

const html = `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>${esc(title)}</title>
<style>
:root{color-scheme:light dark}
html,body{margin:0;padding:0;background:var(--background, light-dark(#ffffff, #121417));color:var(--foreground, light-dark(#16181d, #e8eaee));font-family:var(--font-sans, system-ui, -apple-system, "Segoe UI", sans-serif)}
main{width:100%;max-width:100%;box-sizing:border-box}
h1{font-size:1.05rem;font-weight:600;margin:0 0 .25rem;line-height:1.35}
.sub{margin:0 0 .9rem;font-size:.85rem;color:var(--muted-foreground, light-dark(#5b6270, #9aa1ad))}
.plot{display:grid;grid-template-columns:2.6rem 1fr;height:220px}
.yaxis{position:relative;font-size:.72rem;color:var(--muted-foreground, light-dark(#5b6270, #9aa1ad));font-variant-numeric:tabular-nums}
.yaxis span{position:absolute;right:.4rem;transform:translateY(50%)}
.area{position:relative;height:220px}
.area svg{position:absolute;inset:0;width:100%;height:100%;display:block}
.val{position:absolute;transform:translate(-50%,-.2rem);font-size:.72rem;font-variant-numeric:tabular-nums;color:var(--foreground, light-dark(#16181d, #e8eaee))}
.xaxis{display:flex;margin-left:2.6rem;font-size:.75rem;color:var(--muted-foreground, light-dark(#5b6270, #9aa1ad))}
.xaxis span{flex:1;text-align:center}
.unit{margin:.35rem 0 0 2.6rem;font-size:.72rem;color:var(--muted-foreground, light-dark(#5b6270, #9aa1ad))}
.legend{display:flex;flex-wrap:wrap;gap:.9rem;margin:.6rem 0 0 2.6rem;font-size:.75rem}
.sw{display:inline-block;width:.7rem;height:.7rem;border-radius:2px;margin-right:.3rem;vertical-align:-1px}
.empty{height:220px;display:flex;flex-direction:column;justify-content:center;align-items:center;text-align:center;color:var(--muted-foreground, light-dark(#5b6270, #9aa1ad));font-size:.85rem;background:var(--muted, light-dark(#f3f4f6, #1b1e23));border-radius:var(--radius, 8px)}
.empty-h{font-weight:600;color:var(--foreground, light-dark(#16181d, #e8eaee));margin:0 0 .3rem}
.empty p{margin:0;max-width:28rem;padding:0 1rem}
details{margin-top:1rem;font-size:.75rem;color:var(--muted-foreground, light-dark(#5b6270, #9aa1ad))}
pre{white-space:pre-wrap;word-break:break-word;font-family:var(--font-mono, ui-monospace, Menlo, monospace);margin:.4rem 0 0}
</style></head>
<body><main>
<h1>${esc(title)}</h1>
<p class="sub">${rows.length ? `Six weeks of CI builds on the main branch. W39 is the first full week with the dependency cache enabled.` : "Six-week window, main branch."}</p>
<section data-role="figure">
${figure}
${rows.length ? `<div class="legend"><span><i class="sw" style="background:var(--chart-2, light-dark(#8a94a6, #6b7486))"></i>Before cache</span><span><i class="sw" style="background:var(--chart-1, light-dark(#2563eb, #60a5fa))"></i>With cache</span></div>` : ""}
</section>
<details data-role="method"><summary>How these numbers were computed</summary><pre>${esc(METHOD)}</pre>${rows.length ? `<p>Illustrative sample data shipped with this skill (${rows.length} rows, none dropped).</p>` : ""}</details>
</main></body></html>
`;
writeFileSync(outPath, html);
process.stdout.write(`${outPath}\n`);
