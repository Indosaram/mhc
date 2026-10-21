#!/usr/bin/env bun
import { createHash } from "node:crypto";
import { chmodSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, readlinkSync, realpathSync, renameSync, rmSync, statSync, watch, writeFileSync } from "node:fs";
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
let intermediatePackagePath = null;
let intermediateVersion = null;

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
	} else if (args[i] === "--intermediate-package" && i + 1 < args.length) {
		intermediatePackagePath = resolve(args[++i]);
	} else if (args[i] === "--intermediate-version" && i + 1 < args.length) {
		intermediateVersion = args[++i];
	} else if (args[i] === "--rollback-probe" && i + 1 < args.length) {
		rollbackProbe = resolve(args[++i]);
	} else {
		throw new Error(`Unknown or incomplete argument: ${args[i]}`);
	}
}

if (!oldPackagePath || !newPackagePath || !evidenceDir || !oldVersion || !newVersion) {
	console.error("Usage: bun scripts/qa-native-update.mjs --old-package <A> --new-package <B> --old-version <A> --new-version <B> --evidence-dir <dir> [--rollback-probe <exe>] [--upgrade-only [--intermediate-package <P> --intermediate-version <v>]]");
	console.error("--upgrade-only: --old-package is a published flat-layout release; only its migration through install.sh/install.ps1 into the new --new-package is exercised (its own `mhc update` is never run).");
	process.exit(2);
}
if (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0 || targetTuple !== detectTargetTuple()) {
	throw new Error("Expected a positive bounded timeout and the host's native target tuple");
}
if (oldVersion === newVersion) throw new Error("Candidate versions A and B must differ");
if (intermediateVersion && (intermediateVersion === oldVersion || intermediateVersion === newVersion)) throw new Error("The intermediate version must differ from the old and new versions");
if (Boolean(intermediatePackagePath) !== Boolean(intermediateVersion)) throw new Error("--intermediate-package and --intermediate-version must be given together");
if (intermediatePackagePath && !upgradeOnly) throw new Error("--intermediate-package is only meaningful with --upgrade-only");
if (intermediatePackagePath && !existsSync(intermediatePackagePath)) throw new Error(`Intermediate package archive does not exist: ${intermediatePackagePath}`);
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
const launcherName = `mhc-launcher${exeSuffix}`;
const scriptsDir = dirname(fileURLToPath(import.meta.url));
const installShPath = resolve(scriptsDir, "..", "install.sh");
const installPs1Path = resolve(scriptsDir, "..", "install.ps1");

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
					if (readdirSync(directoryPath).some(name => name.startsWith(".staging-"))) return;
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

function verifyPackageIntegrity(dir, { launcher = true } = {}) {
	const manifestPath = join(dir, "manifest.json");
	if (!existsSync(manifestPath)) {
		return { ok: false, error: "manifest.json missing in package directory" };
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
			return { ok: false, error: `Untracked extraneous file in package: ${relPath}` };
		}
	}

	const binaryEntry = actualFiles.get(binaryName);
	if (!binaryEntry) {
		return { ok: false, error: `Binary ${binaryName} not found in package` };
	}
	if (launcher && !expectedEntries.has(launcherName)) {
		return { ok: false, error: `Manifest does not track ${launcherName}` };
	}
	if (launcher && !actualFiles.has(launcherName)) {
		return { ok: false, error: `${launcherName} not found in package` };
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
		const proc = Bun.spawn([binary, ...cmdArgs], {
			...options,
			terminal: { cols: 100, rows: 32, data(_terminal, chunk) { onData(Buffer.from(chunk).toString("utf8")); } },
		});
		proc.pidReady = Promise.resolve(proc.pid);
		return proc;
	}
	const bridge = Bun.spawn(["node", fileURLToPath(new URL("./qa-native-update-pty.mjs", import.meta.url)),
		JSON.stringify({ binary, args: cmdArgs, cwd: options.cwd, env: options.env })], {
		env: process.env, stdin: "pipe", stdout: "pipe", stderr: "inherit",
	});
	let exitCode = null;
	let resolvePid;
	const pidReady = new Promise(resolvePromise => { resolvePid = resolvePromise; });
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
				if (typeof record.pid === "number") resolvePid(record.pid);
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
		exited, get exitCode() { return exitCode; }, pidReady,
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
const projectDir = join(qaRoot, "test-active-project");
mkdirSync(agentDir, { recursive: true });
mkdirSync(binDir, { recursive: true });
mkdirSync(projectDir, { recursive: true });

const sentinelContent = `mhc-qa-sentinel-${Date.now()}-${Math.random().toString(36).slice(2)}`;
const sentinelFile = join(agentDir, "user_state_sentinel.txt");
writeFileSync(sentinelFile, sentinelContent);
const initialSentinelSha = computeSha256(sentinelFile);

const settingsFile = join(agentDir, "settings.json");
writeFileSync(settingsFile, JSON.stringify({ theme: "dark", editor: "vim", customUserSetting: true }, null, 2));
const initialSettingsSha = computeSha256(settingsFile);

