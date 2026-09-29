// Regenerates crates/maho-ai/data/{models,image-models}.json from the pinned senpi
// packages/ai model catalogs (models.generated.ts, image-models.generated.ts).
// Output is generated data; never hand-edit it. Usage: bun tools/golden/gen-models.mjs [--check]
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { pinnedSenpiRoot } from "./pin.mjs";

const senpi = pinnedSenpiRoot();
const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const outDir = join(repo, "crates", "maho-ai", "data");
const aiSrc = join(senpi, "packages", "ai", "src");

const { MODELS } = await import(pathToFileURL(join(aiSrc, "models.generated.ts")).href);
const { IMAGE_MODELS } = await import(pathToFileURL(join(aiSrc, "image-models.generated.ts")).href);

// Key order is preserved as in senpi so provider/model enumeration order matches getProviders()/getModels().
const outputs = {
	"models.json": `${JSON.stringify(MODELS, null, "\t")}\n`,
	"image-models.json": `${JSON.stringify(IMAGE_MODELS, null, "\t")}\n`,
};

const check = process.argv.includes("--check");
let stale = 0;
mkdirSync(outDir, { recursive: true });
for (const [name, content] of Object.entries(outputs)) {
	const path = join(outDir, name);
	if (check) {
		let current = "";
		try {
			current = readFileSync(path, "utf8");
		} catch {}
		if (current !== content) {
			console.error(`stale: ${path}`);
			stale++;
		}
	} else {
		writeFileSync(path, content);
		console.log(`wrote ${path} (${content.length} bytes)`);
	}
}
if (stale > 0) process.exit(1);
