#!/usr/bin/env bun
import { createHash } from "node:crypto";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { createUpdateFixtureServer, detectTargetTuple } from "./qa-native-update-fixture.mjs";

const args = process.argv.slice(2);
let oldPackagePath = null;
let newPackagePath = null;
let evidenceDir = null;
let targetTuple = detectTargetTuple();
let timeoutMs = 45000;

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
	}
}

if (!oldPackagePath || !newPackagePath || !evidenceDir) {
	console.error("Usage: bun scripts/qa-native-update.mjs --old-package <A> --new-package <B> --evidence-dir <dir>");
	process.exit(2);
}

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
	if (!existsSync(filePath)) return null;
	return createHash("sha256").update(readFileSync(filePath)).digest("hex");
}

async function extractArchive(archivePath, destination) {
	mkdirSync(destination, { recursive: true });
	if (archivePath.endsWith(".zip")) {
		if (isWindows) {
			const proc = Bun.spawn(["powershell", "-NoProfile", "-Command", `Expand-Archive -LiteralPath '${archivePath}' -DestinationPath '${destination}' -Force`]);
			const code = await proc.exited;
			if (code !== 0) throw new Error(`Expand-Archive failed with code ${code}`);
		} else {
			const proc = Bun.spawn(["unzip", "-q", "-o", archivePath, "-d", destination]);
			const code = await proc.exited;
			if (code !== 0) throw new Error(`unzip failed with code ${code}`);
		}
	} else {
		const proc = Bun.spawn(["tar", "-xzf", archivePath, "-C", destination]);
		const code = await proc.exited;
		if (code !== 0) throw new Error(`tar extraction failed with code ${code}`);
	}

	const topEntries = readdirSync(destination);
	if (topEntries.length === 1 && statSync(join(destination, topEntries[0])).isDirectory()) {
		const innerDir = join(destination, topEntries[0]);
		const innerEntries = readdirSync(innerDir);
		for (const entry of innerEntries) {
			const src = join(innerDir, entry);
			const dst = join(destination, entry);
			const renameProc = Bun.spawn(isWindows ? ["cmd", "/c", "move", src, dst] : ["mv", src, dst]);
			await renameProc.exited;
		}
		rmSync(innerDir, { recursive: true, force: true });
	}
}

async function executeCommand(binaryPath, cmdArgs, options = {}) {
	const {
		cwd = dirname(binaryPath),
		env = {},
		stdin = "ignore",
		timeout = timeoutMs,
	} = options;

	const mergedEnv = {
		PATH: process.env.PATH,
		TMPDIR: options.tmpDir || tmpdir(),
		HOME: options.homeDir || tmpdir(),
		...env,
	};

	const proc = Bun.spawn([binaryPath, ...cmdArgs], {
		cwd,
		env: mergedEnv,
		stdin: typeof stdin === "string" ? "pipe" : stdin,
		stdout: "pipe",
		stderr: "pipe",
	});

	if (typeof stdin === "string" && stdin.length > 0) {
		proc.stdin.write(stdin);
		proc.stdin.flush();
		proc.stdin.end();
	}

	let killed = false;
	const killTimer = setTimeout(() => {
		killed = true;
		proc.kill("SIGKILL");
	}, timeout);

	const [stdout, stderr, exitCode] = await Promise.all([
		new Response(proc.stdout).text(),
		new Response(proc.stderr).text(),
		proc.exited,
	]);

	clearTimeout(killTimer);
	return {
		stdout,
		stderr,
		exitCode,
		killed,
	};
}

import { watch } from "node:fs";

