#!/usr/bin/env bun
// Real-surface QA driver for the pinned Senpi supervisor / multi-session host / watchdog
// integration (runtime lane RU-03). Authored BEFORE the product edits so the commands,
// observables and cleanup below are fixed by the pinned upstream contract, not by the result.
//
//   bun tools/latest-omo-host-qa.mjs --binary <built mhc> --evidence <isolated E> \
//        [--install <staged tree>] [--scenario lifecycle|idle-exit|watchdog|mounted-skills|all]
//
// Scenario options (authored up front; every one is a REAL registered surface):
//
//   --binary <path>   the built `mhc` binary under test (REQUIRED; sha256 + mtime captured before
//                     and after the run, so a binary that moved under the run is unverified).
//   --evidence <dir>  isolated evidence root (REQUIRED); artifacts land in <E>/qa/.
//   --install <dir>   the staged product tree (`tools/package-native.mjs --skill-source latest`),
//                     i.e. `<dir>/skills` + `<dir>/mhc`. Used only by `mounted-skills`.
//   --scenario <name> one of lifecycle | idle-exit | watchdog | mounted-skills | all (default all).
//
// The scenarios, their exact invocation, observable and cleanup:
//
//   lifecycle  mhc host ensure --socket <home>/rpc.sock --json
//              mhc host status --socket <home>/rpc.sock --json
//              mhc host handoff --socket <home>/rpc.sock --json
//              mhc host stop   --socket <home>/rpc.sock --drain --json
//              mhc host status --socket <home>/rpc.sock --json
//     observable  exactly one JSON line each: ensure `action=start` + `pid>0` (exit 0); status
//                 `reachable=true` (exit 0); handoff is a well-formed JSON line with an `action`
//                 and exit in {0,3} (a same-build handoff is refused); stop `action=drained|stopped`
//                 (exit 0); the final status is `reachable=false` (exit 3, refused).
//     cleanup     the supervisor is NOT a child of this driver, so its death is evidenced by the
//                 OWNED lifecycle signal (the daemon registration pointer the watchdog removes),
//                 subscribed BEFORE `host stop`; then rm -rf the isolated home; assert the public
//                 socket and the registration pointer are gone. No foreign-pid poll is a verdict.
//
//   idle-exit  mhc --internal-rpc-host-supervisor --socket <home>/rpc.sock --agent-dir <home>/agent
//              (SENPI_RPC_HOST_IDLE_EXIT_MS=<short>) — the hidden supervisor route run directly.
//     observable  the supervisor exits on its own within the bound; the host child it spawned is
//                 gone; the public socket is removed.
//     cleanup     the driver OWNS the supervisor as a child: readiness is the socket appearing
//                 (a filesystem subscription registered before the spawn), and its exit is
//                 `proc.exited` — never a poll. Then bounded cleanup of the owned process.
//
//   watchdog   mhc host ensure --socket <home>/rpc.sock --json  (real ensure -> supervisor -> host)
//              then SIGKILL the supervisor pid from the ensure payload.
//     observable  the host child (pid from the daemon registration `host.pid`) is ALIVE right
//                 before the kill and GONE within the bound after it — the watchdog fired on the
//                 inherited pipe EOF / reparenting; the supervisor's scratch + cleanup paths are
//                 removed. The host child is a FOREIGN process, so its death is evidenced by the
//                 owned lifecycle signal the watchdog itself produces: the registration pointer is
//                 removed. If that pointer never appears, the scenario is explicitly inconclusive
//                 rather than a pid-poll proxy pass.
//     cleanup     assert the registration pointer is gone and the isolated home is removed.
//
//   mounted-skills  the OMO MOUNTED consumer, not the generic CLI listing. `mhc --mode rpc
//                   --multi-session --listen unix://<home>/host.sock` is started directly with
//                   `OMO_SENPI_SKILLS_ROOT=<install>/skills` (the `manifest.loader` binding) and an
//                   EMPTY isolated agent dir, so `<agent_dir>/skills` contributes nothing; the
//                   packaged root is the only possible source. On the host socket:
//                     {"id":1,"type":"open_session","cwd":"<home>/project"}
//                     {"id":2,"type":"prompt","sessionId":"<sid>","message":"mass ulw: ..."}
//                     {"id":3,"type":"get_entries","sessionId":"<sid>"}
//     observable  a session entry with `customType == "omo-mass-ulw:skill-pointer"` whose text
//                 names `<install>/skills/mass-ulw/SKILL.md` - the OMO component's own pointer,
//                 emitted from the root `omo_mount.rs` resolved. A generic `skills/list` line is
//                 NOT this proof (it reads the app-server loader, a different seam).
//     cleanup     the driver owns the host as a child (await its exit); rm -rf the isolated home;
//                 assert the socket is gone. A host that cannot open a session records blocked.
//
// Every scenario runs with an isolated HOME + MAHO_CODING_AGENT_DIR, an offline agent config and a
// short idle/empty policy. Process death is awaited by the exact event: an owned child's `exited`
// promise, an `fs.watch` subscription on the path registered BEFORE the trigger, or the watchdog's
// own registration-pointer removal. A fixed-interval poll is never the verdict. A surface the
// binary does not reach records status "blocked" with the exact missing contract; never a proxy
// pass.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, watch, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";

