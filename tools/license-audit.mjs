#!/usr/bin/env bun
// Audit repository crates, metadata, and staged packages for license consistency with SUL-1.0.
//
//   bun tools/license-audit.mjs [--root <dir>]
//   bun tools/license-audit.mjs --package <staging-dir> [--root <dir>]
//   bun tools/license-audit.mjs --self-test
//
// Exit 0 = pass, 1 = failure, 2 = usage error.

import { existsSync, readdirSync, readFileSync, statSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { dirname, join, relative, resolve, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";

const here = dirname(fileURLToPath(import.meta.url));
const defaultRepoRoot = resolve(join(here, ".."));

export const REQUIRED_PACKAGE_LICENSES = [
	{ name: "MIT.txt", source: "LICENSES/MIT.txt" },
	{ name: "LICENSE", source: "LICENSE" },
	{ name: "THIRD_PARTY_LICENSES.md", source: "THIRD_PARTY_LICENSES.md" },
	{ name: "lazycodex-MIT.txt", source: "LICENSES/lazycodex-MIT.txt" },
	{ name: "maho-ext-webfetch-LICENSE-READABILITY.txt", source: "crates/builtins/maho-ext-webfetch/LICENSE-READABILITY.txt" },
	{ name: "maho-ext-webfetch-LICENSE-TURNDOWN.txt", source: "crates/builtins/maho-ext-webfetch/LICENSE-TURNDOWN.txt" },
	{ name: "maho-ext-webfetch-LICENSE-HTMLPARSER2.txt", source: "crates/builtins/maho-ext-webfetch/LICENSE-HTMLPARSER2.txt" },
	{ name: "maho-ext-omo-zcode-oauth-LICENSE", source: "crates/extensions/maho-ext-omo-zcode-oauth/LICENSE" },
	{ name: "maho-ext-pi-webfetch-THIRD_PARTY_NOTICES.md", source: "crates/extensions/maho-ext-pi-webfetch/THIRD_PARTY_NOTICES.md" },
	{ name: "maho-ext-pi-rules-THIRD_PARTY_NOTICES.md", source: "crates/extensions/maho-ext-pi-rules/THIRD_PARTY_NOTICES.md" },
	{ name: "frontend-LICENSE-Apache-2.0.txt", source: "assets/skills/frontend/LICENSE-Apache-2.0.txt" },
	{ name: "ast-grep-LICENSE", source: "assets/skills/ast-grep/LICENSE" },
	{ name: "node-omowright-LICENSE", source: "LICENSES/node/omowright-LICENSE" },
	{ name: "node-ws-LICENSE", source: "LICENSES/node/ws-LICENSE" },
	{ name: "node-zod-LICENSE", source: "LICENSES/node/zod-LICENSE" },
];

function usage() {
	console.log(`usage: bun tools/license-audit.mjs [--root <dir>]
       bun tools/license-audit.mjs --package <staging-dir> [--root <dir>]
       bun tools/license-audit.mjs --self-test`);
}

function usageError(message) {
	console.error(`license-audit: ${message}`);
	usage();
	process.exit(2);
}

/** Recursively collect files matching a predicate. */
function collectFiles(dir, filterFn) {
	const results = [];
	if (!existsSync(dir)) return results;
	const entries = readdirSync(dir, { withFileTypes: true });
	for (const entry of entries) {
		const fullPath = join(dir, entry.name);
		if (entry.isDirectory()) {
			if (entry.name === ".git" || entry.name === "target" || entry.name === "node_modules") continue;
			results.push(...collectFiles(fullPath, filterFn));
		} else if (entry.isFile()) {
			if (filterFn(fullPath, entry.name)) {
				results.push(fullPath);
			}
		}
	}
	return results;
}

/** Parse minimal Cargo.toml fields from [package] section. */
export function parseCargoPackage(cargoTomlContent) {
	const lines = cargoTomlContent.split("\n");
	let inPackage = false;
	let name = null;
	let license = null;

	for (const rawLine of lines) {
		const line = rawLine.trim();
		if (line.startsWith("[") && line.endsWith("]")) {
			inPackage = line === "[package]";
			continue;
		}
		if (inPackage) {
			const nameMatch = line.match(/^name\s*=\s*"([^"]+)"/);
			if (nameMatch) {
				name = nameMatch[1];
			}
			const licenseMatch = line.match(/^license\s*=\s*"([^"]+)"/);
			if (licenseMatch) {
				license = licenseMatch[1];
			}
		}
	}
	return { name, license };
}

