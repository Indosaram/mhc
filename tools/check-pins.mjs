#!/usr/bin/env bun
// tools/check-pins.mjs
// Verifies agreement among the upstream senpi and oh-my-openagent pins:
// 1. PINS.md: `senpi` and `oh-my-openagent` rows (parity baselines)
// 2. tools/golden/pin.mjs: `SENPI_PIN` and `OMO_PIN`
// 3. .github/workflows/ci.yml & mhc-binary.yml: `SENPI_PIN`
// 4. tools/parity-audit.mjs: `SENPI_PIN` and `OMO_PIN`
//
// Usage:
//   bun tools/check-pins.mjs [--self-test] [--ancestry <senpi-git-dir>] [--omo-ancestry <omo-git-dir>] [--root <repo-root>]
//
// Environment overrides:
//   PIN_CI_OVERRIDE=<sha>   Overrides the CI SENPI_PIN for testing mismatch detection.
//   PIN_OMO_OVERRIDE=<sha>  Overrides the parity-audit OMO_PIN for testing mismatch detection.

import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";

/**
 * Extracts the full 40-character senpi commit SHA from PINS.md content.
 */
export function parsePinsMd(content) {
	const match = content.match(/^\|\s*senpi\s*\|\s*([a-f0-9]+)\s*\|\s*([a-f0-9]{40})\s*\|/m);
	if (!match) {
		throw new Error("Could not find senpi row with 40-character commit in PINS.md");
	}
	return match[2];
}

/**
 * Extracts the full 40-character oh-my-openagent commit SHA from PINS.md content.
 */
export function parsePinsMdOmo(content) {
	const match = content.match(/^\|\s*oh-my-openagent\s*\|\s*([a-f0-9]+)\s*\|\s*([a-f0-9]{40})\s*\|/m);
	if (!match) {
		throw new Error("Could not find oh-my-openagent row with 40-character commit in PINS.md");
	}
	return match[2];
}

/**
 * Extracts SENPI_PIN from tools/golden/pin.mjs content.
 */
