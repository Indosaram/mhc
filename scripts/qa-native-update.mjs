#!/usr/bin/env bun
import { createHash } from "node:crypto";
import { chmodSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, renameSync, rmSync, statSync, watch, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createUpdateFixtureServer, detectTargetTuple } from "./qa-native-update-fixture.mjs";

const args = process.argv.slice(2);
let oldPackagePath = null;
let newPackagePath = null;
let evidenceDir = null;
let targetTuple = detectTargetTuple();
let timeoutMs = 45000;
let oldVersion = null;
let newVersion = null;
let upgradeOnly = false;
let rollbackProbe = null;

for (let i = 0; i < args.length; i++) {
	if (args[i] === "--old-package" && i + 1 < args.length) {
		oldPackagePath = resolve(args[++i]);
	} else if (args[i] === "--new-package" && i + 1 < args.length) {
		newPackagePath = resolve(args[++i]);
	} else if (args[i] === "--evidence-dir" && i + 1 < args.length) {
		evidenceDir = resolve(args[++i]);
	} else if (args[i] === "--target" && i + 1 < args.length) {
		targetTuple = args[++i];
	} else if (args[i] === "--timeout" && i + 1 < args.length) {
		timeoutMs = Number.parseInt(args[++i], 10);
	} else if (args[i] === "--old-version" && i + 1 < args.length) {
		oldVersion = args[++i];
	} else if (args[i] === "--new-version" && i + 1 < args.length) {
		newVersion = args[++i];
	} else if (args[i] === "--upgrade-only") {
		upgradeOnly = true;
	} else if (args[i] === "--rollback-probe" && i + 1 < args.length) {
		rollbackProbe = resolve(args[++i]);
	} else {
		throw new Error(`Unknown or incomplete argument: ${args[i]}`);
	}
}

if (!oldPackagePath || !newPackagePath || !evidenceDir || !oldVersion || !newVersion) {
	console.error("Usage: bun scripts/qa-native-update.mjs --old-package <A> --new-package <B> --old-version <A> --new-version <B> --evidence-dir <dir>");
	process.exit(2);
}
if (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0 || targetTuple !== detectTargetTuple()) {
	throw new Error("Expected a positive bounded timeout and the host's native target tuple");
}
if (oldVersion === newVersion) throw new Error("Candidate versions A and B must differ");
if (!upgradeOnly && (!rollbackProbe || !existsSync(rollbackProbe))) throw new Error("Full QA requires --rollback-probe native executable");

if (!existsSync(oldPackagePath)) {
	console.error(`Old package archive does not exist: ${oldPackagePath}`);
	process.exit(1);
}
if (!existsSync(newPackagePath)) {
	console.error(`New package archive does not exist: ${newPackagePath}`);
	process.exit(1);
}

mkdirSync(evidenceDir, { recursive: true });

const isWindows = process.platform === "win32";
const exeSuffix = isWindows ? ".exe" : "";
const binaryName = `mhc${exeSuffix}`;

function computeSha256(filePath) {
	if (!filePath || !existsSync(filePath)) return null;
	return createHash("sha256").update(readFileSync(filePath)).digest("hex");
}

async function boundedExit(proc) {
	let timer;
	try {
		return await Promise.race([
			proc.exited,
			new Promise((_, reject) => {
				timer = setTimeout(() => {
					if (isWindows) {
						Promise.resolve(proc.kill("SIGKILL")).then(
							() => reject(new Error("Child process exit timed out")),
							error => reject(error),
						);
						return;
					}
					proc.kill("SIGKILL");
					reject(new Error("Child process exit timed out"));
				}, timeoutMs);
			}),
		]);
	} finally {
		clearTimeout(timer);
	}
}

async function extractArchive(archivePath, destination) {
	mkdirSync(destination, { recursive: true });
	if (archivePath.endsWith(".zip")) {
		if (isWindows) {
			const proc = Bun.spawn(["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; Expand-Archive -LiteralPath $env:MHC_QA_ARCHIVE -DestinationPath $env:MHC_QA_EXTRACT -Force"], {
				env: { ...process.env, MHC_QA_ARCHIVE: archivePath, MHC_QA_EXTRACT: destination },
			});
			const code = await boundedExit(proc);
			if (code !== 0) throw new Error(`Expand-Archive failed with code ${code}`);
		} else {
			const proc = Bun.spawn(["unzip", "-q", "-o", archivePath, "-d", destination]);
			const code = await boundedExit(proc);
			if (code !== 0) throw new Error(`unzip failed with code ${code}`);
		}
	} else {
		const proc = Bun.spawn(["tar", "-xzf", archivePath, "-C", destination]);
		const code = await boundedExit(proc);
		if (code !== 0) throw new Error(`tar extraction failed with code ${code}`);
	}

	const topEntries = readdirSync(destination);
	if (topEntries.length === 1 && statSync(join(destination, topEntries[0])).isDirectory()) {
		const innerDir = join(destination, topEntries[0]);
		const innerEntries = readdirSync(innerDir);
		for (const entry of innerEntries) {
			const src = join(innerDir, entry);
			const dst = join(destination, entry);
			if (existsSync(dst)) {
				rmSync(dst, { recursive: true, force: true });
			}
			renameSync(src, dst);
		}
		rmSync(innerDir, { recursive: true, force: true });
	}
}