const SCHEMA = "latest-omo-host-qa/v1";
const DAEMON_LAYOUT_DIR = "rpc-host-daemon";
// senpi `HOST_IDLE_EXIT_MS_ENV` / `RPC_HOST_EMPTY_EXIT_MS_ENV`: short windows so a real host ends
// on its own inside the driver's bound instead of the 15-minute production default.
const SUPERVISOR_IDLE_EXIT_MS = "4000";
const HOST_EMPTY_EXIT_MS = "2000";
const READY_TIMEOUT_MS = 30_000;
const DEATH_TIMEOUT_MS = 15_000;
// The bounded window a graceful (SIGTERM) shutdown gets before the driver forces the owned process.
const GRACE_TIMEOUT_MS = 5_000;
// The bounded reap window for a stream/exit after the driver has signalled its owned process.
const REAP_TIMEOUT_MS = 5_000;

const delay = (ms) => new Promise((resolvePromise) => setTimeout(resolvePromise, ms));

function usage(message) {
	console.error(`latest-omo-host-qa: ${message}`);
	console.error("usage: bun tools/latest-omo-host-qa.mjs --binary <mhc> --evidence <E> [--install <staged tree>] [--scenario lifecycle|idle-exit|watchdog|mounted-skills|all]");
	process.exit(2);
}

function sha256(data) {
	return createHash("sha256").update(data).digest("hex");
}

function parseArgs(argv) {
	const args = { scenario: "all" };
	for (let i = 0; i < argv.length; i++) {
		const key = argv[i];
		if (key === "--binary") args.binary = argv[++i];
		else if (key === "--evidence") args.evidence = argv[++i];
		else if (key === "--scenario") args.scenario = argv[++i];
		else if (key === "--install") args.install = argv[++i];
		else usage(`unknown argument ${key}`);
	}
	if (!args.binary) usage("--binary is required");
	if (!args.evidence) usage("--evidence is required");
	return args;
}

const newHome = () => mkdtempSync(join(tmpdir(), "host-qa-home-"));

/** The daemon directory senpi derives from the socket: `<agent>/rpc-host-daemon/<sha256(sock)[..16]>`. */
function daemonDir(agentDir, socket) {
	return join(agentDir, DAEMON_LAYOUT_DIR, sha256(socket).slice(0, 16));
}

/** One isolated environment for every scenario: no ambient auth, no shared daemon. */
function isolatedEnv(home) {
	const agent = join(home, "agent");
	mkdirSync(agent, { recursive: true });
	writeFileSync(join(agent, "models.json"), JSON.stringify({ providers: {} }));
	return {
		PATH: process.env.PATH,
		HOME: home,
		MAHO_CODING_AGENT_DIR: agent,
		MAHO_RPC_HOST_IDLE_EXIT_MS: SUPERVISOR_IDLE_EXIT_MS,
		SENPI_RPC_HOST_IDLE_EXIT_MS: SUPERVISOR_IDLE_EXIT_MS,
		SENPI_RPC_HOST_EMPTY_EXIT_MS: HOST_EMPTY_EXIT_MS,
		MAHO_RPC_HOST_EMPTY_EXIT_MS: HOST_EMPTY_EXIT_MS,
	};
}

/**
 * One bounded one-shot invocation. The kill timer is armed at `timeoutMs`, so `await proc.exited`
 * resolves either when the OWNED child exits on its own or when the timer signals it - never
 * before, and the timer is never cleared while the child is still alive. Both captured streams are
 * then reaped under a further bound; an incomplete stream is `null` with `streamsComplete: false`,
 * which callers MUST treat as a failure rather than silently reading "".
 */
