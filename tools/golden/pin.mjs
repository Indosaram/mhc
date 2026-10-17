// Shared senpi pin check for the golden tools. senpi is read-only: we only read files and git metadata.
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";

export const SENPI_PIN = "cf69f5e72eb325219a93e17bb54bbd4ba787899e";

/** Returns the absolute SENPI_SRC path, or exits 3 when it is not a checkout at the pinned commit. */
export function pinnedSenpiRoot() {
	const root = resolve(process.env.SENPI_SRC ?? "/Users/indo/code/senpi");
	let head;
	try {
		head = execFileSync("git", ["-C", root, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
	} catch (error) {
		console.error(`senpi: ${root} is not a git checkout (${error.message.split("\n")[0]})`);
		process.exit(3);
	}
	if (head !== SENPI_PIN) {
		console.error(`senpi: ${root} is at ${head}, expected ${SENPI_PIN}; refusing to generate fixtures`);
		process.exit(3);
	}
	return root;
}