function eventWaiter(capture, predicate, label, timeoutLimitMs = 15000) {
	return new Promise((resolveWait, rejectWait) => {
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

function awaitWindowsReceipt(targetInstallDir, timeoutLimitMs = 30000) {
	if (!isWindows) return Promise.resolve({ receiptObserved: false, receipt: null, committed: true });
	const receiptPath = join(targetInstallDir, "update-receipt.json");
	return new Promise((resolvePromise) => {
		let watcher;
		let timer;
		const checkReceipt = () => {
			if (!existsSync(receiptPath)) return;
			try {
				const content = JSON.parse(readFileSync(receiptPath, "utf8"));
				if (content.state === "committed" || content.state === "rolledback" || content.state === "failed") {
					if (timer) clearTimeout(timer);
					watcher?.close();
					resolvePromise({
						receiptObserved: true,
						receipt: content,
						committed: content.state === "committed",
					});
				}
			} catch {}
		};
		timer = setTimeout(() => {
			watcher?.close();
			resolvePromise({ receiptObserved: existsSync(receiptPath), receipt: null, committed: false, timeout: true });
		}, timeoutLimitMs);
		try {
			watcher = watch(targetInstallDir, checkReceipt);
		} catch {}
		checkReceipt();
	});
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
const initialSentinelBuffer = Buffer.from(sentinelContent);
const settingsFile = join(agentDir, "settings.json");
const initialSettingsObj = { theme: "dark", editor: "vim", customUserSetting: true };
writeFileSync(settingsFile, JSON.stringify(initialSettingsObj, null, 2));
const initialSettingsBuffer = readFileSync(settingsFile);

const fixtureServer = createUpdateFixtureServer({
	packagePath: newPackagePath,
	version: "2026.10.20.2",
	target: targetTuple,
});

const commonEnv = {
	MHC_UPDATE_RELEASE_URL: fixtureServer.releaseUrl,
	MAHO_UPDATE_URL: fixtureServer.url,
	MHC_UPDATE_BASE_URL: fixtureServer.url,
	MAHO_CODING_AGENT_DIR: agentDir,
	CODING_AGENT_DIR: agentDir,
	MAHO_OFFLINE: "0",
};

const EXPECTED_SCENARIOS = [
	"success_update_A_to_B",
	"latest_noop_clean_exit",
	"bad_checksum_refusal",
	"invalid_archive_refusal",
	"rollback_via_filesystem_failure",
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

async function resetInstallDirToA() {
	rmSync(installDir, { recursive: true, force: true });
	mkdirSync(installDir, { recursive: true });
	await extractArchive(oldPackagePath, installDir);
	if (!isWindows) {
		chmodSync(installedMhc, 0o755);
	}
	const checkA = await executeCommand(installedMhc, ["--version"], { env: commonEnv });
	const verA = checkA.stdout.trim();
	if (!verA.includes("2026.10.20.1")) {
		throw new Error(`Failed to reset installation to version A; got: ${verA}`);
	}
}

function verifyStateUnmodified() {
	const currentSentinel = existsSync(sentinelFile) ? readFileSync(sentinelFile) : null;
	const currentSettings = existsSync(settingsFile) ? readFileSync(settingsFile) : null;
	const sentinelOk = currentSentinel !== null && currentSentinel.equals(initialSentinelBuffer);
	const settingsOk = currentSettings !== null && currentSettings.equals(initialSettingsBuffer);
	return { sentinelOk, settingsOk };
}

const scenarioReceipts = [];

function recordScenario(name, pass, details = {}) {
	const record = {
		scenario: name,
		pass: Boolean(pass),
		timestamp: new Date().toISOString(),
		sentinelSha: computeSha256(sentinelFile),
		...details,
	};
	scenarioReceipts.push(record);
	console.log(`[QA SCENARIO] ${name}: ${pass ? "PASS" : "FAIL"}`);
	writeFileSync(join(evidenceDir, `${name}.receipt.json`), JSON.stringify(record, null, 2));
}

try {
	console.log(`Starting real update QA on target ${targetTuple}`);
	console.log(`Fixture loopback server listening on ${fixtureServer.url}`);

	await extractArchive(oldPackagePath, installDir);
	if (!existsSync(installedMhc)) {
		throw new Error(`Package A extraction did not yield binary: ${installedMhc}`);
	}
	if (!isWindows) {
		chmodSync(installedMhc, 0o755);
	}

	const initialVersionCheck = await executeCommand(installedMhc, ["--version"], { env: commonEnv });
	const initialVersion = initialVersionCheck.stdout.trim();
	const initialSha = computeSha256(installedMhc);
	writeFileSync(join(evidenceDir, "initial_version.txt"), initialVersion);
	writeFileSync(join(evidenceDir, "initial_binary.sha256"), `${initialSha}  ${binaryName}\n`);

	{
		const scenarioName = "success_update_A_to_B";
		fixtureServer.setMode("normal");
		fixtureServer.setPackage(newPackagePath, "2026.10.20.2");
		fixtureServer.clearRequests();
		const inventoryBefore = getSiblingInventory(installDir);

		const updateResult = await executeCommand(installedMhc, ["update"], { env: commonEnv });
		writeFileSync(join(evidenceDir, `${scenarioName}.stdout.txt`), updateResult.stdout);
		writeFileSync(join(evidenceDir, `${scenarioName}.stderr.txt`), updateResult.stderr);

		const windowsHandoff = await awaitWindowsReceipt(installDir);

		const postUpdateCheck = await executeCommand(installedMhc, ["--version"], { env: commonEnv });
		const postVersion = postUpdateCheck.stdout.trim();
		const postSha = computeSha256(installedMhc);
		const inventoryAfter = getSiblingInventory(installDir);

		const exactBVersion = postVersion.includes("2026.10.20.2");
		const hashChanged = postSha !== initialSha;
		const exitOk = updateResult.exitCode === 0;
		const stateCheck = verifyStateUnmodified();
		const windowsOk = !isWindows || windowsHandoff.committed;
		const hasManifest = "manifest.json" in inventoryAfter;
		const pass = exitOk && exactBVersion && hashChanged && stateCheck.sentinelOk && stateCheck.settingsOk && windowsOk && hasManifest;

		recordScenario(scenarioName, pass, {
			exitCode: updateResult.exitCode,
			initialVersion,
			postVersion,
			initialSha,
			postSha,
			windowsHandoff,
			networkRequests: fixtureServer.getRequestCount(),
			inventoryBefore,
			inventoryAfter,
			stateCheck,
		});
	}

	{
		const scenarioName = "latest_noop_clean_exit";
		fixtureServer.setMode("normal");
		fixtureServer.setPackage(newPackagePath, "2026.10.20.2");
		const preSha = computeSha256(installedMhc);

		const updateResult = await executeCommand(installedMhc, ["update"], { env: commonEnv });
		writeFileSync(join(evidenceDir, `${scenarioName}.stdout.txt`), updateResult.stdout);
		writeFileSync(join(evidenceDir, `${scenarioName}.stderr.txt`), updateResult.stderr);

		const postSha = computeSha256(installedMhc);
		const outputText = `${updateResult.stdout} ${updateResult.stderr}`.toLowerCase();
		const indicatesUpToDate = outputText.includes("up to date") || outputText.includes("already") || updateResult.exitCode === 0;
		const stateCheck = verifyStateUnmodified();
		const pass = updateResult.exitCode === 0 && indicatesUpToDate && preSha === postSha && stateCheck.sentinelOk && stateCheck.settingsOk;

		recordScenario(scenarioName, pass, {
			exitCode: updateResult.exitCode,
			preSha,
			postSha,
			stdoutExcerpt: updateResult.stdout.trim().slice(0, 300),
			stateCheck,
		});
	}

	{
		const scenarioName = "bad_checksum_refusal";
		await resetInstallDirToA();
		fixtureServer.setMode("bad_checksum");
		fixtureServer.setPackage(newPackagePath, "2026.10.20.2");
		const preSha = computeSha256(installedMhc);

		const updateResult = await executeCommand(installedMhc, ["update", "--force"], { env: commonEnv });
		writeFileSync(join(evidenceDir, `${scenarioName}.stdout.txt`), updateResult.stdout);
		writeFileSync(join(evidenceDir, `${scenarioName}.stderr.txt`), updateResult.stderr);

		const postSha = computeSha256(installedMhc);
		const outputText = `${updateResult.stdout} ${updateResult.stderr}`.toLowerCase();
		const refused = updateResult.exitCode !== 0;
		const mentionsIntegrity = outputText.includes("checksum") || outputText.includes("hash") || outputText.includes("integrity") || outputText.includes("mismatch");
		const stateCheck = verifyStateUnmodified();
		const pass = refused && mentionsIntegrity && preSha === postSha && stateCheck.sentinelOk && stateCheck.settingsOk;

		recordScenario(scenarioName, pass, {
			exitCode: updateResult.exitCode,
			preSha,
			postSha,
			refused,
			mentionsIntegrity,
			stateCheck,
		});
	}

	{
		const scenarioName = "invalid_archive_refusal";
		await resetInstallDirToA();
		fixtureServer.setMode("invalid_archive");
		fixtureServer.setPackage(newPackagePath, "2026.10.20.2");
		const preSha = computeSha256(installedMhc);

		const updateResult = await executeCommand(installedMhc, ["update", "--force"], { env: commonEnv });
		writeFileSync(join(evidenceDir, `${scenarioName}.stdout.txt`), updateResult.stdout);
		writeFileSync(join(evidenceDir, `${scenarioName}.stderr.txt`), updateResult.stderr);

		const postSha = computeSha256(installedMhc);
		const refused = updateResult.exitCode !== 0;
		const stateCheck = verifyStateUnmodified();
		const pass = refused && preSha === postSha && stateCheck.sentinelOk && stateCheck.settingsOk;

		recordScenario(scenarioName, pass, {
			exitCode: updateResult.exitCode,
			preSha,
			postSha,
			refused,
			stateCheck,
		});
	}

	{
		const scenarioName = "rollback_via_filesystem_failure";
		await resetInstallDirToA();
		fixtureServer.setMode("normal");
		fixtureServer.setPackage(newPackagePath, "2026.10.20.2");
		const preSha = computeSha256(installedMhc);
		const inventoryBefore = getSiblingInventory(installDir);

		const updateResult = await executeCommand(installedMhc, ["update", "--force"], { env: commonEnv });
		writeFileSync(join(evidenceDir, `${scenarioName}.stdout.txt`), updateResult.stdout);
		writeFileSync(join(evidenceDir, `${scenarioName}.stderr.txt`), updateResult.stderr);

		const postCheck = await executeCommand(installedMhc, ["--version"], { env: commonEnv });
		const postSha = computeSha256(installedMhc);
		const inventoryAfter = getSiblingInventory(installDir);
		const outputText = `${updateResult.stdout} ${updateResult.stderr}`.toLowerCase();
		const failedAsExpected = updateResult.exitCode !== 0;
		const mentionsRollback = outputText.includes("rollback") || outputText.includes("rolled back") || outputText.includes("revert");
		const stateCheck = verifyStateUnmodified();
		const binaryRestored = postCheck.exitCode === 0 && postSha === preSha;
		const pass = failedAsExpected && mentionsRollback && binaryRestored && stateCheck.sentinelOk && stateCheck.settingsOk;

		recordScenario(scenarioName, pass, {
			updateExitCode: updateResult.exitCode,
			failedAsExpected,
			mentionsRollback,
			binaryRestored,
			inventoryBefore,
			inventoryAfter,
			stateCheck,
		});
	}

	{
		const scenarioName = "active_session_refusal";
		await resetInstallDirToA();
		fixtureServer.setMode("normal");
		fixtureServer.setPackage(newPackagePath, "2026.10.20.2");

		let activeProc = null;
		let promptObserved = false;
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

		try {
			activeProc = Bun.spawn([installedMhc, "--provider", "local_mock", "--model", "mock-model", "--no-approve"], {
				cwd: projectDir,
				env: {
					...commonEnv,
					TERM: "xterm-256color",
				},
				terminal: {
					cols: 100,
					rows: 32,
					data(_terminal, chunk) {
						activeCapture.raw += Buffer.from(chunk).toString("utf8");
						for (const l of activeCapture.listeners) l();
					},
				},
			});

			promptObserved = await eventWaiter(
				activeCapture,
				(raw) => /›|❯|\>\s/.test(raw),
				"active process interactive startup",
				15000,
			);
		} catch {}

		const preSha = computeSha256(installedMhc);
		const updateResult = await executeCommand(installedMhc, ["update"], { env: commonEnv });
		writeFileSync(join(evidenceDir, `${scenarioName}.stdout.txt`), updateResult.stdout);
		writeFileSync(join(evidenceDir, `${scenarioName}.stderr.txt`), updateResult.stderr);

		const processWasStillAlive = activeProc !== null && activeProc.exitCode === null;
		if (activeProc) {
			try { activeProc.terminal.write("\u0004"); } catch {}
			try { activeProc.kill("SIGTERM"); } catch {}
			await activeProc.exited;
		}
		modelServer.stop(true);

		const postSha = computeSha256(installedMhc);
		const outputText = `${updateResult.stdout} ${updateResult.stderr}`.toLowerCase();
		const refusedActive = updateResult.exitCode !== 0 && (outputText.includes("active") || outputText.includes("session") || outputText.includes("running") || outputText.includes("lease"));
		const stateCheck = verifyStateUnmodified();
		const pass = refusedActive && processWasStillAlive && preSha === postSha && stateCheck.sentinelOk && stateCheck.settingsOk;

		recordScenario(scenarioName, pass, {
			exitCode: updateResult.exitCode,
			preSha,
			postSha,
			refusedActive,
			processWasStillAlive,
			promptObserved,
			stateCheck,
		});
	}

	{
		const scenarioName = "sentinel_preservation_and_cleanup";
		const stateCheck = verifyStateUnmodified();

		const parentDir = dirname(installDir);
		const remainingStaging = readdirSync(parentDir).filter(name => name.includes("staging") || name.includes(".backup."));
		const cleanupOk = remainingStaging.length === 0;

		const pass = stateCheck.sentinelOk && stateCheck.settingsOk && cleanupOk;
		recordScenario(scenarioName, pass, {
			sentinelOk: stateCheck.sentinelOk,
			settingsOk: stateCheck.settingsOk,
			cleanupOk,
			remainingStaging,
		});
	}

	{
		const scenarioName = "startup_notice_pty_and_cache";
		await resetInstallDirToA();
		let ptyPass = false;
		let ptyDetails = {};

		try {
			fixtureServer.setMode("normal");
			fixtureServer.setPackage(newPackagePath, "2026.10.20.9");
			fixtureServer.clearRequests();

			const checkOnline = await executeCommand(installedMhc, ["update", "--check"], {
				env: { ...commonEnv, MAHO_OFFLINE: "0" },
			});
			const onlineRequests = fixtureServer.getRequestCount();

			const cacheFile = join(agentDir, "cache", "mhc-latest-release.json");
			const cacheWritten = existsSync(cacheFile);
			let cachedVersion = null;
			if (cacheWritten) {
				try {
					const parsedCache = JSON.parse(readFileSync(cacheFile, "utf8"));
					cachedVersion = parsedCache?.release?.version ?? null;
				} catch {}
			}

			fixtureServer.clearRequests();
			const capture = { raw: "", listeners: new Set() };
			const ptyProc = Bun.spawn([installedMhc, "--offline", "--no-tools", "--no-skills"], {
				cwd: qaRoot,
				env: {
					...commonEnv,
					MAHO_OFFLINE: "1",
					TERM: "xterm-256color",
					COLORTERM: "truecolor",
				},
				terminal: {
					cols: 100,
					rows: 32,
					data(_terminal, chunk) {
						const text = Buffer.from(chunk).toString("utf8");
						capture.raw += text;
						for (const listener of capture.listeners) listener();
					},
				},
			});

			const noticeSeen = await eventWaiter(
				capture,
				(raw) => raw.includes("Update") || raw.includes("mhc update") || raw.includes("2026.10.20.9"),
				"startup notice in offline mode",
				15000,
			);

			try { ptyProc.terminal.write("\u0004"); } catch {}
			try { ptyProc.kill("SIGTERM"); } catch {}
			await ptyProc.exited;

			writeFileSync(join(evidenceDir, `${scenarioName}.ansi`), capture.raw);
			writeFileSync(join(evidenceDir, `${scenarioName}.txt`), capture.raw.replace(/\x1b\[[0-9;]*[a-zA-Z]/g, ""));

			const offlineRequests = fixtureServer.getRequestCount();
			ptyPass = onlineRequests >= 1 && cacheWritten && cachedVersion === "2026.10.20.9" && offlineRequests === 0 && noticeSeen === true;
			ptyDetails = {
				onlineRequests,
				offlineRequests,
				cacheWritten,
				cachedVersion,
				noticeSeen,
			};
		} catch (err) {
			ptyPass = false;
			ptyDetails = { error: String(err) };
		}

		recordScenario(scenarioName, ptyPass, ptyDetails);
	}

} catch (fatalError) {
	console.error("Fatal failure in QA run:", fatalError);
	recordScenario("fatal_run_error", false, { error: String(fatalError) });
} finally {
	fixtureServer.stop();
	rmSync(qaRoot, { recursive: true, force: true });

	const completedNames = new Set(scenarioReceipts.map(r => r.scenario));
	const allExpectedPresent = EXPECTED_SCENARIOS.length === scenarioReceipts.length &&
		EXPECTED_SCENARIOS.every(name => completedNames.has(name));
	const allPassed = allExpectedPresent && scenarioReceipts.every(r => r.pass === true);

	const summary = {
		suite: "mhc native update QA",
		target: targetTuple,
		completedAt: new Date().toISOString(),
		expectedScenariosCount: EXPECTED_SCENARIOS.length,
		totalScenarios: scenarioReceipts.length,
		passed: scenarioReceipts.filter(r => r.pass).length,
		failed: scenarioReceipts.filter(r => !r.pass).length,
		allExpectedPresent,
		allPassed,
		scenarios: scenarioReceipts,
	};

	writeFileSync(join(evidenceDir, "summary.json"), JSON.stringify(summary, null, 2));
	console.log(`\nQA Run Complete. Summary written to ${join(evidenceDir, "summary.json")}`);
	console.log(`Result: ${allPassed ? "ALL SCENARIOS PASSED" : "FAILURES DETECTED"}`);
	if (!allPassed) {
		process.exit(1);
	}
	process.exit(0);
}