const fixtureServer = createUpdateFixtureServer({
	packagePath: newPackagePath,
	version: newVersion,
	target: targetTuple,
});

const modelServer = Bun.serve({
	port: 0,
	fetch() {
		return new Response("data: [DONE]\n\n", { headers: { "content-type": "text/event-stream" } });
	},
});
writeFileSync(join(agentDir, "models.json"), JSON.stringify({
	providers: {
		local_mock: {
			api: "openai-completions",
			baseUrl: `http://127.0.0.1:${modelServer.port}/v1`,
			apiKey: "mock-key",
			models: [{ id: "mock-model", reasoning: false, input: ["text"], contextWindow: 4096, maxTokens: 1024 }],
		},
	},
}, null, 2));

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

const EXPECTED_SCENARIOS = upgradeOnly ? [
	"published_flat_migration_via_installer",
	"update_after_migration",
] : [
	"installer_bootstrap_and_flat_migration",
	"success_update_A_to_B",
	"latest_noop_clean_exit",
	"bad_checksum_refusal",
	"invalid_archive_refusal",
	"rollback_after_installed_validation_failure",
	"running_sessions_survive_update",
	"retention_defers_old_version_cleanup",
	"sentinel_preservation_and_cleanup",
	"startup_notice_pty_and_cache",
];

const installedMhc = join(installDir, binaryName);
const payloadExecutable = (root, version) => join(root, "versions", version, binaryName);

const referenceRoot = mkdtempSync(join(tmpdir(), "mhc-qa-ref-"));
const baselineDirA = join(referenceRoot, "baseline-A");
const referenceDirB = join(referenceRoot, "reference-B");
const intermediateDir = join(referenceRoot, "intermediate");

await extractArchive(oldPackagePath, baselineDirA);
await extractArchive(newPackagePath, referenceDirB);

const initialPackageCheck = verifyPackageIntegrity(baselineDirA, { launcher: !upgradeOnly });
if (!initialPackageCheck.ok) {
	throw new Error(`Package A baseline integrity failed: ${initialPackageCheck.error}`);
}
const referenceBCheck = verifyPackageIntegrity(referenceDirB);
if (!referenceBCheck.ok) {
	throw new Error(`Package B reference integrity failed: ${referenceBCheck.error}`);
}
let intermediateCheck = null;
if (upgradeOnly) {
	if (!intermediatePackagePath) throw new Error("--upgrade-only requires --intermediate-package/--intermediate-version: the updater-capable package the installer installs over the published flat release");
	await extractArchive(intermediatePackagePath, intermediateDir);
	intermediateCheck = verifyPackageIntegrity(intermediateDir);
	if (!intermediateCheck.ok) throw new Error(`Intermediate package integrity failed: ${intermediateCheck.error}`);
}

const hashOf = (check, name) => check.allHashes[name] ?? null;
const expectedASha = hashOf(initialPackageCheck, binaryName);
const expectedBSha = hashOf(referenceBCheck, binaryName);
const launcherShaA = upgradeOnly ? null : hashOf(initialPackageCheck, launcherName);
const launcherShaB = hashOf(referenceBCheck, launcherName);

function matchesReference(check, reference) {
	return check.ok && Object.keys(check.allHashes).length === Object.keys(reference.allHashes).length &&
		Object.entries(reference.allHashes).every(([path, hash]) => check.allHashes[path] === hash);
}

function downloadedFixture() {
	const paths = fixtureServer.getRequests().map(request => request.pathname);
	return paths.some(path => path.startsWith("/assets/") && path.endsWith(".sha256")) &&
		paths.some(path => path.startsWith("/assets/") && !path.endsWith(".sha256"));
}

function readPointer(root) {
	try {
		return JSON.parse(readFileSync(join(root, "current.json"), "utf8"));
	} catch {
		return null;
	}
}

function pointerIs(root, current, previous) {
	const pointer = readPointer(root);
	return pointer !== null && pointer.schemaVersion === 1 && pointer.current === current && (pointer.previous ?? null) === previous;
}

function installedVersions(root) {
	const directory = join(root, "versions");
	if (!existsSync(directory)) return [];
	return readdirSync(directory).filter(name => !name.endsWith(".lock") && statSync(join(directory, name)).isDirectory()).sort();
}

function leftoverWorkspaces(root) {
	return readdirSync(root).filter(name => name.startsWith(".staging-") || name.startsWith(".install."));
}

function samePath(a, b) {
	const canonical = path => {
		try { return realpathSync(path); } catch { return resolve(path); }
	};
	return canonical(a.replace(/^\\\\\?\\/, "")) === canonical(b);
}

function setExecutableModes(root) {
	if (isWindows) return;
	const manifest = JSON.parse(readFileSync(join(root, "manifest.json"), "utf8"));
	for (const entry of manifest.executables) chmodSync(join(root, entry.path), 0o755);
}

function copyPackage(packageDir, destination) {
	cpSync(packageDir, destination, { recursive: true });
	setExecutableModes(destination);
}