async function run(binary, args, { cwd, env, timeoutMs = 30_000 }) {
	const proc = Bun.spawn([binary, ...args], { cwd, env, stdin: "ignore", stdout: "pipe", stderr: "pipe" });
	const stdoutText = new Response(proc.stdout).text().catch(() => null);
	const stderrText = new Response(proc.stderr).text().catch(() => null);
	let timedOut = false;
	const timer = setTimeout(() => {
		timedOut = true;
		try {
			process.kill(proc.pid, "SIGKILL");
		} catch {
			/* already gone */
		}
	}, timeoutMs);
	try {
		const exitCode = await proc.exited;
		clearTimeout(timer);
		const [stdout, stderr] = await Promise.all([
			Promise.race([stdoutText, delay(REAP_TIMEOUT_MS).then(() => null)]),
			Promise.race([stderrText, delay(REAP_TIMEOUT_MS).then(() => null)]),
		]);
		return { exitCode: timedOut ? null : exitCode, stdout, stderr, timedOut, exited: true, streamsComplete: stdout !== null && stderr !== null };
	} finally {
		clearTimeout(timer);
	}
}

/** Start consuming a pipe immediately, so a chatty host can never block on a full pipe. */
function drain(stream) {
	return new Response(stream).text().catch(() => null);
}

/** The single JSON line a `mhc host` request prints, or null when the shape is wrong. */
function oneJsonLine(stdout) {
	if (typeof stdout !== "string") return null;
	const lines = stdout.split("\n").map((line) => line.trim()).filter((line) => line.length > 0);
	if (lines.length !== 1) return null;
	try {
		return JSON.parse(lines[0]);
	} catch {
		return null;
	}
}

function pidAlive(pid) {
	try {
		process.kill(pid, 0);
		return true;
	} catch (cause) {
		return !(cause instanceof Error && "code" in cause && cause.code === "ESRCH");
	}
}

/**
 * Bounded await of a path APPEARING, driven by the filesystem event rather than a poll. Register
 * BEFORE the spawn that creates it, so the creation cannot be missed.
 */
function waitForPathAppear(path, timeoutMs) {
	return new Promise((resolvePromise) => {
		if (existsSync(path)) return resolvePromise(true);
		const directory = dirname(path);
		mkdirSync(directory, { recursive: true });
		const watcher = watch(directory, () => {
			if (existsSync(path)) {
				watcher.close();
				clearTimeout(timer);
				resolvePromise(true);
			}
		});
		const timer = setTimeout(() => {
			watcher.close();
			resolvePromise(existsSync(path));
		}, timeoutMs);
	});
}

/**
 * Bounded await of a path disappearing, driven by the filesystem event rather than a sleep: the
 * watcher is registered on the parent BEFORE the trigger, so the removal cannot be missed.
 */
function waitForPathGone(path, timeoutMs) {
	return new Promise((resolvePromise) => {
		if (!existsSync(path)) return resolvePromise(true);
		const watcher = watch(dirname(path), () => {
			if (!existsSync(path)) {
				watcher.close();
				clearTimeout(timer);
				resolvePromise(true);
			}
		});
		const timer = setTimeout(() => {
			watcher.close();
			resolvePromise(!existsSync(path));
		}, timeoutMs);
	});
}

/** The daemon registration pointer the watchdog removes on a supervisor death. */
function registrationPointer(socket, agent) {
	return join(daemonDir(agent, socket), "host.pid");
}

/**
 * The host child's pid, read exactly as `read_host_registration` does: the pointer file names the
 * current generation directory, and that generation's own `host.pid` holds the pid. The pointer
 * itself carries no pid, so a pid read straight off it is not the registration contract.
 */
function readRegistrationPid(socket, agent) {
	const pointer = registrationPointer(socket, agent);
	if (!existsSync(pointer)) return null;
	try {
		const record = JSON.parse(readFileSync(pointer, "utf8"));
		if (typeof record.instance_id !== "string") return null;
		const generation = join(daemonDir(agent, socket), record.generation_dir ?? join("generations", record.instance_id), "host.pid");
		if (!existsSync(generation)) return null;
		const pid = JSON.parse(readFileSync(generation, "utf8")).pid;
		return typeof pid === "number" && pid > 0 ? pid : null;
	} catch {
		return null;
	}
}

/** Best-effort orphan sweep for a supervisor group this driver did not spawn. Never a verdict. */
function killGroup(pid) {
	for (const signal of ["SIGTERM", "SIGKILL"]) {
		try {
			process.kill(-pid, signal);
		} catch {
			/* group already gone */
		}
	}
}