async function executeCommand(binaryPath, cmdArgs, options = {}) {
	const {
		cwd = dirname(binaryPath),
		env = {},
		stdin = "",
		timeout = isWindows ? Math.max(timeoutMs, 150000) : timeoutMs,
	} = options;

	const homeDir = options.homeDir || tmpdir();
	const tmpDir = options.tmpDir || tmpdir();
	const mergedEnv = {
		SystemRoot: process.env.SystemRoot,
		WINDIR: process.env.WINDIR,
		COMSPEC: process.env.COMSPEC,
		PATH: process.env.PATH,
		TMPDIR: tmpDir,
		TMP: tmpDir,
		TEMP: tmpDir,
		HOME: homeDir,
		USERPROFILE: homeDir,
		...env,
	};

	const proc = Bun.spawn([binaryPath, ...cmdArgs], {
		cwd,
		env: mergedEnv,
		stdin: typeof stdin === "string" ? "pipe" : stdin,
		stdout: "pipe",
		stderr: "pipe",
	});

	if (typeof stdin === "string") {
		proc.stdin.write(stdin);
		proc.stdin.flush();
		proc.stdin.end();
	}

	let killed = false;
	const killTimer = setTimeout(() => {
		killed = true;
		proc.kill("SIGKILL");
	}, timeout);

	let completionTimer;
	try {
		const [stdout, stderr, exitCode] = await Promise.race([
			Promise.all([new Response(proc.stdout).text(), new Response(proc.stderr).text(), proc.exited]),
			new Promise((_, reject) => {
				completionTimer = setTimeout(() => reject(new Error("Command output/exit timed out")), timeout + 5000);
			}),
		]);
		return { stdout, stderr, exitCode, killed };
	} finally {
		clearTimeout(killTimer);
		clearTimeout(completionTimer);
	}
}

function eventWaiter(capture, predicate, label, timeoutLimitMs = 15000) {
	return new Promise((resolveWait) => {
		let timer;
		const onData = () => {
			if (!predicate(capture.raw)) return;
			if (timer) clearTimeout(timer);
			capture.listeners.delete(onData);
			resolveWait(true);
		};
		timer = setTimeout(() => {
			capture.listeners.delete(onData);
			resolveWait(false);
		}, timeoutLimitMs);
		capture.listeners.add(onData);
		onData();
	});
}

function waitForFileAppearance(directoryPath, targetFileName, timeoutMs = 8000) {
	const targetPath = join(directoryPath, targetFileName);
	if (existsSync(targetPath)) {
		return Promise.resolve(true);
	}
	return new Promise((resolveWait) => {
		let watcher = null;
		let timer = null;

		const cleanup = (result) => {
			if (timer) clearTimeout(timer);
			if (watcher) {
				try { watcher.close(); } catch {}
				watcher = null;
			}
			resolveWait(result);
		};

		timer = setTimeout(() => cleanup(false), timeoutMs);

		try {
			watcher = watch(directoryPath, (_eventType, filename) => {
				if (existsSync(targetPath)) {
					cleanup(true);
				}
			});
		} catch {
			cleanup(false);
			return;
		}

		if (existsSync(targetPath)) {
			cleanup(true);
		}
	});
}

function collectAllFiles(dir, baseDir = dir) {
	const result = new Map();
	if (!existsSync(dir)) return result;
	const entries = readdirSync(dir, { withFileTypes: true });
	for (const entry of entries) {
		const full = join(dir, entry.name);
		if (entry.isDirectory()) {
			const sub = collectAllFiles(full, baseDir);
			for (const [k, v] of sub) result.set(k, v);
		} else if (entry.isFile()) {
			const rel = relative(baseDir, full).replace(/\\/g, "/");
			result.set(rel, {
				relPath: rel,
				fullPath: full,
				size: statSync(full).size,
				sha256: computeSha256(full),
			});
		} else {
			throw new Error(`Non-regular package entry: ${full}`);
		}
	}
	return result;
}

function observeUpdateCleanup(directoryPath) {
	let check = () => {};
	const watcher = watch(directoryPath, () => check());
	return {
		async afterUpdate() {
			return new Promise((resolveWait) => {
				const timer = setTimeout(() => {
					watcher.close();
					resolveWait(false);
				}, timeoutMs);
				check = () => {
					if (readdirSync(directoryPath).some(name => name.startsWith(".mhc-update-") || name.startsWith(".mhc-backup-"))) return;
					clearTimeout(timer);
					watcher.close();
					resolveWait(true);
				};
				check();
			});
		},
		close() { watcher.close(); },
	};
}

function verifyPackageIntegrity(dir) {
	const manifestPath = join(dir, "manifest.json");
	if (!existsSync(manifestPath)) {
		return { ok: false, error: "manifest.json missing in install directory" };
	}
	let manifest;
	try {
		manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
	} catch (e) {
		return { ok: false, error: `Invalid manifest.json: ${e.message}` };
	}

	const expectedEntries = new Map();
	const addEntry = (entry) => {
		if (entry && typeof entry.path === "string" && /^[a-fA-F0-9]{64}$/.test(entry.sha256) && Number.isSafeInteger(entry.size)) {
			const normalized = entry.path.replace(/\\/g, "/");
			if (expectedEntries.has(normalized)) throw new Error(`Duplicate manifest entry: ${normalized}`);
			expectedEntries.set(normalized, {
				size: entry.size,
				sha256: entry.sha256.toLowerCase(),
				mode: entry.mode,
			});
		} else {
			throw new Error("Invalid manifest file entry");
		}
	};

	const executables = Array.isArray(manifest.executables) ? manifest.executables : [];
	const licenses = Array.isArray(manifest.licenses) ? manifest.licenses : [];
	const schema = manifest.schema ? [manifest.schema] : [];
	const skills = Array.isArray(manifest.skills?.files) ? manifest.skills.files : [];
	const condSkills = Array.isArray(manifest.conditionalSkills?.files) ? manifest.conditionalSkills.files : [];

	for (const e of executables) addEntry(e);
	for (const e of licenses) addEntry(e);
	for (const e of schema) addEntry(e);
	for (const e of skills) addEntry(e);
	for (const e of condSkills) addEntry(e);

	const actualFiles = collectAllFiles(dir);

	if (!actualFiles.has("manifest.json")) {
		return { ok: false, error: "manifest.json missing from file scan" };
	}

	for (const [relPath, expected] of expectedEntries) {
		const actual = actualFiles.get(relPath);
		if (!actual) {
			return { ok: false, error: `Missing expected package file: ${relPath}` };
		}
		if (expected.size !== undefined && actual.size !== expected.size) {
			return { ok: false, error: `Size mismatch for ${relPath}: expected ${expected.size}, got ${actual.size}` };
		}
		if (actual.sha256.toLowerCase() !== expected.sha256) {
			return { ok: false, error: `SHA256 mismatch for ${relPath}: expected ${expected.sha256}, got ${actual.sha256}` };
		}
		if (!isWindows && (statSync(actual.fullPath).mode & 0o777) !== (Number.parseInt(expected.mode, 8) & 0o777)) {
			return { ok: false, error: `Mode mismatch for ${relPath}` };
		}
	}

	for (const [relPath] of actualFiles) {
		if (relPath !== "manifest.json" && !expectedEntries.has(relPath)) {
			return { ok: false, error: `Untracked extraneous file in installation: ${relPath}` };
		}
	}

	const binaryEntry = actualFiles.get(binaryName);
	if (!binaryEntry) {
		return { ok: false, error: `Binary ${binaryName} not found in installation` };
	}

	const hashes = {};
	for (const [k, v] of actualFiles) hashes[k] = v.sha256;

	return {
		ok: true,
		manifest,
		fileCount: actualFiles.size,
		binarySha256: binaryEntry.sha256,
		allHashes: hashes,
	};
}

