// Barrier for the rules matcher golden generator. The pinned picomatch parser is the
// only source of truth for the corpus: refuse to generate unless the checkout is at the
// pinned pi-rules commit and the resolved picomatch version is the pinned one.
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";

export const PI_RULES_PIN = "12ad906f0b29e949ebbd1f89d8f85789578aa6e6";
export const PICOMATCH_VERSION = "4.0.5";

export function pinnedPicomatch() {
	const root = resolve(process.env.PI_RULES_SRC ?? "/home/indo/.omo/agent/git/github.com/code-yeongyu/pi-rules");
	let head;
	try {
		head = execFileSync("git", ["-C", root, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
	} catch (error) {
		console.error(`pi-rules: ${root} is not a git checkout (${error.message.split("\n")[0]})`);
		process.exit(3);
	}
	if (head !== PI_RULES_PIN) {
		console.error(`pi-rules: ${root} is at ${head}, expected ${PI_RULES_PIN}; refusing to generate fixtures`);
		process.exit(3);
	}
	const require = createRequire(`${root}/package.json`);
	// picomatch@4.0.5 has no `exports` map and its `main` is `index.js` at the package
	// root, so the resolved entry's directory IS the package root: the manifest and
	// `lib/` sit beside it. `resolve(entry, "..", "..", "package.json")` reads the
	// *parent* package.json instead, and `resolve(entry, "..")` is the package root,
	// not `lib/`.
	const entry = require.resolve("picomatch");
	const packageDir = dirname(entry);
	const packagePath = join(packageDir, "package.json");
	const manifest = JSON.parse(readFileSync(packagePath, "utf8"));
	if (manifest.version !== PICOMATCH_VERSION) {
		console.error(`picomatch: ${manifest.version} != ${PICOMATCH_VERSION}; refusing to generate fixtures`);
		process.exit(3);
	}
	return { root, picomatch: require("picomatch"), version: manifest.version, packagePath, packageDir, libDir: join(packageDir, "lib") };
}