/** Detect required license identifier(s) from a license/notice file content. */
export function detectRequiredLicenses(content) {
	const licenses = new Set();
	if (/Apache License,\s*Version 2\.0|http:\/\/www\.apache\.org\/licenses\/LICENSE-2\.0|SPDX-License-Identifier:\s*Apache-2\.0/i.test(content)) {
		licenses.add("Apache-2.0");
	}
	if (/MIT License|The MIT License|Permission is hereby granted, free of charge|SPDX-License-Identifier:\s*MIT/i.test(content)) {
		licenses.add("MIT");
	}
	if (/BSD 3-Clause|BSD-3-Clause/i.test(content)) {
		licenses.add("BSD-3-Clause");
	}
	if (/BSD 2-Clause|BSD-2-Clause/i.test(content)) {
		licenses.add("BSD-2-Clause");
	}
	if (/ISC License|SPDX-License-Identifier:\s*ISC/i.test(content)) {
		licenses.add("ISC");
	}
	return Array.from(licenses);
}

/** Check whether a Cargo license expression satisfies a required license identifier. */
export function licenseExpressionSatisfies(expression, requiredLicense) {
	if (!expression) return false;
	const tokens = expression.split(/\s+(?:AND|OR)\s+|[()\s/]+/).map((t) => t.trim()).filter(Boolean);
	return tokens.includes(requiredLicense);
}

/**
 * Audit workspace crates for license conformance.
 * Returns array of error strings (empty if passed).
 */
export function auditCrates(repoRoot) {
	const errors = [];
	const cratesDir = join(repoRoot, "crates");
	if (!existsSync(cratesDir)) {
		errors.push(`crates directory not found at ${cratesDir}`);
		return errors;
	}

	const omoDir = join(cratesDir, "omo");
	if (existsSync(omoDir)) {
		const omoCargoFiles = collectFiles(omoDir, (_, name) => name === "Cargo.toml");
		for (const cargoPath of omoCargoFiles) {
			const content = readFileSync(cargoPath, "utf8");
			const { name, license } = parseCargoPackage(content);
			if (!name) continue;

			const relCrate = relative(cratesDir, dirname(cargoPath)).replaceAll("\\", "/");
			if (license !== "LicenseRef-SUL-1.0") {
				errors.push(`crate ${name} (${relCrate}) under crates/omo/ must have license = "LicenseRef-SUL-1.0", found "${license ?? "<none>"}" (MIT crates live outside crates/omo, e.g. crates/desktop)`);
			}
		}
	}

	// crates/desktop/** holds the MIT senpi-desktop-* ports (moved out of crates/omo so path-based
	// SUL checks over crates/omo stay exact).
	const desktopDir = join(cratesDir, "desktop");
	if (existsSync(desktopDir)) {
		const desktopCargoFiles = collectFiles(desktopDir, (_, name) => name === "Cargo.toml");
		for (const cargoPath of desktopCargoFiles) {
			const content = readFileSync(cargoPath, "utf8");
			const { name, license } = parseCargoPackage(content);
			if (!name) continue;

			const relCrate = relative(cratesDir, dirname(cargoPath)).replaceAll("\\", "/");
			if (license !== "MIT") {
				errors.push(`crate ${name} (${relCrate}) under crates/desktop/ must have license = "MIT", found "${license ?? "<none>"}"`);
			}
		}
	}

	// Check bundled notice files vs Cargo license across all crates
	const allCargoFiles = collectFiles(cratesDir, (_, name) => name === "Cargo.toml");
	for (const cargoPath of allCargoFiles) {
		const crateDir = dirname(cargoPath);
		const cargoContent = readFileSync(cargoPath, "utf8");
		const { name, license } = parseCargoPackage(cargoContent);
		if (!name) continue;

		// Check for LICENSE-* or THIRD_PARTY_NOTICES* inside crateDir
		const noticeFiles = readdirSync(crateDir, { withFileTypes: true })
			.filter((e) => e.isFile() && (/^LICENSE-.+/i.test(e.name) || /^THIRD_PARTY_NOTICES.*/i.test(e.name)))
			.map((e) => join(crateDir, e.name));

		for (const noticePath of noticeFiles) {
			const noticeContent = readFileSync(noticePath, "utf8");
			const required = detectRequiredLicenses(noticeContent);
			const fileName = basename(noticePath);

			for (const req of required) {
				if (!licenseExpressionSatisfies(license, req)) {
					errors.push(`crate ${name} contains ${fileName} requiring license "${req}", but its Cargo license "${license ?? "<none>"}" does not include it`);
				}
			}
		}
	}

	return errors;
}