function checkTerminalAvailability() {
	if (isWindows) {
		return { available: Boolean(process.env.MHC_QA_NODE_PTY_MODULE), reason: "Node node-pty ConPTY adapter (required)" };
	}
	return { available: true, reason: "PTY available" };
}

function spawnTerminal(binary, cmdArgs, options, capture) {
	const onData = text => {
		capture.raw += text;
		for (const listener of capture.listeners) listener();
	};
	if (!isWindows) {
		return Bun.spawn([binary, ...cmdArgs], {
			...options,
			terminal: { cols: 100, rows: 32, data(_terminal, chunk) { onData(Buffer.from(chunk).toString("utf8")); } },
		});
	}
	const bridge = Bun.spawn(["node", fileURLToPath(new URL("./qa-native-update-pty.mjs", import.meta.url)),
		JSON.stringify({ binary, args: cmdArgs, cwd: options.cwd, env: options.env })], {
		env: process.env, stdin: "pipe", stdout: "pipe", stderr: "inherit",
	});
	let exitCode = null;
	let forcedCleanup = Promise.resolve();
	let cleanupError = null;
	const exited = (async () => {
		let pending = "";
		for await (const chunk of bridge.stdout) {
			pending += Buffer.from(chunk).toString("utf8");
			let newline;
			while ((newline = pending.indexOf("\n")) !== -1) {
				const record = JSON.parse(pending.slice(0, newline));
				pending = pending.slice(newline + 1);
				if (typeof record.data === "string") onData(record.data);
				if (typeof record.exitCode === "number") exitCode = record.exitCode;
			}
		}
		const code = await bridge.exited;
		await forcedCleanup;
		if (cleanupError) throw cleanupError;
		exitCode ??= code;
		return exitCode;
	})();
	const send = command => { bridge.stdin.write(`${JSON.stringify(command)}\n`); bridge.stdin.flush(); };
	return {
		exited, get exitCode() { return exitCode; },
		terminal: { write(data) { send({ data }); } },
		kill(signal) {
			if (signal === "SIGKILL") {
				// Killing only Node can orphan the ConPTY child and retain its install lease.
				const killer = Bun.spawn(["taskkill.exe", "/PID", String(bridge.pid), "/T", "/F"], {
					stdout: "ignore", stderr: "inherit",
				});
				forcedCleanup = boundedExit(killer).then(code => {
					if (code !== 0 && bridge.exitCode === null) throw new Error(`ConPTY tree cleanup failed: ${code}`);
				}).catch(error => { cleanupError = error; });
				return forcedCleanup.then(() => { if (cleanupError) throw cleanupError; });
			}
			send({ kill: true });
		},
	};
}

const qaRoot = mkdtempSync(join(tmpdir(), "mhc-qa-native-run-"));
const installDir = join(qaRoot, "install");
const agentDir = join(qaRoot, "agent");
const binDir = join(qaRoot, "bin");
mkdirSync(installDir, { recursive: true });
mkdirSync(agentDir, { recursive: true });
mkdirSync(binDir, { recursive: true });

const sentinelContent = `mhc-qa-sentinel-${Date.now()}-${Math.random().toString(36).slice(2)}`;
const sentinelFile = join(agentDir, "user_state_sentinel.txt");
writeFileSync(sentinelFile, sentinelContent);
const initialSentinelSha = computeSha256(sentinelFile);

const settingsFile = join(agentDir, "settings.json");
const initialSettingsObj = { theme: "dark", editor: "vim", customUserSetting: true };
writeFileSync(settingsFile, JSON.stringify(initialSettingsObj, null, 2));
const initialSettingsSha = computeSha256(settingsFile);

const fixtureServer = createUpdateFixtureServer({
	packagePath: newPackagePath,
	version: newVersion,
	target: targetTuple,
});

const commonEnv = {
	PATH: process.env.PATH,
	SystemRoot: process.env.SystemRoot,
	WINDIR: process.env.WINDIR,
	COMSPEC: process.env.COMSPEC,
	HOME: qaRoot,
	USERPROFILE: qaRoot,
	TMPDIR: qaRoot,
	TMP: qaRoot,
	TEMP: qaRoot,
	MHC_UPDATE_RELEASE_URL: fixtureServer.releaseUrl,
	MAHO_UPDATE_URL: fixtureServer.url,
	MHC_UPDATE_BASE_URL: fixtureServer.url,
	MAHO_CODING_AGENT_DIR: agentDir,
	CODING_AGENT_DIR: agentDir,
	MAHO_OFFLINE: "0",
};