/**
 * Bounded cleanup of an OWNED child: subscribe the socket removal BEFORE the trigger, SIGTERM the
 * process the driver owns (SIGKILL cannot unlink the socket), await its own `exited` promise, and
 * only then force-kill under a bound. Both the graceful and forced outcomes are reported.
 */
async function shutdownOwnedHost(receipt, { proc, socket }) {
	const socketGone = waitForPathGone(socket, DEATH_TIMEOUT_MS);
	const exited = proc.exited.then(() => true).catch(() => true);
	try {
		process.kill(proc.pid, "SIGTERM");
	} catch {
		/* already gone */
	}
	receipt.cleanup.gracefulExit = await Promise.race([exited, delay(GRACE_TIMEOUT_MS).then(() => false)]);
	if (!receipt.cleanup.gracefulExit) {
		try {
			process.kill(proc.pid, "SIGKILL");
		} catch {
			/* already gone */
		}
		receipt.cleanup.forcedKill = true;
		receipt.cleanup.forcedExit = await Promise.race([exited, delay(REAP_TIMEOUT_MS).then(() => false)]);
	}
	receipt.cleanup.socketGone = await socketGone;
}

/** A persistent JSONL client on the host socket: one request per id, answers keyed by id. */
async function connectHost(socketPath, timeoutMs) {
	const client = { buffer: "", waiters: new Map(), nextId: 1, socket: null };
	client.socket = await Bun.connect({
		unix: socketPath,
		socket: {
			data(_socket, chunk) {
				client.buffer += chunk.toString();
				let index;
				while ((index = client.buffer.indexOf("\n")) >= 0) {
					const line = client.buffer.slice(0, index);
					client.buffer = client.buffer.slice(index + 1);
					let message;
					try {
						message = JSON.parse(line);
					} catch {
						continue;
					}
					if (message && typeof message.id === "number" && client.waiters.has(message.id)) {
						const settle = client.waiters.get(message.id);
						client.waiters.delete(message.id);
						settle(message);
					}
				}
			},
			close() {
				for (const settle of client.waiters.values()) settle(null);
				client.waiters.clear();
			},
			error() {},
		},
	});
	client.request = (fields, ms = timeoutMs) =>
		new Promise((resolve) => {
			const id = client.nextId++;
			const timer = setTimeout(() => {
				client.waiters.delete(id);
				resolve(null);
			}, ms);
			client.waiters.set(id, (message) => {
				clearTimeout(timer);
				resolve(message);
			});
			client.socket.write(JSON.stringify({ id, ...fields }) + "\n");
		});
	client.close = () => {
		try {
			client.socket.end();
		} catch {
			/* already closed */
		}
	};
	return client;
}

// ---------------------------------------------------------------------------------------------
// scenarios
// ---------------------------------------------------------------------------------------------