/**
 * Audit staged package directory and its manifest.
 * Returns array of error strings (empty if passed).
 */
export function auditPackage(stagingDir) {
	const errors = [];
	if (!existsSync(stagingDir)) {
		errors.push(`staged package directory not found: ${stagingDir}`);
		return errors;
	}

	const licensesDir = join(stagingDir, "licenses");
	if (!existsSync(licensesDir)) {
		errors.push(`staged licenses directory missing: ${licensesDir}`);
	}

	const manifestPath = join(stagingDir, "manifest.json");
	let manifest = null;
	if (!existsSync(manifestPath)) {
		errors.push(`staged manifest.json missing: ${manifestPath}`);
	} else {
		try {
			manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
		} catch (err) {
			errors.push(`failed to parse staged manifest.json: ${err.message}`);
		}
	}

	const manifestLicenses = Array.isArray(manifest?.licenses) ? manifest.licenses : [];

	for (const req of REQUIRED_PACKAGE_LICENSES) {
		const stagedFilePath = join(licensesDir, req.name);
		if (!existsSync(stagedFilePath)) {
			errors.push(`staged license file missing: licenses/${req.name} (source: ${req.source})`);
		}

		if (manifest) {
			const expectedRel = `licenses/${req.name}`;
			const foundInManifest = manifestLicenses.some(
				(entry) => entry?.path === expectedRel || entry?.source === req.source || entry?.path === req.name
			);
			if (!foundInManifest) {
				errors.push(`manifest.json missing required license entry: ${expectedRel} (source: ${req.source})`);
			}
		}
	}

	return errors;
}

