#!/usr/bin/env bun
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

function usage() {
	console.error("Usage: bun tools/hg-v-ulw.mjs --binary <installed-binary> --evidence <directory> --expected-sha <sha256>");
	process.exit(2);
}

const options = {};
for (let i = 2; i < Bun.argv.length; i += 1) {
	const key = Bun.argv[i];
	if (!["--binary", "--evidence", "--expected-sha"].includes(key) || !Bun.argv[i + 1]) usage();
	options[key] = Bun.argv[++i];
}
if (!options["--binary"] || !options["--evidence"] || !/^[a-f\d]{64}$/i.test(options["--expected-sha"])) usage();

const binary = resolve(options["--binary"]);
const evidenceDir = resolve(options["--evidence"]);
const expectedSha = options["--expected-sha"].toLowerCase();
const root = mkdtempSync(join(tmpdir(), "hg-ulw-installed-"));
const home = join(root, "home");
const cwd = join(root, "repo");
mkdirSync(home, { recursive: true });
mkdirSync(cwd, { recursive: true });
mkdirSync(evidenceDir, { recursive: true });
const session = "installed-acceptance";
const stateDir = join(cwd, ".omo", "ulw-loop", session);
const evidenceLog = join(evidenceDir, "hg-v-ulw.log");
const traceLog = join(evidenceDir, "hg-v-ulw.execve.log");
const childPids = [];
writeFileSync(evidenceLog, "");
writeFileSync(traceLog, "");
const originalHome = process.env.HOME;
const originalCwd = process.cwd();