async function scenarioLifecycle(ctx) {
	const home = newHome();
	const artifacts = [];
	const env = isolatedEnv(home);
	const socket = join(home, "rpc.sock");
	const agent = env.MAHO_CODING_AGENT_DIR;
	const receipt = { home, socket, agent, steps: [], cleanup: {} };
	let supervisorPid = null;
	let ok = false;
	let detail = "";
	try {
		const ensure = await run(ctx.binary, ["host", "ensure", "--socket", socket, "--json"], { cwd: home, env });
		const ensureLine = oneJsonLine(ensure.stdout);
		receipt.steps.push({ step: "ensure", exitCode: ensure.exitCode, timedOut: ensure.timedOut, streamsComplete: ensure.streamsComplete, line: ensureLine, stderr: ensure.stderr?.trim() ?? null });
		const ensured = ensure.exitCode === 0 && ensureLine?.action === "start" && typeof ensureLine.pid === "number" && ensureLine.pid > 0;
		supervisorPid = ensured ? ensureLine.pid : null;

		const status = await run(ctx.binary, ["host", "status", "--socket", socket, "--json"], { cwd: home, env });
		const statusLine = oneJsonLine(status.stdout);
		receipt.steps.push({ step: "status", exitCode: status.exitCode, timedOut: status.timedOut, streamsComplete: status.streamsComplete, line: statusLine, stderr: status.stderr?.trim() ?? null });
		const reachable = status.exitCode === 0 && statusLine?.reachable === true;

		const handoff = await run(ctx.binary, ["host", "handoff", "--socket", socket, "--json"], { cwd: home, env });
		const handoffLine = oneJsonLine(handoff.stdout);
		receipt.steps.push({ step: "handoff", exitCode: handoff.exitCode, timedOut: handoff.timedOut, streamsComplete: handoff.streamsComplete, line: handoffLine, stderr: handoff.stderr?.trim() ?? null });
		const handoffReported = [0, 3].includes(handoff.exitCode) && typeof handoffLine?.action === "string";

		// Subscribe BEFORE the stop trigger: the watchdog removes the registration pointer as part
		// of the supervisor's own teardown, which is the owned lifecycle signal for a non-child.
		const pointerGone = waitForPathGone(registrationPointer(socket, agent), DEATH_TIMEOUT_MS);
		const stop = await run(ctx.binary, ["host", "stop", "--socket", socket, "--drain", "--json"], { cwd: home, env });
		const stopLine = oneJsonLine(stop.stdout);
		receipt.steps.push({ step: "stop", exitCode: stop.exitCode, timedOut: stop.timedOut, streamsComplete: stop.streamsComplete, line: stopLine, stderr: stop.stderr?.trim() ?? null });
		const stopped = stop.exitCode === 0 && ["drained", "stopped"].includes(stopLine?.action);
		receipt.cleanup.registrationGone = await pointerGone;

		const after = await run(ctx.binary, ["host", "status", "--socket", socket, "--json"], { cwd: home, env });
		const afterLine = oneJsonLine(after.stdout);
		receipt.steps.push({ step: "status-after-stop", exitCode: after.exitCode, timedOut: after.timedOut, streamsComplete: after.streamsComplete, line: afterLine, stderr: after.stderr?.trim() ?? null });
		const gone = after.exitCode === 3 && afterLine?.reachable === false;

		ok = ensured && reachable && handoffReported && stopped && gone
			&& receipt.steps.every((step) => step.streamsComplete === true && step.timedOut === false);
		detail = `ensured=${ensured} reachable=${reachable} handoffReported=${handoffReported} stopped=${stopped} gone=${gone}`;
	} catch (error) {
		detail = `driver error ${error.message}`;
	} finally {
		// Best-effort orphan sweep only; the verdict below rests on the owned signals.
		if (supervisorPid !== null && pidAlive(supervisorPid)) killGroup(supervisorPid);
		receipt.cleanup.socketGone = await waitForPathGone(socket, DEATH_TIMEOUT_MS);
		rmSync(home, { recursive: true, force: true });
		receipt.cleanup.homeRemoved = !existsSync(home);
	}
	writeFileSync(join(ctx.qaRoot, "lifecycle-receipt.json"), JSON.stringify(receipt, null, 2) + "\n");
	artifacts.push("lifecycle-receipt.json");
	const cleanupOk = receipt.cleanup.registrationGone === true && receipt.cleanup.socketGone === true && receipt.cleanup.homeRemoved === true;
	return {
		status: ok && cleanupOk ? "pass" : "blocked",
		blocker: ok && cleanupOk ? null : `lifecycle: ${detail} cleanup=${JSON.stringify(receipt.cleanup)} (host ensure/status/handoff/stop must round-trip through the real supervisor)`,
		artifacts,
		cleanup_ok: cleanupOk,
	};
}