/** Run in-memory and temp-fixture self tests. */
export async function runSelfTest() {
	let passed = 0;
	let total = 0;

	function check(desc, condition) {
		total++;
		if (!condition) {
			throw new Error(`FAIL: ${desc}`);
		}
		console.log(`ok   ${desc}`);
		passed++;
	}

	const testDir = join(tmpdir(), `.license-audit-selftest-${Date.now()}-${Math.random().toString(36).slice(2)}`);
	mkdirSync(testDir, { recursive: true });

	try {
		// Fixture 1: Happy path repo
		const happyRepo = join(testDir, "happy-repo");
		mkdirSync(join(happyRepo, "crates/omo/some-sul-crate"), { recursive: true });
		writeFileSync(
			join(happyRepo, "crates/omo/some-sul-crate/Cargo.toml"),
			`[package]\nname = "some-sul-crate"\nversion = "0.1.0"\nlicense = "LicenseRef-SUL-1.0"\n`
		);
		mkdirSync(join(happyRepo, "crates/desktop/senpi-desktop-core"), { recursive: true });
		writeFileSync(
			join(happyRepo, "crates/desktop/senpi-desktop-core/Cargo.toml"),
			`[package]\nname = "senpi-desktop-core"\nversion = "0.1.0"\nlicense = "MIT"\n`
		);
		mkdirSync(join(happyRepo, "crates/builtins/maho-ext-webfetch"), { recursive: true });
		writeFileSync(
			join(happyRepo, "crates/builtins/maho-ext-webfetch/Cargo.toml"),
			`[package]\nname = "maho-ext-webfetch"\nversion = "0.1.0"\nlicense = "MIT AND Apache-2.0"\n`
		);
		writeFileSync(
			join(happyRepo, "crates/builtins/maho-ext-webfetch/LICENSE-READABILITY.txt"),
			`Licensed under the Apache License, Version 2.0 (the "License");\n`
		);

		const happyErrors = auditCrates(happyRepo);
		check("happy path crate audit passes", happyErrors.length === 0);

		// Fixture 2: Crates under omo with non-SUL license fail
		const badOmoRepo = join(testDir, "bad-omo-repo");
		mkdirSync(join(badOmoRepo, "crates/omo/bad-sul-crate"), { recursive: true });
		writeFileSync(
			join(badOmoRepo, "crates/omo/bad-sul-crate/Cargo.toml"),
			`[package]\nname = "bad-sul-crate"\nversion = "0.1.0"\nlicense = "MIT"\n`
		);
		const badOmoErrors = auditCrates(badOmoRepo);
		check(
			"omo crate with MIT license fails",
			badOmoErrors.length > 0 && badOmoErrors.some((e) => e.includes("bad-sul-crate") && e.includes("LicenseRef-SUL-1.0"))
		);

		// Fixture 3: senpi-desktop crate with non-MIT license fails
		const badDesktopRepo = join(testDir, "bad-desktop-repo");
		mkdirSync(join(badDesktopRepo, "crates/desktop/senpi-desktop-tool"), { recursive: true });
		writeFileSync(
			join(badDesktopRepo, "crates/desktop/senpi-desktop-tool/Cargo.toml"),
			`[package]\nname = "senpi-desktop-tool"\nversion = "0.1.0"\nlicense = "LicenseRef-SUL-1.0"\n`
		);
		const badDesktopErrors = auditCrates(badDesktopRepo);
		check(
			"senpi-desktop crate with SUL license fails",
			badDesktopErrors.length > 0 && badDesktopErrors.some((e) => e.includes("senpi-desktop-tool") && e.includes("MIT"))
		);

		// Fixture 4: Bundled Apache-2.0 notice missing from Cargo license fails naming crate
		const mismatchRepo = join(testDir, "mismatch-repo");
		mkdirSync(join(mismatchRepo, "crates/builtins/maho-ext-webfetch"), { recursive: true });
		writeFileSync(
			join(mismatchRepo, "crates/builtins/maho-ext-webfetch/Cargo.toml"),
			`[package]\nname = "maho-ext-webfetch"\nversion = "0.1.0"\nlicense = "MIT"\n`
		);
		writeFileSync(
			join(mismatchRepo, "crates/builtins/maho-ext-webfetch/LICENSE-READABILITY.txt"),
			`Licensed under the Apache License, Version 2.0 (the "License");\n`
		);
		const mismatchErrors = auditCrates(mismatchRepo);
		check(
			"crate with unstated bundled Apache-2.0 notice fails naming maho-ext-webfetch",
			mismatchErrors.length > 0 && mismatchErrors.some((e) => e.includes("maho-ext-webfetch") && e.includes("Apache-2.0"))
		);

		// Fixture 5: Staged package happy path
		const happyStaging = join(testDir, "happy-staging");
		mkdirSync(join(happyStaging, "licenses"), { recursive: true });
		const manifestLicenses = [];
		for (const req of REQUIRED_PACKAGE_LICENSES) {
			writeFileSync(join(happyStaging, "licenses", req.name), `fixture for ${req.name}\n`);
			manifestLicenses.push({ path: `licenses/${req.name}`, source: req.source });
		}
		writeFileSync(
			join(happyStaging, "manifest.json"),
			JSON.stringify({ schemaVersion: 1, licenses: manifestLicenses }, null, 2)
		);
		const happyPkgErrors = auditPackage(happyStaging);
		check("staged package with all licenses passes", happyPkgErrors.length === 0);

		// Fixture 6: Staged package missing a license file fails
		const missingFileStaging = join(testDir, "missing-file-staging");
		mkdirSync(join(missingFileStaging, "licenses"), { recursive: true });
		const partialLicenses = manifestLicenses.filter((l) => !l.path.includes("node-ws-LICENSE"));
		for (const req of REQUIRED_PACKAGE_LICENSES) {
			if (req.name !== "node-ws-LICENSE") {
				writeFileSync(join(missingFileStaging, "licenses", req.name), `fixture\n`);
			}
		}
		writeFileSync(
			join(missingFileStaging, "manifest.json"),
			JSON.stringify({ schemaVersion: 1, licenses: partialLicenses }, null, 2)
		);
		const missingFileErrors = auditPackage(missingFileStaging);
		check(
			"staged package missing license file fails naming node-ws-LICENSE",
			missingFileErrors.length > 0 && missingFileErrors.some((e) => e.includes("node-ws-LICENSE"))
		);

		// Fixture 7: Staged package with file on disk but omitted from manifest fails
		const omittedManifestStaging = join(testDir, "omitted-manifest-staging");
		mkdirSync(join(omittedManifestStaging, "licenses"), { recursive: true });
		for (const req of REQUIRED_PACKAGE_LICENSES) {
			writeFileSync(join(omittedManifestStaging, "licenses", req.name), `fixture\n`);
		}
		writeFileSync(
			join(omittedManifestStaging, "manifest.json"),
			JSON.stringify({ schemaVersion: 1, licenses: partialLicenses }, null, 2)
		);
		const omittedManifestErrors = auditPackage(omittedManifestStaging);
		check(
			"staged package with file omitted from manifest fails",
			omittedManifestErrors.length > 0 && omittedManifestErrors.some((e) => e.includes("manifest.json missing"))
		);

	} finally {
		rmSync(testDir, { recursive: true, force: true });
	}

	console.log(`self-test: ${passed}/${total} passed`);
	return passed === total;
}

