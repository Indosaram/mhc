// Shared senpi pin check for the golden tools. senpi is read-only: we only read files and git metadata.
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";

export const SENPI_PIN = "99a26322f4a81d5ed95f0fd9f292938a6afbd8ff";
/** senpi v2026.11.19-5: matches SENPI_PIN and the commit crates/maho-ai/data/models.json is generated from. */
export const SENPI_CATALOG_PIN = "99a26322f4a81d5ed95f0fd9f292938a6afbd8ff";
/** oh-my-openagent v5.1.81: parity baseline matching PINS.md. */
export const OMO_PIN = "5835a98e88438f28a8ae0ee18039fad4d5fd415b";

/** Returns the absolute SENPI_SRC path, or exits 3 when it is not a checkout at `pin`. */
export function pinnedSenpiRoot(pin = SENPI_PIN) {
	const root = resolve(process.env.SENPI_SRC ?? "/Users/indo/code/senpi");
	let head;
	try {
		head = execFileSync("git", ["-C", root, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
	} catch (error) {
		console.error(`senpi: ${root} is not a git checkout (${error.message.split("\n")[0]})`);
		process.exit(3);
	}
	if (head !== pin) {
		console.error(`senpi: ${root} is at ${head}, expected ${pin}; refusing to generate fixtures`);
		process.exit(3);
	}
	return root;
}