const EXPECTED_SCENARIOS = upgradeOnly ? ["success_update_A_to_B"] : [
	"success_update_A_to_B",
	"latest_noop_clean_exit",
	"bad_checksum_refusal",
	"invalid_archive_refusal",
	"rollback_after_installed_validation_failure",
	"active_session_refusal",
	"sentinel_preservation_and_cleanup",
	"startup_notice_pty_and_cache",
];

function getSiblingInventory(dir) {
	if (!existsSync(dir)) return {};
	const inventory = {};
	const entries = readdirSync(dir);
	for (const entry of entries) {
		const fullPath = join(dir, entry);
		try {
			const stat = statSync(fullPath);
			if (stat.isFile()) {
				inventory[entry] = {
					type: "file",
					size: stat.size,
					sha256: computeSha256(fullPath),
				};
			} else if (stat.isDirectory()) {
				inventory[entry] = {
					type: "dir",
					fileCount: readdirSync(fullPath).length,
				};
			}
		} catch {}
	}
	return inventory;
}

const installedMhc = join(installDir, binaryName);

const referenceRoot = mkdtempSync(join(tmpdir(), "mhc-qa-ref-"));
const baselineDirA = join(referenceRoot, "baseline-A");
const referenceDirB = join(referenceRoot, "reference-B");

await extractArchive(oldPackagePath, baselineDirA);
await extractArchive(newPackagePath, referenceDirB);

const initialPackageCheck = verifyPackageIntegrity(baselineDirA);
if (!initialPackageCheck.ok) {
	throw new Error(`Package A baseline integrity failed: ${initialPackageCheck.error}`);
}
const referenceBCheck = verifyPackageIntegrity(referenceDirB);
if (!referenceBCheck.ok) {
	throw new Error(`Package B reference integrity failed: ${referenceBCheck.error}`);
}

const expectedBSha = referenceBCheck.binarySha256;
const expectedASha = initialPackageCheck.binarySha256;

function matchesReference(check, reference) {
	return check.ok && Object.keys(check.allHashes).length === Object.keys(reference.allHashes).length &&
		Object.entries(reference.allHashes).every(([path, hash]) => check.allHashes[path] === hash);
}

function downloadedFixture() {
	const paths = fixtureServer.getRequests().map(request => request.pathname);
	return paths.some(path => path.startsWith("/assets/") && path.endsWith(".sha256")) &&
		paths.some(path => path.startsWith("/assets/") && !path.endsWith(".sha256"));
}

async function resetInstallDirToA() {
	rmSync(installDir, { recursive: true, force: true });
	mkdirSync(installDir, { recursive: true });
	cpSync(baselineDirA, installDir, { recursive: true });
	if (!isWindows) {
		chmodSync(installedMhc, 0o755);
	}
	const checkA = await executeCommand(installedMhc, ["--version"], { env: commonEnv });
	const verA = checkA.stdout.trim();
	if (checkA.exitCode !== 0 || checkA.killed || verA !== oldVersion) {
		throw new Error(`Failed to reset installation to version A; expected '${oldVersion}', got: '${verA}'`);
	}
}

function verifyStateUnmodified() {
	const currentSentinelExists = existsSync(sentinelFile);
	const currentSettingsExists = existsSync(settingsFile);
	const currentSentinelSha = computeSha256(sentinelFile);
	const currentSettingsSha = computeSha256(settingsFile);

	const sentinelOk = currentSentinelExists && currentSentinelSha === initialSentinelSha;
	const settingsOk = currentSettingsExists && currentSettingsSha === initialSettingsSha;

	return {
		sentinelOk,
		settingsOk,
		initialSentinelSha,
		currentSentinelSha,
		initialSettingsSha,
		currentSettingsSha,
	};
}

const scenarioReceipts = [];

function recordScenario(name, pass, details = {}) {
	const record = {
		scenario: name,
		pass: Boolean(pass),
		timestamp: new Date().toISOString(),
		sentinelSha: computeSha256(sentinelFile),
		settingsSha: computeSha256(settingsFile),
		...details,
	};
	scenarioReceipts.push(record);
	const statusLabel = details.skipped ? "SKIPPED" : pass ? "PASS" : "FAIL";
	console.log(`[QA SCENARIO] ${name}: ${statusLabel}`);
	writeFileSync(join(evidenceDir, `${name}.receipt.json`), JSON.stringify(record, null, 2));
}

const terminalStatus = checkTerminalAvailability();