// CLI entry point
if (import.meta.main) {
	const args = process.argv.slice(2);
	let repoRoot = defaultRepoRoot;
	let stagingDir = null;
	let selfTest = false;

	for (let i = 0; i < args.length; i++) {
		const arg = args[i];
		if (arg === "--help" || arg === "-h") {
			usage();
			process.exit(0);
		} else if (arg === "--self-test") {
			selfTest = true;
		} else if (arg === "--root") {
			if (i + 1 >= args.length) usageError("--root requires a directory argument");
			repoRoot = resolve(args[++i]);
		} else if (arg === "--package") {
			if (i + 1 >= args.length) usageError("--package requires a directory argument");
			stagingDir = resolve(args[++i]);
		} else {
			usageError(`unrecognized argument: ${arg}`);
		}
	}

	if (selfTest) {
		const ok = await runSelfTest();
		process.exit(ok ? 0 : 1);
	}

	const allErrors = [];
	allErrors.push(...auditCrates(repoRoot));

	if (stagingDir) {
		allErrors.push(...auditPackage(stagingDir));
	}

	if (allErrors.length > 0) {
		console.error("license-audit: license policy violations detected:\n");
		for (const err of allErrors) {
			console.error(`  - ${err}`);
		}
		process.exit(1);
	}

	console.log("license-audit: all workspace crates and notices conform to license policy");
	process.exit(0);
}