export function parsePinMjs(content) {
	const match = content.match(/export\s+const\s+SENPI_PIN\s*=\s*["']([a-f0-9]{40})["']/);
	if (!match) {
		throw new Error("Could not find SENPI_PIN in tools/golden/pin.mjs");
	}
	return match[1];
}

/**
 * Extracts OMO_PIN from tools/golden/pin.mjs content.
 */
export function parsePinMjsOmo(content) {
	const match = content.match(/export\s+const\s+OMO_PIN\s*=\s*["']([a-f0-9]{40})["']/);
	if (!match) {
		throw new Error("Could not find OMO_PIN in tools/golden/pin.mjs");
	}
	return match[1];
}

/**
 * Extracts OMO_PIN from tools/parity-audit.mjs content.
 */
export function parseParityAuditOmo(content) {
	const match = content.match(/const\s+OMO_PIN\s*=\s*["']([a-f0-9]{40})["']/);
	if (!match) {
		throw new Error("Could not find OMO_PIN in tools/parity-audit.mjs");
	}
	return match[1];
}

/**
 * Extracts SENPI_PIN from tools/parity-audit.mjs content.
 */
export function parseParityAuditSenpi(content) {
	const match = content.match(/const\s+SENPI_PIN\s*=\s*["']([a-f0-9]{40})["']/);
	if (!match) {
		throw new Error("Could not find SENPI_PIN in tools/parity-audit.mjs");
	}
	return match[1];
}

/**
 * Extracts SENPI_PIN from .github/workflows/ci.yml content.
 */
export function parseCiYml(content) {
	const match = content.match(/(?:^|\n)\s*SENPI_PIN:\s*["']?([a-f0-9]{40})["']?/);
	if (!match) {
		throw new Error("Could not find SENPI_PIN in .github/workflows/ci.yml");
	}
	return match[1];
}

/**
 * Checks that CI pin equals golden pin.mjs pin.
 */
export function verifyPinAgreement(ciPin, pinMjsPin) {
	if (ciPin !== pinMjsPin) {
		return {
			ok: false,
			error: `PIN mismatch: .github/workflows/ci.yml has SENPI_PIN=${ciPin}, but tools/golden/pin.mjs has SENPI_PIN=${pinMjsPin}`,
		};
	}
	return { ok: true };
}

/**
 * Checks that PINS.md, tools/golden/pin.mjs, and tools/parity-audit.mjs agree on OMO_PIN.
 */
export function verifyOmoPinAgreement(pinsMdOmo, pinMjsOmo, auditOmo) {
	if (pinsMdOmo !== pinMjsOmo) {
		return {
			ok: false,
			error: `OMO PIN mismatch: PINS.md has ${pinsMdOmo}, but tools/golden/pin.mjs has OMO_PIN=${pinMjsOmo}`,
		};
	}
	if (pinsMdOmo !== auditOmo) {
		return {
			ok: false,
			error: `OMO PIN mismatch: PINS.md has ${pinsMdOmo}, but tools/parity-audit.mjs has OMO_PIN=${auditOmo}`,
		};
	}
	return { ok: true };
}

/**
 * Verifies that golden pin is an ancestor of PINS.md senpi pin in the given git repo.
 * Fetches both commits from origin with depth 200 first.
 */
export function verifyAncestry(senpiGitDir, pinMjsPin, pinsMdPin, spawnSync = Bun.spawnSync) {
	for (const sha of [pinMjsPin, pinsMdPin]) {
		const fetchRes = spawnSync(["git", "-C", senpiGitDir, "fetch", "--depth=200", "origin", sha], {
			stdout: "pipe",
			stderr: "pipe",
		});
		if (fetchRes.exitCode !== 0) {
			const err = fetchRes.stderr ? fetchRes.stderr.toString().trim() : "";
			return {
				ok: false,
				error: `Failed to fetch object ${sha} from origin in ${senpiGitDir}: ${err}`,
			};
		}
	}

	const mergeRes = spawnSync(
		["git", "-C", senpiGitDir, "merge-base", "--is-ancestor", pinMjsPin, pinsMdPin],
		{
			stdout: "pipe",
			stderr: "pipe",
		}
	);
	if (mergeRes.exitCode !== 0) {
		const err = mergeRes.stderr ? mergeRes.stderr.toString().trim() : "";
		return {
			ok: false,
			error: `Commit ${pinMjsPin} (pin.mjs) is not an ancestor of ${pinsMdPin} (PINS.md senpi) in ${senpiGitDir}${err ? `: ${err}` : ""}`,
		};
	}

	return { ok: true };
}

/**
 * Runs self-tests using fixture strings and a local git repo.
 */
export function runSelfTest() {
	console.log("Running check-pins.mjs self-tests...");

	// 1. Fixture test for parsePinsMd
	const samplePinsMd = `
# Source pins

| Source | Short | Full commit | Location |
| --- | --- | --- | --- |
| senpi | 26acd9353 | 26acd935303869fd624f203dd8beb41f66ca4e5d | code-yeongyu/senpi |
| oh-my-openagent | 7d818abaf9 | 7d818abaf97831537271b4af9629a5264649de37 | code-yeongyu/oh-my-openagent |
`;
	const parsedPinsMd = parsePinsMd(samplePinsMd);
	if (parsedPinsMd !== "26acd935303869fd624f203dd8beb41f66ca4e5d") {
		throw new Error(`Self-test failed: expected 26acd935303869fd624f203dd8beb41f66ca4e5d, got ${parsedPinsMd}`);
	}

	let threw = false;
	try {
		parsePinsMd("# No pins table here\n| foo | bar |");
	} catch {
		threw = true;
	}
	if (!threw) {
		throw new Error("Self-test failed: parsePinsMd should throw on missing senpi row");
	}

	// 2. Fixture test for parsePinMjs
	const samplePinMjs = `
import { execFileSync } from "node:child_process";
export const SENPI_PIN = "903b83367545bea516f35fc7f7c7cb692847988d";
`;
	const parsedPinMjs = parsePinMjs(samplePinMjs);
	if (parsedPinMjs !== "903b83367545bea516f35fc7f7c7cb692847988d") {
		throw new Error(`Self-test failed: expected 903b83367545bea516f35fc7f7c7cb692847988d, got ${parsedPinMjs}`);
	}

	threw = false;
	try {
		parsePinMjs("export const OTHER_PIN = '1234';");
	} catch {
		threw = true;
	}
	if (!threw) {
		throw new Error("Self-test failed: parsePinMjs should throw on missing SENPI_PIN");
	}

	// 3. Fixture test for parseCiYml
	const sampleCiYml = `
env:
  AST_GREP_VERSION: "0.45.0"
  SENPI_PIN: 903b83367545bea516f35fc7f7c7cb692847988d
jobs:
`;
	const parsedCiYml = parseCiYml(sampleCiYml);
	if (parsedCiYml !== "903b83367545bea516f35fc7f7c7cb692847988d") {
		throw new Error(`Self-test failed: expected 903b83367545bea516f35fc7f7c7cb692847988d, got ${parsedCiYml}`);
	}

	threw = false;
	try {
		parseCiYml("env:\n  FOO: bar\n");
	} catch {
		threw = true;
	}
	if (!threw) {
		throw new Error("Self-test failed: parseCiYml should throw on missing SENPI_PIN");
	}

	// 4. Test verifyPinAgreement
	const agree = verifyPinAgreement(
		"903b83367545bea516f35fc7f7c7cb692847988d",
		"903b83367545bea516f35fc7f7c7cb692847988d"
	);
	if (!agree.ok) {
		throw new Error(`Self-test failed: expected agreement to succeed, got ${agree.error}`);
	}

	const mismatch = verifyPinAgreement(
		"deadbeef",
		"903b83367545bea516f35fc7f7c7cb692847988d"
	);
	if (mismatch.ok) {
		throw new Error("Self-test failed: expected mismatch on deadbeef vs 903b83367545bea516f35fc7f7c7cb692847988d");
	}
	if (!mismatch.error.includes("deadbeef") || !mismatch.error.includes("903b83367545bea516f35fc7f7c7cb692847988d")) {
		throw new Error(`Self-test failed: mismatch error should name both values: ${mismatch.error}`);
	}

	// 5. Test ancestry logic with a temporary local git repository
	const tempDir = mkdtempSync(join(tmpdir(), "check-pins-self-test-"));
	try {
		const runGit = (...args) => {
			const res = Bun.spawnSync(["git", ...args], { cwd: tempDir, stdout: "pipe", stderr: "pipe" });
			if (res.exitCode !== 0) {
				throw new Error(`git ${args.join(" ")} failed: ${res.stderr.toString()}`);
			}
			return res.stdout.toString().trim();
		};

		runGit("init");
		runGit("config", "user.name", "Self Test");
		runGit("config", "user.email", "selftest@example.com");

		writeFileSync(join(tempDir, "file.txt"), "commit 1\n");
		runGit("add", "file.txt");
		runGit("commit", "-m", "commit 1");
		const c1 = runGit("rev-parse", "HEAD");

		writeFileSync(join(tempDir, "file.txt"), "commit 2\n");
		runGit("add", "file.txt");
		runGit("commit", "-m", "commit 2");
		const c2 = runGit("rev-parse", "HEAD");

		// Test merge-base --is-ancestor
		const ancResult = Bun.spawnSync(["git", "-C", tempDir, "merge-base", "--is-ancestor", c1, c2]);
		if (ancResult.exitCode !== 0) {
			throw new Error("Self-test failed: c1 should be ancestor of c2");
		}
		const nonAncResult = Bun.spawnSync(["git", "-C", tempDir, "merge-base", "--is-ancestor", c2, c1]);
		if (nonAncResult.exitCode === 0) {
			throw new Error("Self-test failed: c2 should NOT be ancestor of c1");
		}

		// Test verifyAncestry fetch error handling with dummy spawnSync
		const fakeFetchFail = verifyAncestry(
			tempDir,
			c1,
			c2,
			(cmd) => {
				if (cmd.includes("fetch")) {
					return { exitCode: 128, stderr: Buffer.from("fatal: could not read from remote") };
				}
				return { exitCode: 0, stderr: Buffer.from("") };
			}
		);
		if (fakeFetchFail.ok) {
			throw new Error("Self-test failed: verifyAncestry should fail if fetch fails");
		}
	} finally {
		rmSync(tempDir, { recursive: true, force: true });
	}

	const parsedPinsMdOmo = parsePinsMdOmo(samplePinsMd);
	if (parsedPinsMdOmo !== "7d818abaf97831537271b4af9629a5264649de37") {
		throw new Error(`Self-test failed: expected omo pin, got ${parsedPinsMdOmo}`);
	}

	const omoAgree = verifyOmoPinAgreement(
		"7d818abaf97831537271b4af9629a5264649de37",
		"7d818abaf97831537271b4af9629a5264649de37",
		"7d818abaf97831537271b4af9629a5264649de37"
	);
	if (!omoAgree.ok) {
		throw new Error(`Self-test failed: expected omo agreement to succeed, got ${omoAgree.error}`);
	}
	const omoMismatch = verifyOmoPinAgreement(
		"7d818abaf97831537271b4af9629a5264649de37",
		"7d818abaf97831537271b4af9629a5264649de37",
		"deadbeef"
	);
	if (omoMismatch.ok) {
		throw new Error("Self-test failed: expected omo mismatch");
	}

	console.log("Self-test passed: all assertions verified.");
}

export function main() {
	const args = process.argv.slice(2);
	let selfTest = false;
	let ancestryDir = null;
	let omoAncestryDir = null;
	let rootDir = null;

	for (let i = 0; i < args.length; i++) {
		const arg = args[i];
		if (arg === "--self-test") {
			selfTest = true;
		} else if (arg === "--ancestry") {
			ancestryDir = args[++i];
		} else if (arg.startsWith("--ancestry=")) {
			ancestryDir = arg.slice("--ancestry=".length);
		} else if (arg === "--omo-ancestry") {
			omoAncestryDir = args[++i];
		} else if (arg.startsWith("--omo-ancestry=")) {
			omoAncestryDir = arg.slice("--omo-ancestry=".length);
		} else if (arg === "--root") {
			rootDir = args[++i];
		} else if (arg.startsWith("--root=")) {
			rootDir = arg.slice("--root=".length);
		} else if (arg === "--help" || arg === "-h") {
			console.log("Usage: bun tools/check-pins.mjs [--self-test] [--ancestry <senpi-git-dir>] [--omo-ancestry <omo-git-dir>] [--root <repo-root>]");
			process.exit(0);
		} else {
			console.error(`Unknown argument: ${arg}`);
			process.exit(1);
		}
	}

	if (selfTest) {
		runSelfTest();
		process.exit(0);
	}

	if (!rootDir) {
		rootDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
	}

	const pinsMdPath = join(rootDir, "PINS.md");
	const pinMjsPath = join(rootDir, "tools/golden/pin.mjs");
	const ciYmlPath = join(rootDir, ".github/workflows/ci.yml");
	const binYmlPath = join(rootDir, ".github/workflows/mhc-binary.yml");
	const auditMjsPath = join(rootDir, "tools/parity-audit.mjs");

	for (const p of [pinsMdPath, pinMjsPath, ciYmlPath]) {
		if (!existsSync(p)) {
			console.error(`Error: ${p} does not exist`);
			process.exit(1);
		}
	}

	const pinsMdContent = readFileSync(pinsMdPath, "utf8");
	const pinMjsContent = readFileSync(pinMjsPath, "utf8");
	const ciYmlContent = readFileSync(ciYmlPath, "utf8");

	const pinsMdSenpi = parsePinsMd(pinsMdContent);
	const pinMjsPin = parsePinMjs(pinMjsContent);
	const ciPin = process.env.PIN_CI_OVERRIDE || parseCiYml(ciYmlContent);

	const agreement = verifyPinAgreement(ciPin, pinMjsPin);
	if (!agreement.ok) {
		console.error(agreement.error);
		process.exit(1);
	}

	if (existsSync(binYmlPath)) {
		const binPin = parseCiYml(readFileSync(binYmlPath, "utf8"));
		if (binPin !== pinMjsPin) {
			console.error(`PIN mismatch: .github/workflows/mhc-binary.yml has SENPI_PIN=${binPin}, but tools/golden/pin.mjs has SENPI_PIN=${pinMjsPin}`);
			process.exit(1);
		}
	}

	if (existsSync(auditMjsPath)) {
		const auditContent = readFileSync(auditMjsPath, "utf8");
		const auditSenpi = parseParityAuditSenpi(auditContent);
		if (auditSenpi !== pinsMdSenpi) {
			console.error(`PIN mismatch: tools/parity-audit.mjs has SENPI_PIN=${auditSenpi}, but PINS.md has ${pinsMdSenpi}`);
			process.exit(1);
		}
		const pinsMdOmo = parsePinsMdOmo(pinsMdContent);
		const pinMjsOmo = parsePinMjsOmo(pinMjsContent);
		const auditOmo = process.env.PIN_OMO_OVERRIDE || parseParityAuditOmo(auditContent);
		const omoAgreement = verifyOmoPinAgreement(pinsMdOmo, pinMjsOmo, auditOmo);
		if (!omoAgreement.ok) {
			console.error(omoAgreement.error);
			process.exit(1);
		}
		if (omoAncestryDir) {
			const omoPath = resolve(omoAncestryDir);
			const res = verifyAncestry(omoPath, pinMjsOmo, pinsMdOmo);
			if (!res.ok) {
				console.error(res.error);
				process.exit(1);
			}
		}
	}

	if (ancestryDir) {
		const senpiPath = resolve(ancestryDir);
		if (!existsSync(senpiPath)) {
			console.error(`Error: senpi git directory does not exist: ${senpiPath}`);
			process.exit(1);
		}
		const ancestry = verifyAncestry(senpiPath, pinMjsPin, pinsMdSenpi);
		if (!ancestry.ok) {
			console.error(ancestry.error);
			process.exit(1);
		}
		console.log(
			`OK: CI/binary/parity-audit SENPI_PIN (${ciPin}) and OMO_PIN agree with PINS.md (${pinsMdSenpi} / ${parsePinsMdOmo(pinsMdContent)}) in ${senpiPath}`
		);
	} else {
		console.log(
			`OK: CI/binary/parity-audit SENPI_PIN (${ciPin}) and OMO_PIN agree with PINS.md (${pinsMdSenpi} / ${parsePinsMdOmo(pinsMdContent)})`
		);
	}
}

if (import.meta.url === `file://${process.argv[1]}` || process.argv[1]?.endsWith("check-pins.mjs")) {
	main();
}