function sha256(path) {
	return createHash("sha256").update(readFileSync(path)).digest("hex");
}
function record(text) {
	writeFileSync(evidenceLog, `${readFileSyncSafe(evidenceLog)}${text}\n`);
}
function readFileSyncSafe(path) {
	return existsSync(path) ? readFileSync(path, "utf8") : "";
}
function assertHash(stage) {
	const actual = sha256(binary);
	assert.equal(actual, expectedSha, `${stage}: installed binary SHA-256 changed or did not match --expected-sha`);
	return actual;
}
async function traced(argv, directory, env) {
	const child = Bun.spawn(["/usr/bin/strace", "-f", "-e", "trace=execve", "-A", "-o", traceLog, binary, "ulw-loop", ...argv], { cwd: directory, env, stdout: "pipe", stderr: "pipe" });
	childPids.push(child.pid);
	const timer = setTimeout(() => child.kill("SIGKILL"), 20_000);
	try {
		const [stdout, stderr, status] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited]);
		return { stdout, stderr, status, signal: child.signalCode, pid: child.pid };
	} finally { clearTimeout(timer); }
}
async function run(args, label, expectedStatus = 0) {
	const env = { ...process.env, HOME: home, PATH: "/usr/bin:/bin" };
	for (const key of ["OMO_BIN", "OMO_AGENT_TOOLKIT_BIN", "OMO_ULW_LOOP_SESSION_ID", "CODEX_SESSION_ID", "CODEX_THREAD_ID", "PI_SESSION_ID", "OMO_AGENT_TOOLKIT_SURFACE"]) delete env[key];
	const result = await traced([...args, "--session-id", session, "--json"], cwd, env);
	const entry = { label, argv: ["ulw-loop", ...args, "--session-id", session, "--json"], status: result.status, signal: result.signal, stdout: result.stdout, stderr: result.stderr };
	record(JSON.stringify(entry));
	assert.equal(result.status, expectedStatus, `${label}: expected exit ${expectedStatus}; got ${result.status}\n${result.stdout}\n${result.stderr}`);
	assertHash(`${label} after run`);
	return { ...entry, value: parseOutput(result.stdout, label) };
}
function parseOutput(stdout, label) {
	try { return JSON.parse(stdout); }
	catch (error) { throw new Error(`${label}: expected JSON stdout (${error}); got ${JSON.stringify(stdout)}`); }
}
function checkNoExternalExec() {
	const trace = readFileSyncSafe(traceLog);
	const forbidden = trace.split("\n").filter((line) => /execve\("[^\"]*(?:omo-agent-toolkit|\/omo(?:"|\/)|\/bun(?:"|\/)|bunx)/i.test(line));
	assert.deepEqual(forbidden, [], `unexpected external omo/bun exec observed:\n${forbidden.join("\n")}`);
	record(JSON.stringify({ label: "execve-audit", forbidden: [] }));
}

let failed = false;
try {
	assertHash("before acceptance");
	assert.ok(existsSync("/usr/bin/strace"), "strace must be installed for execve verification");
	const created = (await run(["create-goals", "--brief", "Build one durable state acceptance story"], "create-goals")).value;
	assert.equal(created.ok, true);
	assert.equal(created.plan.version, 1);
	assert.equal(created.plan.evidenceLayoutVersion, 2);
	assert.ok(created.plan.goals.length > 0);
	const goal = created.plan.goals[0];
	assert.ok(goal.successCriteria.length > 0, "real native goal construction must provide criteria");
	const goalsPath = join(stateDir, "goals.json");
	const ledgerPath = join(stateDir, "ledger.jsonl");
	const revisionPath = join(stateDir, "revisions", "00000001.json");
	const persisted = JSON.parse(readFileSync(goalsPath, "utf8"));
	const fixturePath = "/home/indo/code/oh-my-openagent/packages/omo-codex/plugin/components/ulw-loop/test/fixtures/sample-plan.json";
	const pinnedFixture = JSON.parse(readFileSync(fixturePath, "utf8"));
	assert.equal(persisted.version, pinnedFixture.version);
	assert.equal(persisted.codexGoalMode, pinnedFixture.codexGoalMode);
	for (const field of ["id", "title", "objective", "status", "successCriteria"]) {
		assert.equal(typeof persisted.goals[0][field], typeof pinnedFixture.goals[0][field], `pinned goal field ${field}`);
	}
	for (const field of ["id", "scenario", "userModel", "expectedEvidence", "capturedEvidence", "status"]) {
		assert.equal(typeof persisted.goals[0].successCriteria[0][field], typeof pinnedFixture.goals[0].successCriteria[0][field], `pinned criterion field ${field}`);
	}
	assert.equal(persisted.goals[0].successCriteria[0].capturedEvidence, null);
	record(JSON.stringify({ label: "pinned-fixture-shape", fixturePath, fixtureSha256: sha256(fixturePath), upstreamPin: "77f3067f157a4f88e6d8ed48b3a6c338654402ed", matched: true }));
	const record1 = JSON.parse(readFileSync(revisionPath, "utf8"));
	assert.equal(persisted.revision, 1);
	assert.equal(persisted.goals[0].id, goal.id);
	assert.equal(record1.version, 1);
	assert.equal(record1.plan.revision, 1);
	assert.equal(record1.ledger[0].kind, "plan_created");
	assert.equal(JSON.parse(readFileSync(ledgerPath, "utf8").trim()).kind, "plan_created");
	
	const status = (await run(["status"], "status")).value;
	assert.equal(status.ok, true);
	assert.equal(status.plan.revision, 1);
	assert.equal(status.currentAttemptDir, null);
	const started = (await run(["complete-goals"], "complete-goals-start")).value;
	assert.equal(started.ok, true);
	assert.equal(started.goal.id, goal.id);
	assert.equal(started.goal.status, "in_progress");
	assert.equal(started.goal.attempt, 1);
	const current = JSON.parse(readFileSync(goalsPath, "utf8"));
	assert.equal(current.activeGoalId, goal.id);
	assert.equal(current.revision, 2);
	const record2 = JSON.parse(readFileSync(join(stateDir, "revisions", "00000002.json"), "utf8"));
	assert.equal(record2.ledger[0].kind, "goal_started");
	
	const template = (await run(["checkpoint", "--print-template", "--goal-id", goal.id], "checkpoint-template")).value;
	assert.equal(template.ok, true);
	assert.equal(template.attemptDir, `.omo/evidence/ulw/${session}/${goal.id}/a1`);
	const attemptDir = join(cwd, template.attemptDir);
	mkdirSync(attemptDir, { recursive: true });
	for (const name of ["qa-cli.txt", "review.md"]) writeFileSync(join(attemptDir, name), `Observed installed native acceptance: ${name}\n`);
	const criterionIds = goal.successCriteria.map((criterion) => criterion.id);
	const gate = {
		manualQa: {
			by: "main-session", status: "passed", evidence: "Installed CLI lifecycle and persisted state were observed; this fixture is used only to assert the native rejection path.",
			surfaceEvidence: [{ id: "cli-lifecycle", criterionRef: criterionIds[0], surface: "cli", invocation: "installed mhc ulw-loop lifecycle", verdict: "passed", artifactRefs: ["cli"] }],
			adversarialCases: [{ id: "incomplete-proof", criterionRef: criterionIds[0], scenario: "checkpoint with incomplete proof", expectedBehavior: "native quality gate rejects absent proof", verdict: "passed", artifactRefs: ["cli"] }],
			artifactRefs: [{ id: "cli", kind: "cli-transcript", description: "Installed acceptance lifecycle transcript", path: `${template.attemptDir}/qa-cli.txt` }],
		},
		gateReview: { by: "main-session", recommendation: "APPROVE", reportPath: `${template.attemptDir}/review.md`, evidence: "Fixture fields exercise validation; no successful completion is asserted.", blockers: [] },
		iteration: { fullRerun: true, status: "passed", rerunCommands: ["installed acceptance driver"], evidence: "Installed acceptance scenario completed." },
		criteriaCoverage: { totalCriteria: criterionIds.length, passCount: criterionIds.length, originalIntent: "Exercise native ULW lifecycle", desiredOutcome: "Only native persisted state changes", userOutcomeReview: "CLI state was parsed from actual output and disk", adversarialClassesCovered: ["incomplete-proof"] },
	};
	for (const criterion of goal.successCriteria) {
		const captured = (await run(["record-evidence", "--goal-id", goal.id, "--criterion-id", criterion.id, "--status", "pass", "--evidence", `Acceptance observed criterion ${criterion.id}`], `criterion-${criterion.id}`)).value;
		assert.equal(captured.ok, true);
	}
	const qualified = JSON.parse(readFileSync(goalsPath, "utf8"));
	assert.ok(qualified.goals[0].successCriteria.every((criterion) => criterion.status === "pass"));
	const badGate = structuredClone(gate);
	badGate.criteriaCoverage.passCount = 0;
	const rejected = (await run(["checkpoint", "--goal-id", goal.id, "--status", "complete", "--evidence", "Attempt should be rejected by native quality gate", "--codex-goal-json", JSON.stringify({ goal: { objective: qualified.codexObjective, status: "complete" } }), "--quality-gate-json", JSON.stringify(badGate), "--no-advance"], "checkpoint-reject-incomplete-gate", 1)).value;
	assert.equal(rejected.ok, false);
	assert.equal(rejected.error.code, "ULW_LOOP_QUALITY_GATE_INVALID");
	assert.equal(JSON.parse(readFileSync(goalsPath, "utf8")).goals[0].status, "in_progress", "rejected gate must not persist completion");
	const acceptedGate = structuredClone(gate);
	acceptedGate.gateReview.by = "category:deep";
	acceptedGate.gateReview.evidence = "Synthetic acceptance fixture for the checkpoint validator, not a review of the product.";
	const completed = (await run(["checkpoint", "--goal-id", goal.id, "--status", "complete", "--evidence", "Isolated synthetic lifecycle fixture completed", "--codex-goal-json", JSON.stringify({ goal: { objective: qualified.codexObjective, status: "complete" } }), "--quality-gate-json", JSON.stringify(acceptedGate), "--no-advance"], "checkpoint-complete-fixture")).value;
	assert.equal(completed.ok, true);
	const finalPlan = JSON.parse(readFileSync(goalsPath, "utf8"));
	assert.equal(finalPlan.goals[0].status, "complete");
	
	const corruptRoot = join(root, "corrupt-repo");
	mkdirSync(corruptRoot, { recursive: true });
	const corruptDir = join(corruptRoot, ".omo", "ulw-loop", session);
	mkdirSync(corruptDir, { recursive: true });
	copyFileSync(goalsPath, join(corruptDir, "goals.json"));
	const corruptRevisions = join(corruptDir, "revisions");
	mkdirSync(corruptRevisions, { recursive: true });
	for (const name of readdirSync(join(stateDir, "revisions"))) copyFileSync(join(stateDir, "revisions", name), join(corruptRevisions, name));
	// The native reader skips malformed ledger lines; the actual protected corruption contract
	// is projection truncation: a ledger goal_added absent from goals.json blocks mutation.
	const latestCorruptRevision = join(corruptRevisions, `${String(finalPlan.revision).padStart(8, "0")}.json`);
	const corruptRecord = JSON.parse(readFileSync(latestCorruptRevision, "utf8"));
	corruptRecord.ledger.push({ at: "fixture", kind: "goal_added", goalId: "G999-corrupt", revision: finalPlan.revision, id: `${finalPlan.revision}-999` });
	writeFileSync(latestCorruptRevision, `${JSON.stringify(corruptRecord)}\n`);
	const corruptArgs = ["add-goal", "--title", "Must remain blocked", "--objective", "Native corruption check"];
	const env = { ...process.env, HOME: home, PATH: "/usr/bin:/bin" };
	for (const key of ["OMO_BIN", "OMO_AGENT_TOOLKIT_BIN", "OMO_ULW_LOOP_SESSION_ID", "CODEX_SESSION_ID", "CODEX_THREAD_ID", "PI_SESSION_ID", "OMO_AGENT_TOOLKIT_SURFACE"]) delete env[key];
	const corruptResult = await traced([...corruptArgs, "--session-id", session, "--json"], corruptRoot, env);
	record(JSON.stringify({ label: "corrupt-projection-mutation", argv: corruptArgs, status: corruptResult.status, stdout: corruptResult.stdout, stderr: corruptResult.stderr }));
	assert.equal(corruptResult.status, 1);
	const corruptOutput = parseOutput(corruptResult.stdout, "corrupt projection mutation");
	assert.equal(corruptOutput.ok, false);
	assert.equal(corruptOutput.error.code, "ULW_LOOP_PROJECTION_TRUNCATED");
	assert.match(corruptOutput.error.message, /G999-corrupt/);
	assert.equal(JSON.parse(readFileSync(join(corruptDir, "goals.json"), "utf8")).revision, finalPlan.revision);
	assertHash("after corrupt scenario");
} catch (error) {
	failed = true;
	try { record(`RESULT=FAIL ${error.stack ?? error}`); } catch {}
	throw error;
} finally {
	process.chdir(originalCwd);
	if (originalHome === undefined) delete process.env.HOME;
	else process.env.HOME = originalHome;
	rmSync(root, { recursive: true, force: true });
	const gone = childPids.every(pid => { try { process.kill(pid, 0); return false; } catch (error) { return error.code === "ESRCH"; } });
	record(JSON.stringify({ label: "cleanup", binary_sha256: sha256(binary), root, child_pids: childPids, gone: { subprocesses: gone, home_and_cwd: !existsSync(root) } }));
	assert.ok(gone, "traced subprocesses must exit before cleanup receipt");
	if (!failed) assert.ok(!existsSync(root), "isolated HOME/CWD fixture must be removed");
}

checkNoExternalExec();
record("RESULT=PASS; outputs and execve trace retained in evidence directory");
