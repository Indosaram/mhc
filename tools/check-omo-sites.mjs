#!/usr/bin/env bun
// Verifies the todo-2 home-dir rename against tools/omo-sites.json.
//
// (1) COMP at the pinned commit: per-file count of `"\.omo("|/)` literals (same file filter)
//     equals keep+rename, and no unlisted file has any (expected total 34).
// (2) crates/omo in this repo: per-file count of `"\.omo("|/)` equals keep (17) and of
//     `"\.maho("|/)` equals rename (17), and no unlisted file has either.
// (3) Prints both totals. Exits 1 naming every mismatching file.
//
// Usage: bun tools/check-omo-sites.mjs [--root <repo root>] [--comp <COMP git dir>]
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative, resolve, dirname } from "node:path";

const args = process.argv.slice(2);
const option = (name, fallback) => {
	const index = args.indexOf(name);
	return index >= 0 && args[index + 1] ? args[index + 1] : fallback;
};
const root = resolve(option("--root", join(dirname(new URL(import.meta.url).pathname), "..")));
const compGit = option("--comp", "/Volumes/T9-Mac/omo-native-rs/codex/omo-components");

const spec = JSON.parse(readFileSync(join(root, "tools/omo-sites.json"), "utf8"));
const OMO = /"\.omo("|\/)/g;
const MAHO = /"\.maho("|\/)/g;
const exclude = new RegExp(spec.filter.exclude_regex);
const inScope = (file) =>
	file.endsWith(".rs") && spec.filter.roots.some((r) => file.startsWith(`${r}/`)) && !exclude.test(file);
const count = (text, re) => (text.match(re) ?? []).length;

const errors = [];
const listed = new Map(spec.sites.map((s) => [s.file, s]));

// (1) COMP at the pinned commit, read through `git grep <commit>` so the dirty working tree is
// never used. Pathspecs are relative to the COMP directory (git -C cwd).
const git = (...gitArgs) => {
	const result = Bun.spawnSync(["git", "-C", compGit, ...gitArgs], { stdout: "pipe", stderr: "pipe" });
	// git grep exits 1 when nothing matches; anything else is a real failure.
	if (result.exitCode !== 0 && !(gitArgs[0] === "grep" && result.exitCode === 1)) {
		console.error(`git ${gitArgs.join(" ")} failed: ${result.stderr.toString().trim()}`);
		process.exit(2);
	}
	return result.stdout.toString();
};
const prefix = git("rev-parse", "--show-prefix").trim(); // "omo-components/"
const cratesPrefix = `${prefix}crates/`;
const grepOut = git("grep", "-c", "-E", '"\\.omo("|/)', spec.comp_commit, "--", "crates/");
let compTotal = 0;
const compCounts = new Map();
for (const line of grepOut.split("\n").filter(Boolean)) {
	// <commit>:<path relative to the COMP directory>:<count>
	const match = line.match(/^[^:]+:crates\/(.+):(\d+)$/);
	if (!match) continue;
	const file = match[1];
	if (!inScope(file)) continue;
	// git grep -c counts matching lines; count literals per line from the blob instead.
	const n = count(git("show", `${spec.comp_commit}:${cratesPrefix}${file}`), OMO);
	compCounts.set(file, n);
	compTotal += n;
}
for (const [file, n] of compCounts) {
	const site = listed.get(file);
	if (!site) errors.push(`COMP ${file}: ${n} ".omo" literal(s) but file is not listed in omo-sites.json`);
	else if (site.keep + site.rename !== n)
		errors.push(`COMP ${file}: ${n} ".omo" literal(s), expected keep+rename=${site.keep + site.rename}`);
}
for (const site of spec.sites)
	if (!compCounts.has(site.file)) errors.push(`COMP ${site.file}: listed but has no ".omo" literal`);

// (2) crates/omo in this repo.
const nativeAdditions = spec.native_additions ?? [];
const nativeListed = new Map([...spec.sites, ...nativeAdditions].map((site) => [site.file, site]));
const cratesDir = join(root, "crates/omo");
const walk = (dir) =>
	readdirSync(dir).flatMap((name) => {
		const path = join(dir, name);
		return statSync(path).isDirectory() ? walk(path) : [path];
	});
let keepTotal = 0;
let renameTotal = 0;
const seen = new Set();
for (const path of walk(cratesDir)) {
	const file = relative(cratesDir, path);
	if (!inScope(file)) continue;
	const text = readFileSync(path, "utf8");
	const omo = count(text, OMO);
	const maho = count(text, MAHO);
	keepTotal += omo;
	renameTotal += maho;
	if (omo === 0 && maho === 0) continue;
	seen.add(file);
	const site = nativeListed.get(file);
	if (!site) {
		errors.push(`crates/omo/${file}: ".omo"=${omo} ".maho"=${maho} but file is not listed in omo-sites.json`);
		continue;
	}
	if (omo !== site.keep) errors.push(`crates/omo/${file}: ".omo"=${omo}, expected keep=${site.keep}`);
	if (maho !== site.rename) errors.push(`crates/omo/${file}: ".maho"=${maho}, expected rename=${site.rename}`);
}
for (const site of nativeListed.values())
	if (!seen.has(site.file)) errors.push(`crates/omo/${site.file}: listed but has no ".omo"/".maho" literal`);

// (3) Totals.
const expected = spec.expected_totals;
const expectedKeep = expected.keep + nativeAdditions.reduce((sum, site) => sum + site.keep, 0);
const expectedRename = expected.rename + nativeAdditions.reduce((sum, site) => sum + site.rename, 0);
console.log(`COMP ${spec.comp_commit.slice(0, 7)} ".omo" total: ${compTotal} (expected ${expected.comp})`);
console.log(`crates/omo ".omo" (keep) total: ${keepTotal} (expected ${expectedKeep})`);
console.log(`crates/omo ".maho" (rename) total: ${renameTotal} (expected ${expectedRename})`);
if (compTotal !== expected.comp) errors.push(`COMP total ${compTotal} != ${expected.comp}`);
if (keepTotal !== expectedKeep) errors.push(`keep total ${keepTotal} != ${expectedKeep}`);
if (renameTotal !== expectedRename) errors.push(`rename total ${renameTotal} != ${expectedRename}`);

if (errors.length > 0) {
	for (const error of errors) console.error(`FAIL ${error}`);
	process.exit(1);
}
console.log(`OK ${nativeListed.size} files match omo-sites.json`);