async function scenarioIdleExit(ctx) {
	const home = newHome();
	const artifacts = [];
	const env = isolatedEnv(home);
	const socket = join(home, "rpc.sock");
	const agent = env.MAHO_CODING_AGENT_DIR;
	const receipt = { home, socket, agent, exitCode: null, hostChildPid: null, cleanup: {} };
	let proc = null;
	let ok = false;
	let detail = "";
	try {
		// Subscribe to the public socket appearing BEFORE the spawn, so readiness is an event, not
		// a poll; the supervisor is this driver's OWN child, so its exit is `proc.exited`.
		const appeared = waitForPathAppear(socket, READY_TIMEOUT_MS);
		proc = Bun.spawn(
			[ctx.binary, "--internal-rpc-host-supervisor", "--socket", socket, "--agent-dir", agent],
			{ cwd: home, env, detached: true, stdin: "ignore", stdout: "pipe", stderr: "pipe" },
		);
		const stdoutText = drain(proc.stdout);
		const stderrText = drain(proc.stderr);
		const ready = await appeared;
		receipt.hostChildPid = readRegistrationPid(socket, agent);
		const exited = await Promise.race([proc.exited.then((code) => code), delay(READY_TIMEOUT_MS).then(() => null)]);
		receipt.exitCode = exited;
		const [stdout, stderr] = await Promise.all([
			Promise.race([stdoutText, delay(REAP_TIMEOUT_MS).then(() => null)]),
			Promise.race([stderrText, delay(REAP_TIMEOUT_MS).then(() => null)]),
		]);
		receipt.stdoutBytes = stdout === null ? null : stdout.length;
		receipt.stderr = stderr === null ? null : stderr.trim();
		receipt.streamsComplete = stdout !== null && stderr !== null;
		const selfExited = exited === 0;
		// The host child is foreign; socket removal proves the endpoint was unlinked, so the scenario
		// records `hostDeath: "unproved"` rather than claiming process death.
		const socketGone = !existsSync(socket);
		receipt.hostDeath = "unproved";
		ok = ready && selfExited && socketGone && receipt.streamsComplete === true;
		detail = `ready=${ready} exit=${exited} socketGone=${socketGone} streamsComplete=${receipt.streamsComplete} hostDeath=${receipt.hostDeath}`;
	} catch (error) {
		detail = `driver error ${error.message}`;
	} finally {
		if (proc !== null) {
			// The supervisor is owned: subscribe the socket removal BEFORE the trigger, then await
			// the owned exit; only force under a bound if it is still alive.
			await shutdownOwnedHost(receipt, { proc, socket });
		}
		rmSync(home, { recursive: true, force: true });
		receipt.cleanup.homeRemoved = !existsSync(home);
	}
	writeFileSync(join(ctx.qaRoot, "idle-exit-receipt.json"), JSON.stringify(receipt, null, 2) + "\n");
	artifacts.push("idle-exit-receipt.json");
	const cleanupOk = receipt.cleanup.gracefulExit === true && receipt.cleanup.socketGone === true && receipt.cleanup.homeRemoved === true && receipt.streamsComplete === true;
	return {
		status: "inconclusive",
		blocker: `idle-exit: ${detail} supervisorFacts=${ok} cleanup=${JSON.stringify(receipt.cleanup)} (the supervised host child is foreign; socket removal proves unlink only, so the required child-gone observable is unproved and this scenario is not a pass)`,
		artifacts,
		cleanup_ok: cleanupOk,
	};
}

async function scenarioWatchdog(ctx) {
	const home = newHome();
	const artifacts = [];
	const env = isolatedEnv(home);
	const socket = join(home, "rpc.sock");
	const agent = env.MAHO_CODING_AGENT_DIR;
	const receipt = { home, socket, agent, supervisorPid: null, hostChildPid: null, aliveBeforeKill: false, pointerGoneAfterKill: false, cleanup: {} };
	let ok = false;
	let detail = "";
	try {
		const ensure = await run(ctx.binary, ["host", "ensure", "--socket", socket, "--json"], { cwd: home, env });
		const line = oneJsonLine(ensure.stdout);
		receipt.supervisorPid = ensure.exitCode === 0 && typeof line?.pid === "number" ? line.pid : null;
		receipt.hostChildPid = readRegistrationPid(socket, agent);
		receipt.ensure = { exitCode: ensure.exitCode, timedOut: ensure.timedOut, streamsComplete: ensure.streamsComplete, line, stderr: ensure.stderr?.trim() ?? null };
		const started = receipt.supervisorPid !== null && receipt.hostChildPid !== null;
		// A single precondition check (never a poll): the host must be ALIVE before the kill, or
		// "gone after" proves nothing.
		receipt.aliveBeforeKill = started && pidAlive(receipt.hostChildPid);
		if (started) {
			// The host child is foreign; its death is evidenced by the OWNED lifecycle signal the
			// watchdog produces: the daemon registration pointer the cleanup removes. Subscribe
			// BEFORE the trigger.
			const pointerGone = waitForPathGone(registrationPointer(socket, agent), DEATH_TIMEOUT_MS);
			// SIGKILL runs no handler anywhere: only the OS lifetime binding can end the host.
			try {
				process.kill(receipt.supervisorPid, "SIGKILL");
			} catch (cause) {
				detail = `kill failed: ${cause.message}`;
			}
			receipt.pointerGoneAfterKill = await pointerGone;
		}
		const socketGone = !existsSync(socket);
		// The registration-pointer removal proves the pointer was UNLINKED, never that the foreign
		// host process died, and a foreign pid poll is not a verdict. With no registered owned
		// lifetime proof available, the scenario is INCONCLUSIVE rather than a proxy pass.
		receipt.hostDeath = "unproved";
		ok = false;
		detail = detail || `started=${started} aliveBeforeKill=${receipt.aliveBeforeKill} pointerGoneAfterKill=${receipt.pointerGoneAfterKill} socketGone=${socketGone} hostDeath=unproved (no registered owned lifetime signal can prove the foreign host died)`;
	} catch (error) {
		detail = `driver error ${error.message}`;
	} finally {
		// Best-effort orphan sweep of a foreign group; never the verdict.
		if (receipt.supervisorPid !== null && pidAlive(receipt.supervisorPid)) killGroup(receipt.supervisorPid);
		if (receipt.hostChildPid !== null && pidAlive(receipt.hostChildPid)) {
			try {
				process.kill(receipt.hostChildPid, "SIGKILL");
			} catch {
				/* already gone */
			}
		}
		receipt.cleanup.pointerGone = !existsSync(registrationPointer(socket, agent));
		receipt.cleanup.socketGone = !existsSync(socket);
		rmSync(home, { recursive: true, force: true });
		receipt.cleanup.homeRemoved = !existsSync(home);
	}
	writeFileSync(join(ctx.qaRoot, "watchdog-receipt.json"), JSON.stringify(receipt, null, 2) + "\n");
	artifacts.push("watchdog-receipt.json");
	const cleanupOk = receipt.cleanup.pointerGone === true && receipt.cleanup.socketGone === true && receipt.cleanup.homeRemoved === true;
	return {
		status: "inconclusive",
		blocker: `watchdog: ${detail} cleanup=${JSON.stringify(receipt.cleanup)} (the registration-pointer removal proves unlink only; no registered owned lifetime signal proves the foreign host died)`,
		artifacts,
		cleanup_ok: cleanupOk,
	};
}