function buildInstall(root, packageDir, version) {
	rmSync(root, { recursive: true, force: true });
	mkdirSync(join(root, "versions"), { recursive: true });
	copyPackage(packageDir, join(root, "versions", version));
	cpSync(join(packageDir, launcherName), join(root, binaryName));
	if (!isWindows) chmodSync(join(root, binaryName), 0o755);
	writeFileSync(join(root, "current.json"), `${JSON.stringify({ schemaVersion: 1, current: version, previous: null })}\n`);
}

async function runVersion(executable, env = commonEnv) {
	const result = await executeCommand(executable, ["--version"], { env });
	return { ok: result.exitCode === 0 && !result.killed, version: result.stdout.trim() };
}

async function prepareInstallA() {
	buildInstall(installDir, baselineDirA, oldVersion);
	const check = await runVersion(installedMhc);
	if (!check.ok || check.version !== oldVersion) {
		throw new Error(`Failed to prepare version A through the root launcher; expected '${oldVersion}', got '${check.version}'`);
	}
	return { launcherSha: computeSha256(installedMhc) };
}

async function runInstaller(packageDir, root, bin) {
	const env = { ...commonEnv, MHC_PACKAGE_DIR: packageDir, MHC_INSTALL_DIR: root, MHC_BIN_DIR: bin, MHC_NO_PATH_UPDATE: "1" };
	if (isWindows) {
		return executeCommand("powershell.exe", ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", installPs1Path], { env, cwd: qaRoot });
	}
	return executeCommand("sh", [installShPath], { env, cwd: qaRoot });
}

function verifyStateUnmodified() {
	const currentSentinelSha = computeSha256(sentinelFile);
	const currentSettingsSha = computeSha256(settingsFile);
	return {
		sentinelOk: currentSentinelSha !== null && currentSentinelSha === initialSentinelSha,
		settingsOk: currentSettingsSha !== null && currentSettingsSha === initialSettingsSha,
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
	console.log(`[QA SCENARIO] ${name}: ${record.pass ? "PASS" : "FAIL"}`);
	writeFileSync(join(evidenceDir, `${name}.receipt.json`), JSON.stringify(record, null, 2));
}

function saveOutput(name, result) {
	writeFileSync(join(evidenceDir, `${name}.stdout.txt`), result.stdout);
	writeFileSync(join(evidenceDir, `${name}.stderr.txt`), result.stderr);
}

async function runUpdate(env = commonEnv) {
	const cleanup = observeUpdateCleanup(installDir);
	try {
		const result = await executeCommand(installedMhc, ["update"], { env });
		const cleanupFinished = await cleanup.afterUpdate();
		return { result, cleanupFinished };
	} finally {
		cleanup.close();
	}
}

const terminalStatus = checkTerminalAvailability();
const sessionReadyMs = Math.max(timeoutMs, 30000);
const promptPattern = /❯/;

function startSession(executable, label) {
	const capture = { raw: "", listeners: new Set() };
	const ready = eventWaiter(capture, raw => promptPattern.test(raw), `${label} interactive startup`, sessionReadyMs);
	const proc = spawnTerminal(executable, ["--provider", "local_mock", "--model", "mock-model", "--no-approve"], {
		cwd: projectDir,
		env: { ...commonEnv, TERM: "xterm-256color" },
	}, capture);
	const state = { exited: false };
	proc.exited.then(() => { state.exited = true; }, () => { state.exited = true; });
	return { label, proc, capture, ready, state };
}

async function stopSession(session) {
	try { session.proc.terminal.write("\u0004"); } catch {}
	try { await session.proc.kill("SIGTERM"); } catch {}
	try { await boundedExit(session.proc); } catch {}
}

async function sessionExecutable(session) {
	let pidTimer;
	const pid = await Promise.race([
		session.proc.pidReady,
		new Promise(resolveWait => { pidTimer = setTimeout(() => resolveWait(null), sessionReadyMs); }),
	]);
	clearTimeout(pidTimer);
	if (pid === null) return null;
	if (process.platform === "linux") {
		try { return readlinkSync(`/proc/${pid}/exe`); } catch { return null; }
	}
	if (process.platform === "darwin") {
		const ps = await executeCommand("ps", ["-p", String(pid), "-o", "comm="], { env: commonEnv, cwd: qaRoot });
		return ps.exitCode === 0 ? ps.stdout.trim() || null : null;
	}
	const script = "$ErrorActionPreference='Stop'; Get-CimInstance Win32_Process -Filter ('ParentProcessId=' + $env:MHC_QA_PARENT_PID) | Where-Object { $_.ExecutablePath } | Select-Object -First 1 -ExpandProperty ExecutablePath";
	const query = await executeCommand("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", script], { env: { ...commonEnv, MHC_QA_PARENT_PID: String(pid) }, cwd: qaRoot });
	return query.exitCode === 0 ? query.stdout.trim() || null : null;
}

async function sessionRunsPayload(session, version, expectedSha) {
	const executable = await sessionExecutable(session);
	const expected = payloadExecutable(installDir, version);
	const onPath = executable !== null && samePath(executable, expected);
	const sha = onPath ? computeSha256(expected) : null;
	return { executable, onPath, sha, shaMatches: sha !== null && sha === expectedSha };
}

try {
	console.log(`Starting real update QA on target ${targetTuple}`);
	console.log(`Fixture loopback server listening on ${fixtureServer.url}`);
	console.log(`Terminal availability: ${terminalStatus.available ? "AVAILABLE" : "UNAVAILABLE"} (${terminalStatus.reason})`);
	if (!upgradeOnly && !terminalStatus.available) {
		throw new Error(`A real PTY/ConPTY is required for the session and startup-notice scenarios and is unavailable: ${terminalStatus.reason}`);
	}

	if (upgradeOnly) {
		{
			const scenarioName = "published_flat_migration_via_installer";
			rmSync(installDir, { recursive: true, force: true });
			copyPackage(baselineDirA, installDir);
			const flatVersion = await runVersion(join(installDir, binaryName));
			const install = await runInstaller(intermediateDir, installDir, binDir);
			saveOutput(scenarioName, install);
			const rootVersion = await runVersion(installedMhc);
			const migratedVersion = await runVersion(payloadExecutable(installDir, oldVersion));
			const migratedCheck = verifyPackageIntegrity(join(installDir, "versions", oldVersion), { launcher: false });
			const intermediateInstalled = verifyPackageIntegrity(join(installDir, "versions", intermediateVersion));
			const launcherSha = computeSha256(installedMhc);
			const pass = flatVersion.ok && flatVersion.version === oldVersion && install.exitCode === 0 && !install.killed &&
				pointerIs(installDir, intermediateVersion, oldVersion) &&
				rootVersion.ok && rootVersion.version === intermediateVersion &&
				migratedVersion.ok && migratedVersion.version === oldVersion &&
				matchesReference(migratedCheck, initialPackageCheck) && matchesReference(intermediateInstalled, intermediateCheck) &&
				launcherSha === hashOf(intermediateCheck, launcherName) && verifyStateUnmodified().sentinelOk;
			recordScenario(scenarioName, pass, {
				flatVersion: flatVersion.version, rootVersion: rootVersion.version, migratedVersion: migratedVersion.version,
				pointer: readPointer(installDir), versions: installedVersions(installDir), launcherSha,
				note: "the published flat release is only migrated by the new installer; its own `mhc update` is never run",
			});
		}
		{
			const scenarioName = "update_after_migration";
			fixtureServer.setMode("normal");
			fixtureServer.setPackage(newPackagePath, newVersion);
			fixtureServer.clearRequests();
			const launcherBefore = computeSha256(installedMhc);
			const { result, cleanupFinished } = await runUpdate();
			saveOutput(scenarioName, result);
			const rootVersion = await runVersion(installedMhc);
			const newCheck = verifyPackageIntegrity(join(installDir, "versions", newVersion));
			const pass = result.exitCode === 0 && !result.killed && result.stdout.includes(`Installed mhc ${newVersion}.`) &&
				pointerIs(installDir, newVersion, intermediateVersion) &&
				rootVersion.ok && rootVersion.version === newVersion &&
				matchesReference(newCheck, referenceBCheck) && computeSha256(installedMhc) === launcherBefore &&
				installedVersions(installDir).includes(oldVersion) && cleanupFinished && downloadedFixture() &&
				verifyStateUnmodified().sentinelOk;
			recordScenario(scenarioName, pass, {
				exitCode: result.exitCode, rootVersion: rootVersion.version, pointer: readPointer(installDir),
				versions: installedVersions(installDir), launcherBefore, launcherAfter: computeSha256(installedMhc), cleanupFinished,
			});
		}
	}

	if (!upgradeOnly) {
		const scenarioName = "installer_bootstrap_and_flat_migration";
		const freshRoot = join(qaRoot, "installer-fresh");
		const freshBin = join(qaRoot, "installer-fresh-bin");
		const fresh = await runInstaller(baselineDirA, freshRoot, freshBin);
		saveOutput(`${scenarioName}.fresh`, fresh);
		const freshLauncher = join(freshRoot, binaryName);
		const freshRootVersion = await runVersion(freshLauncher);
		const freshOk = fresh.exitCode === 0 && !fresh.killed && pointerIs(freshRoot, oldVersion, null) &&
			matchesReference(verifyPackageIntegrity(join(freshRoot, "versions", oldVersion)), initialPackageCheck) &&
			computeSha256(freshLauncher) === launcherShaA && freshRootVersion.ok && freshRootVersion.version === oldVersion &&
			(isWindows || samePath(join(freshBin, "mhc"), freshLauncher));

		const flatRoot = join(qaRoot, "installer-flat");
		const flatBin = join(qaRoot, "installer-flat-bin");
		copyPackage(baselineDirA, flatRoot);
		const flatBefore = await runVersion(join(flatRoot, binaryName));
		const migrate = await runInstaller(referenceDirB, flatRoot, flatBin);
		saveOutput(`${scenarioName}.migrate`, migrate);
		const flatLauncher = join(flatRoot, binaryName);
		const migratedRootVersion = await runVersion(flatLauncher);
		const migratedPrevious = await runVersion(payloadExecutable(flatRoot, oldVersion));
		const allowedRootEntries = new Set([binaryName, "current.json", "versions", ".update.lock"]);
		const strayRootEntries = readdirSync(flatRoot).filter(name => !allowedRootEntries.has(name));
		const migrateOk = flatBefore.ok && flatBefore.version === oldVersion && migrate.exitCode === 0 && !migrate.killed &&
			pointerIs(flatRoot, newVersion, oldVersion) &&
			matchesReference(verifyPackageIntegrity(join(flatRoot, "versions", oldVersion)), initialPackageCheck) &&
			matchesReference(verifyPackageIntegrity(join(flatRoot, "versions", newVersion)), referenceBCheck) &&
			computeSha256(flatLauncher) === launcherShaB && strayRootEntries.length === 0 &&
			migratedRootVersion.ok && migratedRootVersion.version === newVersion &&
			migratedPrevious.ok && migratedPrevious.version === oldVersion;

		const again = await runInstaller(referenceDirB, flatRoot, flatBin);
		saveOutput(`${scenarioName}.reinstall`, again);
		const reinstallOk = again.exitCode === 0 && !again.killed && pointerIs(flatRoot, newVersion, oldVersion) &&
			computeSha256(flatLauncher) === launcherShaB && leftoverWorkspaces(flatRoot).length === 0 && leftoverWorkspaces(freshRoot).length === 0;

		recordScenario(scenarioName, freshOk && migrateOk && reinstallOk && verifyStateUnmodified().sentinelOk, {
			freshOk, migrateOk, reinstallOk, strayRootEntries,
			freshPointer: readPointer(freshRoot), migratedPointer: readPointer(flatRoot),
		});
	}

	if (!upgradeOnly) {
		await prepareInstallA();
		const initialVersionCheck = await runVersion(installedMhc);
		writeFileSync(join(evidenceDir, "initial_version.txt"), initialVersionCheck.version);
		writeFileSync(join(evidenceDir, "initial_launcher.sha256"), `${computeSha256(installedMhc)}  ${binaryName}\n`);
	}

	if (!upgradeOnly) {
		const scenarioName = "success_update_A_to_B";
		await prepareInstallA();
		fixtureServer.setMode("normal");
		fixtureServer.setPackage(newPackagePath, newVersion);
		fixtureServer.clearRequests();
		const launcherBefore = computeSha256(installedMhc);

		const { result, cleanupFinished } = await runUpdate();
		saveOutput(scenarioName, result);

		const post = await runVersion(installedMhc);
		const launcherAfter = computeSha256(installedMhc);
		const bCheck = verifyPackageIntegrity(join(installDir, "versions", newVersion));
		const aCheck = verifyPackageIntegrity(join(installDir, "versions", oldVersion));
		const stateCheck = verifyStateUnmodified();
		const pass = result.exitCode === 0 && !result.killed && result.stdout.includes(`Installed mhc ${newVersion}.`) &&
			post.ok && post.version === newVersion &&
			pointerIs(installDir, newVersion, oldVersion) &&
			launcherAfter === launcherBefore && launcherBefore === launcherShaA &&
			matchesReference(bCheck, referenceBCheck) && hashOf(bCheck, binaryName) === expectedBSha &&
			matchesReference(aCheck, initialPackageCheck) &&
			downloadedFixture() && cleanupFinished && stateCheck.sentinelOk && stateCheck.settingsOk;
		recordScenario(scenarioName, pass, {
			exitCode: result.exitCode, postVersion: post.version, pointer: readPointer(installDir),
			launcherBefore, launcherAfter, versions: installedVersions(installDir), cleanupFinished,
			networkRequests: fixtureServer.getRequestCount(), stateCheck,
		});
	}

	if (!upgradeOnly) {
		const scenarioName = "latest_noop_clean_exit";
		fixtureServer.setMode("normal");
		fixtureServer.setPackage(newPackagePath, newVersion);
		fixtureServer.clearRequests();
		const pointerBefore = readFileSync(join(installDir, "current.json"), "utf8");
		const versionsBefore = installedVersions(installDir);
		const launcherBefore = computeSha256(installedMhc);

		const result = await executeCommand(installedMhc, ["update"], { env: commonEnv });
		saveOutput(scenarioName, result);

		const outputText = `${result.stdout} ${result.stderr}`.toLowerCase();
		const indicatesUpToDate = outputText.includes("up to date") || outputText.includes("already");
		const bCheck = verifyPackageIntegrity(join(installDir, "versions", newVersion));
		const stateCheck = verifyStateUnmodified();
		const pass = result.exitCode === 0 && !result.killed && indicatesUpToDate &&
			readFileSync(join(installDir, "current.json"), "utf8") === pointerBefore &&
			JSON.stringify(installedVersions(installDir)) === JSON.stringify(versionsBefore) &&
			computeSha256(installedMhc) === launcherBefore && matchesReference(bCheck, referenceBCheck) &&
			fixtureServer.getRequestCount() === 1 && leftoverWorkspaces(installDir).length === 0 &&
			stateCheck.sentinelOk && stateCheck.settingsOk;
		recordScenario(scenarioName, pass, {
			exitCode: result.exitCode, indicatesUpToDate, stdoutExcerpt: result.stdout.trim().slice(0, 300),
			pointer: readPointer(installDir), stateCheck,
		});
	}

	for (const [scenarioName, mode, pattern] of upgradeOnly ? [] : [
		["bad_checksum_refusal", "bad_checksum", /checksum|hash|integrity|mismatch/],
		["invalid_archive_refusal", "invalid_archive", /archive|extract|tar|zip|invalid|corrupt/],
	]) {
		const { launcherSha } = await prepareInstallA();
		fixtureServer.setMode(mode);
		fixtureServer.setPackage(newPackagePath, newVersion);
		fixtureServer.clearRequests();
		const pointerBefore = readFileSync(join(installDir, "current.json"), "utf8");

		const { result, cleanupFinished } = await runUpdate();
		saveOutput(scenarioName, result);

		const outputText = `${result.stdout} ${result.stderr}`.toLowerCase();
		const refused = result.exitCode !== null && result.exitCode !== 0 && !result.killed;
		const mentionsReason = pattern.test(outputText);
		const aCheck = verifyPackageIntegrity(join(installDir, "versions", oldVersion));
		const stateCheck = verifyStateUnmodified();
		const pass = refused && mentionsReason &&
			readFileSync(join(installDir, "current.json"), "utf8") === pointerBefore &&
			JSON.stringify(installedVersions(installDir)) === JSON.stringify([oldVersion]) &&
			computeSha256(installedMhc) === launcherSha && matchesReference(aCheck, initialPackageCheck) &&
			downloadedFixture() && cleanupFinished && stateCheck.sentinelOk && stateCheck.settingsOk;
		recordScenario(scenarioName, pass, {
			exitCode: result.exitCode, refused, mentionsReason, pointer: readPointer(installDir),
			versions: installedVersions(installDir), cleanupFinished, stateCheck,
		});
		fixtureServer.setMode("normal");
	}

	if (!upgradeOnly) {
		const scenarioName = "rollback_after_installed_validation_failure";
		const { launcherSha } = await prepareInstallA();
		fixtureServer.setMode("normal");
		fixtureServer.clearRequests();

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
		const { result, cleanupFinished } = await runUpdate({
			...commonEnv, MHC_QA_ROLLBACK_MARKER: marker, MHC_QA_PROBE_VERSION: newVersion,
		});
		saveOutput(scenarioName, result);
		fixtureServer.setPackage(newPackagePath, newVersion);

		const probeRanInVersionDir = await markerObserved && samePath(readFileSync(marker, "utf8"), payloadExecutable(installDir, newVersion));
		const output = result.stdout + result.stderr;
		const restoredReported = output.includes("previous version restored");
		const post = await runVersion(installedMhc);
		const aCheck = verifyPackageIntegrity(join(installDir, "versions", oldVersion));
		const stateCheck = verifyStateUnmodified();
		const pass = result.exitCode !== null && result.exitCode !== 0 && !result.killed &&
			probeRanInVersionDir && restoredReported && downloadedFixture() && cleanupFinished &&
			pointerIs(installDir, oldVersion, null) &&
			post.ok && post.version === oldVersion &&
			computeSha256(installedMhc) === launcherSha && matchesReference(aCheck, initialPackageCheck) &&
			stateCheck.sentinelOk && stateCheck.settingsOk;
		recordScenario(scenarioName, pass, {
			updateExitCode: result.exitCode, probeRanInVersionDir, restoredReported, cleanupFinished,
			pointer: readPointer(installDir), postVersion: post.version, stateCheck,
			fault: "candidate payload passes staging validation but its post-switch launcher --version fails",
		});
	}

	if (!upgradeOnly) {
		const scenarioName = "running_sessions_survive_update";
		const { launcherSha } = await prepareInstallA();
		fixtureServer.setMode("normal");
		fixtureServer.setPackage(newPackagePath, newVersion);
		fixtureServer.clearRequests();

		const sessions = [];
		let third = null;
		let details = {};
		let pass = false;
		try {
			sessions.push(startSession(installedMhc, "session 1"), startSession(installedMhc, "session 2"));
			const readiness = await Promise.all(sessions.map(session => session.ready));
			const before = [];
			for (const session of sessions) before.push(await sessionRunsPayload(session, oldVersion, expectedASha));

			const { result, cleanupFinished } = await runUpdate();
			saveOutput(scenarioName, result);

			const alive = sessions.map(session => !session.state.exited && session.proc.exitCode === null);
			const after = [];
			for (const session of sessions) after.push(await sessionRunsPayload(session, oldVersion, expectedASha));
			const launcherVersion = await runVersion(installedMhc);
			third = startSession(installedMhc, "session 3");
			const thirdReady = await third.ready;
			const thirdRuns = await sessionRunsPayload(third, newVersion, expectedBSha);
			const aCheck = verifyPackageIntegrity(join(installDir, "versions", oldVersion));
			const bCheck = verifyPackageIntegrity(join(installDir, "versions", newVersion));
			const stateCheck = verifyStateUnmodified();
			const stillAliveAtEnd = sessions.map(session => !session.state.exited && session.proc.exitCode === null);

			pass = readiness.every(Boolean) && before.every(entry => entry.onPath && entry.shaMatches) &&
				result.exitCode === 0 && !result.killed && result.stdout.includes(`Installed mhc ${newVersion}.`) &&
				alive.every(Boolean) && after.every(entry => entry.onPath && entry.shaMatches) && stillAliveAtEnd.every(Boolean) &&
				launcherVersion.ok && launcherVersion.version === newVersion &&
				thirdReady && thirdRuns.onPath && thirdRuns.shaMatches &&
				computeSha256(installedMhc) === launcherSha &&
				pointerIs(installDir, newVersion, oldVersion) &&
				matchesReference(aCheck, initialPackageCheck) && matchesReference(bCheck, referenceBCheck) &&
				cleanupFinished && downloadedFixture() && stateCheck.sentinelOk && stateCheck.settingsOk;
			details = {
				updateExitCode: result.exitCode, readiness, before, after, alive, stillAliveAtEnd,
				launcherVersion: launcherVersion.version, thirdReady, thirdRuns,
				launcherSha, launcherAfter: computeSha256(installedMhc), pointer: readPointer(installDir),
				versions: installedVersions(installDir), cleanupFinished, stateCheck,
			};
		} catch (error) {
			details = { error: String(error) };
		} finally {
			for (const session of [...sessions, ...(third ? [third] : [])]) await stopSession(session);
		}
		recordScenario(scenarioName, pass, details);
	}

	if (!upgradeOnly) {
		const scenarioName = "retention_defers_old_version_cleanup";
		const staleUnused = "0.0.0-stale-unused";
		const staleHeld = "0.0.0-stale-held";
		await prepareInstallA();
		fixtureServer.setMode("normal");
		fixtureServer.setPackage(newPackagePath, newVersion);
		fixtureServer.clearRequests();
		copyPackage(baselineDirA, join(installDir, "versions", staleUnused));
		copyPackage(baselineDirA, join(installDir, "versions", staleHeld));

		let held = null;
		let details = {};
		let pass = false;
		try {
			held = startSession(payloadExecutable(installDir, staleHeld), "held session");
			const heldReady = await held.ready;
			const first = await runUpdate();
			saveOutput(`${scenarioName}.first`, first.result);
			const afterFirst = installedVersions(installDir);
			const heldAlive = !held.state.exited && held.proc.exitCode === null;
			await stopSession(held);

			writeFileSync(join(installDir, "current.json"), `${JSON.stringify({ schemaVersion: 1, current: oldVersion, previous: null })}\n`);
			const second = await runUpdate();
			saveOutput(`${scenarioName}.second`, second.result);
			const afterSecond = installedVersions(installDir);
			const stateCheck = verifyStateUnmodified();

			pass = heldReady && first.result.exitCode === 0 && !first.result.killed &&
				afterFirst.includes(staleHeld) && afterFirst.includes(staleUnused) &&
				afterFirst.includes(oldVersion) && afterFirst.includes(newVersion) && heldAlive &&
				second.result.exitCode === 0 && !second.result.killed &&
				JSON.stringify(afterSecond) === JSON.stringify([oldVersion, newVersion, staleHeld, staleUnused].sort()) &&
				pointerIs(installDir, newVersion, oldVersion) &&
				first.cleanupFinished && second.cleanupFinished && stateCheck.sentinelOk && stateCheck.settingsOk;
			details = { heldReady, heldAlive, afterFirst, afterSecond, pointer: readPointer(installDir), stateCheck };
		} catch (error) {
			details = { error: String(error) };
		} finally {
			if (held) await stopSession(held);
		}
		recordScenario(scenarioName, pass, details);
	}

	if (!upgradeOnly) {
		const scenarioName = "sentinel_preservation_and_cleanup";
		const stateCheck = verifyStateUnmodified();
		const remainingWorkspaces = leftoverWorkspaces(installDir);
		const cleanupOk = remainingWorkspaces.length === 0;
		const expectedBefore = EXPECTED_SCENARIOS.indexOf(scenarioName);
		const precedingScenariosPassed = scenarioReceipts.length === expectedBefore && scenarioReceipts.every(receipt => receipt.pass);
		const pass = precedingScenariosPassed && stateCheck.sentinelOk && stateCheck.settingsOk && cleanupOk;
		recordScenario(scenarioName, pass, {
			sentinelOk: stateCheck.sentinelOk,
			settingsOk: stateCheck.settingsOk,
			cleanupOk,
			remainingWorkspaces,
		});
	}

	if (!upgradeOnly) {
		const scenarioName = "startup_notice_pty_and_cache";
		await prepareInstallA();
		let ptyPass = false;
		let ptyDetails = {};

		try {
			fixtureServer.setMode("normal");
			fixtureServer.setPackage(newPackagePath, newVersion);
			fixtureServer.clearRequests();

			const cacheDir = join(agentDir, "cache");
			const cacheFile = join(cacheDir, "mhc-latest-release.json");
			mkdirSync(cacheDir, { recursive: true });
			rmSync(cacheFile, { force: true });

			const cacheAppearancePromise = waitForFileAppearance(cacheDir, "mhc-latest-release.json", sessionReadyMs);

			let onlineProc = null;
			try {
				onlineProc = spawnTerminal(installedMhc, ["--provider", "local_mock", "--model", "mock-model", "--no-tools", "--no-skills"], {
					cwd: qaRoot,
					env: { ...commonEnv, MAHO_OFFLINE: "0", TERM: "xterm-256color" },
				}, { raw: "", listeners: new Set() });

				const cacheAppeared = await cacheAppearancePromise;
				if (!cacheAppeared) {
					throw new Error("Cache file mhc-latest-release.json did not appear within bounded startup timeout via fs.watch");
				}
			} finally {
				if (onlineProc) {
					try { onlineProc.terminal.write("\u0004"); } catch {}
					try { await onlineProc.kill("SIGTERM"); } catch {}
					try { await boundedExit(onlineProc); } catch {}
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
			const capture = { raw: "", listeners: new Set() };
			const noticePromise = eventWaiter(
				capture,
				(raw) => raw.includes("mhc update") && raw.includes(newVersion),
				"startup notice in offline mode",
				sessionReadyMs,
			);
			const ptyProc = spawnTerminal(installedMhc, ["--offline", "--provider", "local_mock", "--model", "mock-model", "--no-tools", "--no-skills"], {
				cwd: qaRoot,
				env: { ...commonEnv, MAHO_OFFLINE: "1", TERM: "xterm-256color", COLORTERM: "truecolor" },
			}, capture);
			const noticeSeen = await noticePromise;
			try { ptyProc.terminal.write("\u0004"); } catch {}
			try { await ptyProc.kill("SIGTERM"); } catch {}
			try { await boundedExit(ptyProc); } catch {}

			writeFileSync(join(evidenceDir, `${scenarioName}.ansi`), capture.raw);
			writeFileSync(join(evidenceDir, `${scenarioName}.txt`), capture.raw.replace(/\x1b\[[0-9;]*[a-zA-Z]/g, ""));

			const offlineRequests = fixtureServer.getRequestCount();
			const stateCheck = verifyStateUnmodified();
			ptyPass = onlineRequests === 1 && cacheWritten && cacheValid && cachedVersion === newVersion && offlineRequests === 0 && noticeSeen === true &&
				stateCheck.sentinelOk && stateCheck.settingsOk &&
				matchesReference(verifyPackageIntegrity(join(installDir, "versions", oldVersion)), initialPackageCheck);
			ptyDetails = {
				onlineRequests, offlineRequests, cacheWritten, cacheValid, stateCheck, cachedVersion, noticeSeen,
				terminalAvailable: terminalStatus.available, terminalReason: terminalStatus.reason,
			};
		} catch (err) {
			ptyPass = false;
			ptyDetails = { error: String(err), terminalAvailable: terminalStatus.available, terminalReason: terminalStatus.reason };
		}

		recordScenario(scenarioName, ptyPass, ptyDetails);
	}

} catch (fatalError) {
	console.error("Fatal failure in QA run:", fatalError);
	recordScenario("fatal_run_error", false, { error: String(fatalError) });
} finally {
	fixtureServer.stop();
	modelServer.stop(true);
	for (const directory of [qaRoot, referenceRoot]) {
		try {
			rmSync(directory, { recursive: true, force: true });
		} catch (error) {
			console.warn(`[QA WARNING] cannot remove ${directory}: ${error}`);
		}
	}

	const completedNames = new Set(scenarioReceipts.map(r => r.scenario));
	const allExpectedPresent = EXPECTED_SCENARIOS.length === scenarioReceipts.length &&
		EXPECTED_SCENARIOS.every(name => completedNames.has(name));
	const allPassed = allExpectedPresent && scenarioReceipts.every(r => r.pass === true);

	const summary = {
		suite: "mhc native update QA",
		target: targetTuple,
		oldVersion,
		newVersion,
		intermediateVersion,
		upgradeOnly,
		completedAt: new Date().toISOString(),
		expectedScenariosCount: EXPECTED_SCENARIOS.length,
		totalScenarios: scenarioReceipts.length,
		passed: scenarioReceipts.filter(r => r.pass).length,
		failed: scenarioReceipts.filter(r => !r.pass).length,
		terminalStatus,
		allExpectedPresent,
		allPassed,
		scenarios: scenarioReceipts,
	};

	writeFileSync(join(evidenceDir, "summary.json"), JSON.stringify(summary, null, 2));
	console.log(`\nQA Run Complete. Summary written to ${join(evidenceDir, "summary.json")}`);
	console.log(`Result: ${allPassed ? "ALL REQUIRED SCENARIOS PASSED" : "FAILURES OR MISSING SCENARIOS DETECTED"}`);
	process.exit(allPassed ? 0 : 1);
}