try {
	console.log(`Starting real update QA on target ${targetTuple}`);
	console.log(`Fixture loopback server listening on ${fixtureServer.url}`);
	console.log(`Terminal availability: ${terminalStatus.available ? "AVAILABLE" : "UNAVAILABLE"} (${terminalStatus.reason})`);

	await resetInstallDirToA();

	const initialVersionCheck = await executeCommand(installedMhc, ["--version"], { env: commonEnv });
	const initialVersion = initialVersionCheck.stdout.trim();
	const initialSha = computeSha256(installedMhc);
	writeFileSync(join(evidenceDir, "initial_version.txt"), initialVersion);
	writeFileSync(join(evidenceDir, "initial_binary.sha256"), `${initialSha}  ${binaryName}\n`);

	{
		const scenarioName = "success_update_A_to_B";
		fixtureServer.setMode("normal");
		fixtureServer.setPackage(newPackagePath, newVersion);
		fixtureServer.clearRequests();
		const inventoryBefore = getSiblingInventory(installDir);

		const cleanup = observeUpdateCleanup(dirname(installDir));
		let updateResult;
		let cleanupFinished;
		try {
			updateResult = await executeCommand(installedMhc, ["update"], { env: commonEnv });
			cleanupFinished = await cleanup.afterUpdate();
		} finally {
			cleanup.close();
		}
		writeFileSync(join(evidenceDir, `${scenarioName}.stdout.txt`), updateResult.stdout);
		writeFileSync(join(evidenceDir, `${scenarioName}.stderr.txt`), updateResult.stderr);

		const postUpdateCheck = await executeCommand(installedMhc, ["--version"], { env: commonEnv });
		const postVersion = postUpdateCheck.stdout.trim();
		const postSha = computeSha256(installedMhc);
		const inventoryAfter = getSiblingInventory(installDir);

		const exactBVersion = postUpdateCheck.exitCode === 0 && postVersion === newVersion;
		const binaryShaMatchesB = postSha === expectedBSha && postSha !== initialSha;
		const exitOk = updateResult.exitCode === 0 && !updateResult.killed;
		const updateOutputConfirmed = updateResult.stdout.includes(`Updated mhc to ${newVersion}.`);
		const packageCheck = verifyPackageIntegrity(installDir);
		const stateCheck = verifyStateUnmodified();
		const pass = exitOk && updateOutputConfirmed && exactBVersion && binaryShaMatchesB && matchesReference(packageCheck, referenceBCheck) && downloadedFixture() && cleanupFinished && stateCheck.sentinelOk && stateCheck.settingsOk;

		recordScenario(scenarioName, pass, {
			exitCode: updateResult.exitCode,
			initialVersion,
			postVersion,
			initialSha,
			postSha,
			expectedBSha,
			packageCheck,
			networkRequests: fixtureServer.getRequestCount(),
			inventoryBefore,
			inventoryAfter,
			stateCheck,
		});
	}

	if (!upgradeOnly) {
		const scenarioName = "latest_noop_clean_exit";
		fixtureServer.setMode("normal");
		fixtureServer.setPackage(newPackagePath, newVersion);
		fixtureServer.clearRequests();
		const preSha = computeSha256(installedMhc);

		const updateResult = await executeCommand(installedMhc, ["update"], { env: commonEnv });
		writeFileSync(join(evidenceDir, `${scenarioName}.stdout.txt`), updateResult.stdout);
		writeFileSync(join(evidenceDir, `${scenarioName}.stderr.txt`), updateResult.stderr);

		const postSha = computeSha256(installedMhc);
		const outputText = `${updateResult.stdout} ${updateResult.stderr}`.toLowerCase();
		const indicatesUpToDate = outputText.includes("up to date") || outputText.includes("already");
		const exitOk = updateResult.exitCode === 0 && !updateResult.killed;
		const packageCheck = verifyPackageIntegrity(installDir);
		const binaryUnchanged = packageCheck.ok && postSha === preSha && postSha === expectedBSha;
		const stateCheck = verifyStateUnmodified();
		const pass = exitOk && indicatesUpToDate && binaryUnchanged && matchesReference(packageCheck, referenceBCheck) && fixtureServer.getRequestCount() === 1 && stateCheck.sentinelOk && stateCheck.settingsOk;

		recordScenario(scenarioName, pass, {
			exitCode: updateResult.exitCode,
			preSha,
			postSha,
			indicatesUpToDate,
			stdoutExcerpt: updateResult.stdout.trim().slice(0, 300),
			packageCheck,
			stateCheck,
		});
	}

	if (!upgradeOnly) {
		const scenarioName = "bad_checksum_refusal";
		await resetInstallDirToA();
		fixtureServer.setMode("bad_checksum");
		fixtureServer.setPackage(newPackagePath, newVersion);
		fixtureServer.clearRequests();
		const preSha = computeSha256(installedMhc);

		const updateResult = await executeCommand(installedMhc, ["update"], { env: commonEnv });
		writeFileSync(join(evidenceDir, `${scenarioName}.stdout.txt`), updateResult.stdout);
		writeFileSync(join(evidenceDir, `${scenarioName}.stderr.txt`), updateResult.stderr);

		const postSha = computeSha256(installedMhc);
		const outputText = `${updateResult.stdout} ${updateResult.stderr}`.toLowerCase();
		const refused = updateResult.exitCode !== null && updateResult.exitCode !== 0 && !updateResult.killed;
		const mentionsIntegrity = outputText.includes("checksum") || outputText.includes("hash") || outputText.includes("integrity") || outputText.includes("mismatch");
		const packageCheck = verifyPackageIntegrity(installDir);
		const binaryRestored = packageCheck.ok && postSha === preSha && postSha === initialSha;
		const stateCheck = verifyStateUnmodified();
		const pass = refused && mentionsIntegrity && binaryRestored && matchesReference(packageCheck, initialPackageCheck) && downloadedFixture() && stateCheck.sentinelOk && stateCheck.settingsOk;

		recordScenario(scenarioName, pass, {
			exitCode: updateResult.exitCode,
			preSha,
			postSha,
			refused,
			mentionsIntegrity,
			packageCheck,
			stateCheck,
		});
	}

	if (!upgradeOnly) {
		const scenarioName = "invalid_archive_refusal";
		await resetInstallDirToA();
		fixtureServer.setMode("invalid_archive");
		fixtureServer.setPackage(newPackagePath, newVersion);
		fixtureServer.clearRequests();
		const preSha = computeSha256(installedMhc);

		const updateResult = await executeCommand(installedMhc, ["update"], { env: commonEnv });
		writeFileSync(join(evidenceDir, `${scenarioName}.stdout.txt`), updateResult.stdout);
		writeFileSync(join(evidenceDir, `${scenarioName}.stderr.txt`), updateResult.stderr);

		const postSha = computeSha256(installedMhc);
		const outputText = `${updateResult.stdout} ${updateResult.stderr}`.toLowerCase();
		const refused = updateResult.exitCode !== null && updateResult.exitCode !== 0 && !updateResult.killed;
		const mentionsArchiveError = outputText.length > 0 && (
			outputText.includes("archive") || outputText.includes("extract") || outputText.includes("tar") ||
			outputText.includes("zip") ||
			outputText.includes("invalid") || outputText.includes("corrupt")
		);
		const packageCheck = verifyPackageIntegrity(installDir);
		const binaryRestored = packageCheck.ok && postSha === preSha && postSha === initialSha;
		const stateCheck = verifyStateUnmodified();
		const pass = refused && mentionsArchiveError && binaryRestored && matchesReference(packageCheck, initialPackageCheck) && downloadedFixture() && stateCheck.sentinelOk && stateCheck.settingsOk;

		recordScenario(scenarioName, pass, {
			exitCode: updateResult.exitCode,
			preSha,
			postSha,
			refused,
			mentionsArchiveError,
			packageCheck,
			stateCheck,
		});
	}

	if (!upgradeOnly) {
		const scenarioName = "rollback_after_installed_validation_failure";
		await resetInstallDirToA();
		fixtureServer.setMode("normal");
		fixtureServer.setPackage(newPackagePath, newVersion);
		fixtureServer.clearRequests();

		const preSha = computeSha256(installedMhc);
		const initialCheck = verifyPackageIntegrity(installDir);
		if (!initialCheck.ok) {
			throw new Error(`Package integrity failed before rollback test: ${initialCheck.error}`);
		}

		// A manifest-valid native candidate stages successfully but fails after install.
		const faultDir = join(referenceRoot, "rollback-candidate");
		cpSync(referenceDirB, faultDir, { recursive: true });
		cpSync(rollbackProbe, join(faultDir, binaryName));
		if (!isWindows) chmodSync(join(faultDir, binaryName), 0o755);
		const faultManifestPath = join(faultDir, "manifest.json");
		const faultManifest = JSON.parse(readFileSync(faultManifestPath, "utf8"));
		const probeEntry = faultManifest.executables.find(entry => entry.path === binaryName);
		if (!probeEntry) throw new Error("Candidate manifest has no mhc entry");
		probeEntry.size = statSync(join(faultDir, binaryName)).size;
		probeEntry.sha256 = computeSha256(join(faultDir, binaryName));
		probeEntry.mode = "755";
		writeFileSync(faultManifestPath, JSON.stringify(faultManifest));
		const faultCheck = verifyPackageIntegrity(faultDir);
		if (!faultCheck.ok) throw new Error(faultCheck.error);
		const faultArchive = join(referenceRoot, isWindows ? "rollback.zip" : "rollback.tar.gz");
		const pack = isWindows
			? Bun.spawn(["7z", "a", "-tzip", faultArchive, basename(faultDir)], { cwd: referenceRoot })
			: Bun.spawn(["tar", "-czf", faultArchive, "-C", referenceRoot, basename(faultDir)]);
		if (await boundedExit(pack) !== 0) throw new Error("Cannot package rollback probe");
		fixtureServer.setPackage(faultArchive, newVersion);
		const marker = join(qaRoot, "rollback-installed.marker");
		const markerObserved = waitForFileAppearance(qaRoot, basename(marker), timeoutMs);
		const cleanup = observeUpdateCleanup(dirname(installDir));
		let updateResult;
		let cleanupFinished;
		try {
			updateResult = await executeCommand(installedMhc, ["update"], { env: {
				...commonEnv, MHC_QA_ROLLBACK_MARKER: marker, MHC_QA_PROBE_VERSION: newVersion,
			} });
			cleanupFinished = await cleanup.afterUpdate();
		} finally { cleanup.close(); }
		writeFileSync(join(evidenceDir, scenarioName + ".stdout.txt"), updateResult.stdout);
		writeFileSync(join(evidenceDir, scenarioName + ".stderr.txt"), updateResult.stderr);
		const installedProbeRan = await markerObserved && resolve(readFileSync(marker, "utf8").replace(/^\\\\\?\\/, "")) === resolve(installedMhc);
		const output = updateResult.stdout + updateResult.stderr;
		const rollbackObserved = output.includes("candidate validation failed; previous installation restored");
		const postCheck = await executeCommand(installedMhc, ["--version"], { env: commonEnv });
		const packageCheck = verifyPackageIntegrity(installDir);
		const stateCheck = verifyStateUnmodified();
		const pass = updateResult.exitCode !== null && updateResult.exitCode !== 0 && !updateResult.killed &&
			installedProbeRan && rollbackObserved && downloadedFixture() && cleanupFinished &&
			postCheck.exitCode === 0 && !postCheck.killed && postCheck.stdout.trim() === oldVersion &&
			computeSha256(installedMhc) === preSha && matchesReference(packageCheck, initialPackageCheck) &&
			stateCheck.sentinelOk && stateCheck.settingsOk;
		recordScenario(scenarioName, pass, {
			updateExitCode: updateResult.exitCode, installedProbeRan, rollbackObserved, cleanupFinished,
			packageCheck, stateCheck, fault: "native candidate fails installed validation after backup and replacement",
		});
		fixtureServer.setPackage(newPackagePath, newVersion);
	}

	if (!upgradeOnly) {
		const scenarioName = "active_session_refusal";
		await resetInstallDirToA();
		fixtureServer.setMode("normal");
		fixtureServer.setPackage(newPackagePath, newVersion);

		let activeProc = null;
		let promptObserved = false;
		let leaseObserved = false;
		const activeCapture = { raw: "", listeners: new Set() };
		const projectDir = join(qaRoot, "test-active-project");
		mkdirSync(projectDir, { recursive: true });

		const modelServer = Bun.serve({
			port: 0,
			fetch() {
				return new Response("data: [DONE]\n\n", { headers: { "content-type": "text/event-stream" } });
			},
		});

		const activeModelsPath = join(agentDir, "models.json");
		writeFileSync(activeModelsPath, JSON.stringify({
			providers: {
				local_mock: {
					api: "openai-completions",
					baseUrl: `http://127.0.0.1:${modelServer.port}/v1`,
					apiKey: "mock-key",
					models: [{ id: "mock-model", reasoning: false, input: ["text"], contextWindow: 4096, maxTokens: 1024 }],
				},
			},
		}, null, 2));

		const leaseLockFileName = `.${basename(installDir)}.update.lock`;
		const leaseLockFilePath = join(qaRoot, leaseLockFileName);
		if (existsSync(leaseLockFilePath)) {
			rmSync(leaseLockFilePath, { force: true });
		}
		const leaseWaitPromise = waitForFileAppearance(qaRoot, leaseLockFileName, 15000);

		try {
			const promptWaitPromise = eventWaiter(
				activeCapture,
				(raw) => /›|❯|\>\s/.test(raw),
				"active process interactive startup",
				15000,
			);
			const spawnOptions = {
				cwd: projectDir,
				env: {
					...commonEnv,
					TERM: "xterm-256color",
				},
				stdin: "pipe",
				stdout: "pipe",
				stderr: "pipe",
			};

			const activeArgs = ["--provider", "local_mock", "--model", "mock-model", "--no-approve"];
			activeProc = terminalStatus.available
				? spawnTerminal(installedMhc, activeArgs, spawnOptions, activeCapture)
				: Bun.spawn([installedMhc, ...activeArgs], spawnOptions);

			if (!terminalStatus.available && activeProc.stdout) {
				(async () => {
					try {
						const reader = activeProc.stdout.getReader();
						while (true) {
							const { done, value } = await reader.read();
							if (done) break;
							activeCapture.raw += Buffer.from(value).toString("utf8");
							for (const l of activeCapture.listeners) l();
						}
					} catch {}
				})();
			}

			[promptObserved, leaseObserved] = await Promise.all([promptWaitPromise, leaseWaitPromise]);
		} catch (spawnErr) {
			console.warn(`[QA WARNING] Failed to spawn active process: ${spawnErr}`);
		}

		const ready = leaseObserved && promptObserved;

		const preSha = computeSha256(installedMhc);
		const updateResult = await executeCommand(installedMhc, ["update"], { env: commonEnv });
		writeFileSync(join(evidenceDir, `${scenarioName}.stdout.txt`), updateResult.stdout);
		writeFileSync(join(evidenceDir, `${scenarioName}.stderr.txt`), updateResult.stderr);

		const processWasStillAlive = activeProc !== null && activeProc.exitCode === null;
		if (activeProc) {
			try { if (activeProc.terminal) activeProc.terminal.write("\u0004"); } catch {}
			try { activeProc.stdin.end(); } catch {}
			try { activeProc.kill("SIGTERM"); } catch {}
			await boundedExit(activeProc);
		}
		modelServer.stop(true);

		const postSha = computeSha256(installedMhc);
		const outputText = `${updateResult.stdout} ${updateResult.stderr}`.toLowerCase();
		const refusedActive = !updateResult.killed && updateResult.exitCode !== null && updateResult.exitCode !== 0 && (
			outputText.includes("in use") || outputText.includes("active") ||
			outputText.includes("session") || outputText.includes("running") ||
			outputText.includes("lease")
		);
		const packageCheck = verifyPackageIntegrity(installDir);
		const stateCheck = verifyStateUnmodified();
		const binaryUntouched = postSha !== null && postSha === preSha && postSha === initialSha;
		const pass = ready && refusedActive && processWasStillAlive && binaryUntouched && matchesReference(packageCheck, initialPackageCheck) && stateCheck.sentinelOk && stateCheck.settingsOk;

		recordScenario(scenarioName, pass, {
			exitCode: updateResult.exitCode,
			preSha,
			postSha,
			ready,
			promptObserved,
			leaseObserved,
			refusedActive,
			processWasStillAlive,
			binaryUntouched,
			packageCheck,
			stateCheck,
		});
	}

	if (!upgradeOnly) {
		const scenarioName = "sentinel_preservation_and_cleanup";
		const stateCheck = verifyStateUnmodified();

		const parentDir = dirname(installDir);
		const remainingStaging = readdirSync(parentDir).filter(name =>
			name.startsWith(".mhc-update-") || name.startsWith(".mhc-backup-") ||
			name.includes("staging") || name.includes(".backup.")
		);
		const cleanupOk = remainingStaging.length === 0;

		const precedingScenariosPassed = scenarioReceipts.length === 6 && scenarioReceipts.every(receipt => receipt.pass);
		const pass = precedingScenariosPassed && stateCheck.sentinelOk && stateCheck.settingsOk && cleanupOk;
		recordScenario(scenarioName, pass, {
			sentinelOk: stateCheck.sentinelOk,
			settingsOk: stateCheck.settingsOk,
			cleanupOk,
			remainingStaging,
		});
	}

	if (!upgradeOnly) {
		const scenarioName = "startup_notice_pty_and_cache";
		await resetInstallDirToA();
		let ptyPass = false;
		let ptyDetails = {};

		try {
			fixtureServer.setMode("normal");
			fixtureServer.setPackage(newPackagePath, newVersion);
			fixtureServer.clearRequests();

			const cacheDir = join(agentDir, "cache");
			const cacheFile = join(cacheDir, "mhc-latest-release.json");
			mkdirSync(cacheDir, { recursive: true });
			if (existsSync(cacheFile)) {
				rmSync(cacheFile, { force: true });
			}

			// Watch for cache file appearance before spawning online process (bounded 8s timeout)
			const cacheAppearancePromise = waitForFileAppearance(cacheDir, "mhc-latest-release.json", 8000);

			let onlineProc = null;
			try {
				const spawnOptions = {
					cwd: qaRoot,
					env: { ...commonEnv, MAHO_OFFLINE: "0" },
					stdin: "pipe",
					stdout: "ignore",
					stderr: "ignore",
				};
				spawnOptions.env.TERM = "xterm-256color";
				onlineProc = terminalStatus.available
					? spawnTerminal(installedMhc, ["--no-tools", "--no-skills"], spawnOptions, { raw: "", listeners: new Set() })
					: Bun.spawn([installedMhc, "--no-tools", "--no-skills"], spawnOptions);

				const cacheAppeared = await cacheAppearancePromise;
				if (!cacheAppeared) {
					throw new Error("Cache file mhc-latest-release.json did not appear within 8s timeout via fs.watch");
				}
			} finally {
				if (onlineProc) {
					try { onlineProc.stdin.end(); } catch {}
					try { onlineProc.kill("SIGTERM"); } catch {}
					await boundedExit(onlineProc);
				}
			}

			const onlineRequests = fixtureServer.getRequestCount();
			const cacheWritten = existsSync(cacheFile);
			let cachedVersion = null;
			let cacheValid = false;
			if (cacheWritten) {
				try {
					const parsedCache = JSON.parse(readFileSync(cacheFile, "utf8"));
					cachedVersion = parsedCache?.release?.version ?? null;
					cacheValid = parsedCache.schema_version === 1 && parsedCache.success === true &&
						parsedCache.endpoint === fixtureServer.releaseUrl && parsedCache.target === targetTuple;
				} catch {}
			}

			fixtureServer.clearRequests();
			let noticeSeen = false;

			if (terminalStatus.available) {
				const capture = { raw: "", listeners: new Set() };
				const noticePromise = eventWaiter(
					capture,
					(raw) => raw.includes("mhc update") && raw.includes(newVersion),
					"startup notice in offline mode",
					15000,
				);
				const ptyProc = spawnTerminal(installedMhc, ["--offline", "--no-tools", "--no-skills"], {
					cwd: qaRoot,
					env: {
						...commonEnv,
						MAHO_OFFLINE: "1",
						TERM: "xterm-256color",
						COLORTERM: "truecolor",
					},
				}, capture);

				noticeSeen = await noticePromise;

				try { ptyProc.terminal.write("\u0004"); } catch {}
				try { ptyProc.kill("SIGTERM"); } catch {}
				await boundedExit(ptyProc);

				writeFileSync(join(evidenceDir, `${scenarioName}.ansi`), capture.raw);
				writeFileSync(join(evidenceDir, `${scenarioName}.txt`), capture.raw.replace(/\x1b\[[0-9;]*[a-zA-Z]/g, ""));
			} else {
				writeFileSync(join(evidenceDir, `${scenarioName}.txt`), `PTY notice render skipped: ${terminalStatus.reason}\n`);
			}

			const offlineRequests = fixtureServer.getRequestCount();
			const stateCheck = verifyStateUnmodified();
			ptyPass = onlineRequests === 1 && cacheWritten && cacheValid && cachedVersion === newVersion && offlineRequests === 0 && noticeSeen === true &&
				stateCheck.sentinelOk && stateCheck.settingsOk && matchesReference(verifyPackageIntegrity(installDir), initialPackageCheck);
			ptyDetails = {
				skipped: !terminalStatus.available,
				reason: terminalStatus.available ? null : terminalStatus.reason,
				onlineRequests,
				offlineRequests,
				cacheWritten,
				cacheValid,
				stateCheck,
				cachedVersion,
				noticeSeen,
				terminalAvailable: terminalStatus.available,
				terminalReason: terminalStatus.reason,
			};
		} catch (err) {
			ptyPass = false;
			ptyDetails = { error: String(err), skipped: !terminalStatus.available, reason: terminalStatus.reason };
		}

		recordScenario(scenarioName, ptyPass, ptyDetails);
	}

} catch (fatalError) {
	console.error("Fatal failure in QA run:", fatalError);
	recordScenario("fatal_run_error", false, { error: String(fatalError) });
} finally {
	fixtureServer.stop();
	rmSync(qaRoot, { recursive: true, force: true });
	rmSync(referenceRoot, { recursive: true, force: true });

	const completedNames = new Set(scenarioReceipts.map(r => r.scenario));
	const allExpectedPresent = EXPECTED_SCENARIOS.length === scenarioReceipts.length &&
		EXPECTED_SCENARIOS.every(name => completedNames.has(name));
	const skippedList = scenarioReceipts.filter(r => r.skipped);
	// Every scenario is required on every supported release platform.
	const allPassed = allExpectedPresent && scenarioReceipts.every(r => r.pass === true && !r.skipped);

	const summary = {
		suite: "mhc native update QA",
		target: targetTuple,
		oldVersion,
		newVersion,
		upgradeOnly,
		completedAt: new Date().toISOString(),
		expectedScenariosCount: EXPECTED_SCENARIOS.length,
		totalScenarios: scenarioReceipts.length,
		passed: scenarioReceipts.filter(r => r.pass).length,
		failed: scenarioReceipts.filter(r => !r.pass && !r.skipped).length,
		skipped: skippedList.length,
		skippedCoverage: skippedList.map(r => ({ scenario: r.scenario, reason: r.reason })),
		terminalStatus,
		allExpectedPresent,
		allPassed,
		scenarios: scenarioReceipts,
	};

	writeFileSync(join(evidenceDir, "summary.json"), JSON.stringify(summary, null, 2));
	console.log(`\nQA Run Complete. Summary written to ${join(evidenceDir, "summary.json")}`);
	console.log(`Result: ${allPassed ? "ALL REQUIRED SCENARIOS PASSED" : "FAILURES OR REQUIRED SKIPS DETECTED"}`);
	if (skippedList.length > 0) {
		console.log(`Skipped required QA coverage: ${skippedList.map(r => r.scenario).join(", ")}`);
	}
	if (!allPassed) {
		process.exit(1);
	}
	process.exit(0);
}