async function scenarioMountedSkills(ctx) {
	const artifacts = [];
	const receipt = { install: ctx.install ?? null, host: null, session: null, pointer: null, cleanup: {} };
	let ok = false;
	let detail = "";
	if (!ctx.install) {
		return { status: "blocked", blocker: "mounted-skills: --install <staged tree> is required (tools/package-native.mjs --skill-source latest)", artifacts, cleanup_ok: true };
	}
	const skillsRoot = join(ctx.install, "skills");
	if (!existsSync(join(skillsRoot, "mass-ulw", "SKILL.md"))) {
		return { status: "blocked", blocker: `mounted-skills: ${skillsRoot}/mass-ulw/SKILL.md is absent from the staged tree`, artifacts, cleanup_ok: true };
	}
	const home = newHome();
	const env = isolatedEnv(home);
	// The packaged root is the ONLY source: the isolated agent dir holds no skills of its own.
	env.OMO_SENPI_SKILLS_ROOT = skillsRoot;
	const socket = join(home, "host.sock");
	const project = join(home, "project");
	mkdirSync(project, { recursive: true });
	// Subscribe BEFORE the spawn: readiness is the socket appearing, not a poll.
	const appeared = waitForPathAppear(socket, READY_TIMEOUT_MS);
	const host = Bun.spawn([ctx.binary, "--mode", "rpc", "--multi-session", "--listen", `unix://${socket}`], {
		cwd: home,
		env,
		detached: true,
		stdin: "ignore",
		stdout: "pipe",
		stderr: "pipe",
	});
	receipt.host = host.pid;
	// Drain BOTH pipes from the instant of spawn, or the host can block writing a full pipe.
	const stdoutText = drain(host.stdout);
	const stderrText = drain(host.stderr);
	let client = null;
	try {
		const ready = await appeared;
		if (!ready) {
			detail = "the host never bound its socket";
		} else {
		client = await Promise.race([connectHost(socket, READY_TIMEOUT_MS), delay(READY_TIMEOUT_MS).then(() => null)]);
		if (client === null) {
			detail = "the host socket did not accept a bounded connection";
		} else {
		const opened = await client.request({ type: "open_session", cwd: project });
		receipt.session = opened?.data?.sessionId ?? null;
		if (receipt.session) {
			await client.request({ type: "prompt", sessionId: receipt.session, message: "mass ulw: add a tiny feature" });
			const entries = await client.request({ type: "get_entries", sessionId: receipt.session });
			const pointer = (entries?.data?.entries ?? []).find((entry) => entry?.customType === "omo-mass-ulw:skill-pointer");
			receipt.pointer = pointer ? JSON.stringify(pointer).slice(0, 2000) : null;
			const text = pointer ? JSON.stringify(pointer) : "";
			ok = text.includes(`${skillsRoot}/mass-ulw/SKILL.md`) || text.includes(`${skillsRoot}mass-ulw/SKILL.md`);
			detail = `session=${receipt.session} pointer=${pointer ? "present" : "absent"} namesRoot=${ok}`;
		} else {
			detail = "the host answered no sessionId to open_session";
		}
		}
		}
	} catch (error) {
		detail = `driver error ${error.message}`;
	} finally {
		if (client) client.close();
		// Owned host: socket removal is subscribed inside shutdownOwnedHost BEFORE the SIGTERM.
		await shutdownOwnedHost(receipt, { proc: host, socket });
		const [stdout, stderr] = await Promise.all([
			Promise.race([stdoutText, delay(REAP_TIMEOUT_MS).then(() => null)]),
			Promise.race([stderrText, delay(REAP_TIMEOUT_MS).then(() => null)]),
		]);
		receipt.streamsComplete = stdout !== null && stderr !== null;
		receipt.stdoutBytes = stdout === null ? null : stdout.length;
		receipt.stderr = stderr === null ? null : stderr.trim();
		rmSync(home, { recursive: true, force: true });
		receipt.cleanup.homeRemoved = !existsSync(home);
	}
	writeFileSync(join(ctx.qaRoot, "mounted-skills-receipt.json"), JSON.stringify(receipt, null, 2) + "\n");
	artifacts.push("mounted-skills-receipt.json");
	const cleanupOk = receipt.cleanup.gracefulExit === true && receipt.cleanup.socketGone === true && receipt.cleanup.homeRemoved === true && receipt.streamsComplete === true;
	return {
		status: ok && cleanupOk ? "pass" : "blocked",
		blocker: ok && cleanupOk ? null : `mounted-skills: ${detail} cleanup=${JSON.stringify(receipt.cleanup)} (the OMO mount must emit its mass-ulw pointer from OMO_SENPI_SKILLS_ROOT)`,
		artifacts,
		cleanup_ok: cleanupOk,
	};
}

