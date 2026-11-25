// Regenerates crates/maho-ai/data/models.json from the senpi packages/ai chat catalog (`MODELS` in
// models.generated.ts) at SENPI_CATALOG_PIN, which may be newer than the parity pin (docs/divergences.md CAT-1).
// Output is generated data; never hand-edit it. Usage: SENPI_SRC=<senpi at SENPI_CATALOG_PIN> bun tools/golden/gen-models.mjs [--check]
//
// The embedded catalog is senpi's `MODELS` with three load-time facts of crates/maho-ai left out:
// - `meta`, `radius` and `typesafe` are not written: maho-ai inserts Meta from providers/data/meta.json, Radius
//   comes from providers/data/radius.json, and Typesafe serves only the classifier catalog.
// - `type: "chat"` is not written: every `MODELS` row is a chat model.
// - an `inputLimits` that is only the default image resize is not written: maho-ai's MODELS loader applies it to
//   every image-input model.
// crates/maho-ai/data/image-models.json predates senpi's unified catalog (senpi#9948) and is not written here.
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { pinnedSenpiRoot, SENPI_CATALOG_PIN } from "./pin.mjs";

const senpi = pinnedSenpiRoot(SENPI_CATALOG_PIN);
const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const path = join(repo, "crates", "maho-ai", "data", "models.json");

const { MODELS } = await import(pathToFileURL(join(senpi, "packages", "ai", "src", "models.generated.ts")).href);

const LOADED_ELSEWHERE = new Set(["meta", "radius", "typesafe"]);
const DEFAULT_IMAGE_LIMITS = JSON.stringify({
	images: { resize: { maxWidth: 2000, maxHeight: 2000, maxBytes: 4718592, jpegQuality: 80 } },
});

// Key order is preserved as in senpi so provider/model enumeration order matches getProviders()/getModels().
const catalog = {};
for (const [provider, models] of Object.entries(MODELS)) {
	if (LOADED_ELSEWHERE.has(provider)) continue;
	catalog[provider] = {};
	for (const [id, model] of Object.entries(models)) {
		const { type, ...row } = model;
		if (type !== "chat") throw new Error(`${provider}/${id}: unexpected model type ${type}`);
		if (JSON.stringify(row.inputLimits) === DEFAULT_IMAGE_LIMITS) delete row.inputLimits;
		catalog[provider][id] = row;
	}
}
const content = `${JSON.stringify(catalog, null, "\t")}\n`;

if (process.argv.includes("--check")) {
	let current = "";
	try {
		current = readFileSync(path, "utf8");
	} catch {}
	if (current !== content) {
		console.error(`stale: ${path}`);
		process.exit(1);
	}
} else {
	writeFileSync(path, content);
	console.log(`wrote ${path} (${content.length} bytes)`);
}