// ---------------------------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------------------------

const SCENARIOS = { lifecycle: scenarioLifecycle, "idle-exit": scenarioIdleExit, watchdog: scenarioWatchdog, "mounted-skills": scenarioMountedSkills };

async function main() {
	const args = parseArgs(process.argv.slice(2));
	const ctx = { binary: resolve(args.binary), qaRoot: join(resolve(args.evidence), "qa"), install: args.install ? resolve(args.install) : null };
	mkdirSync(ctx.qaRoot, { recursive: true });

	// Stale-binary guard: certify only the binary THIS run exercised.
	const binaryBefore = existsSync(ctx.binary) ? { sha256: sha256(readFileSync(ctx.binary)), mtimeMs: statSync(ctx.binary).mtimeMs } : null;
	if (!binaryBefore) console.error(`latest-omo-host-qa: --binary ${ctx.binary} is missing; every scenario will be blocked`);

	const names = args.scenario === "all" ? Object.keys(SCENARIOS) : [args.scenario];
	for (const name of names) if (!SCENARIOS[name]) usage(`unknown scenario ${name}`);

	const scenarios = {};
	let anyBlocked = false;
	for (const name of names) {
		console.log(`latest-omo-host-qa: scenario ${name}`);
		let result;
		try {
			result = await SCENARIOS[name](ctx);
		} catch (error) {
			result = { status: "blocked", blocker: `${name}: driver error ${error.message}`, artifacts: [], cleanup_ok: false };
		}
		if (result.status !== "pass") anyBlocked = true;
		scenarios[name] = { status: result.status, blocker: result.blocker ?? null, artifacts: result.artifacts ?? [], cleanup_ok: result.cleanup_ok ?? true };
	}

	const binaryAfter = existsSync(ctx.binary) ? { sha256: sha256(readFileSync(ctx.binary)), mtimeMs: statSync(ctx.binary).mtimeMs } : null;
	const binaryStable = JSON.stringify(binaryBefore) === JSON.stringify(binaryAfter);
	if (!binaryStable) anyBlocked = true;

	const report = {
		schema: SCHEMA,
		binary: ctx.binary,
		binary_before: binaryBefore,
		binary_after: binaryAfter,
		binary_stable: binaryStable,
		scenarios,
		status: anyBlocked ? "blocked" : "pass",
	};
	writeFileSync(join(ctx.qaRoot, "latest-omo-host-qa.json"), JSON.stringify(report, null, 2) + "\n");
	console.log(`latest-omo-host-qa: ${report.status} -> ${join(ctx.qaRoot, "latest-omo-host-qa.json")}`);
	process.exit(report.status === "pass" ? 0 : 1);
}

await main();
