// Acceptance checks for dag-library (spec §9). Run: node --test tests/
// Checks 2, 3, 9, 10 use a mock `workflow` tool that models the documented start-idempotency,
// blocking wait, and retry semantics; they do not exercise a live mhc session.
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readdirSync, readFileSync, writeFileSync, copyFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import * as lib from "../scripts/dag-lib.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const example = JSON.parse(readFileSync(join(here, "../references/example-nightly-check.json"), "utf8"));

function sandbox() {
	const root = mkdtempSync(join(tmpdir(), "daglib-"));
	const cwd = join(root, "repo");
	const home = join(root, "home");
	mkdirSync(cwd, { recursive: true });
	mkdirSync(home, { recursive: true });
	return { root, cwd, home, env: {} };
}

function mockWorkflow() {
	const runs = new Map(); // run_id -> run
	const byKey = new Map(); // key+graph -> run_id
	let n = 0;
	return {
		runs,
		async call(args) {
			if (args.action === "start") {
				const sig = `${args.definition.key}|${JSON.stringify(args.definition.nodes)}`;
				if (byKey.has(sig)) return { kind: "started", run_id: byKey.get(sig), reused: true, warnings: [] };
				const run_id = `run-${++n}`;
				const nodes = Object.fromEntries(args.definition.nodes.map((x) => [x.id, x.id === "fail-me" ? "failed" : "completed"]));
				runs.set(run_id, { definition: args.definition, nodes, attempts: 1 });
				byKey.set(sig, run_id);
				return { kind: "started", run_id, reused: false, warnings: [] };
			}
			if (args.action === "wait") {
				assert.equal(args.detach, false, "blocking wait must pass detach:false");
				const r = runs.get(args.run_id);
				return { status: Object.values(r.nodes).includes("failed") ? "failed" : "completed" };
			}
			if (args.action === "retry") {
				const r = runs.get(args.run_id);
				for (const id of args.node_ids ?? [args.node_id]) r.nodes[id] = "completed";
				r.attempts++;
				return { run_id: args.run_id };
			}
			throw new Error(`unexpected action ${args.action}`);
		},
	};
}

test("1. save writes exactly one valid file in <scope>/.mhc/dags", () => {
	const s = sandbox();
	for (const scope of ["user", "project"]) {
		const r = lib.save("nightly-check", example, { scope, cwd: s.cwd, home: s.home });
		const dir = join(scope === "user" ? s.home : s.cwd, ".mhc", "dags");
		assert.equal(r.path, join(dir, "nightly-check.json"));
		assert.deepEqual(readdirSync(dir), ["nightly-check.json"]);
		assert.deepEqual(lib.validateDefinition(JSON.parse(readFileSync(r.path, "utf8"))), []);
	}
	assert.throws(() => lib.save("nightly-check", example, { scope: "user", home: s.home }), /already exists/);
	assert.equal(lib.save("nightly-check", example, { scope: "user", home: s.home, force: true }).overwrote, true);
});

test("2. two default runs -> two run_ids, reused false both times", async () => {
	const s = sandbox();
	lib.save("nightly-check", example, { scope: "project", cwd: s.cwd });
	const wf = mockWorkflow();
	const a = await lib.start("nightly-check", { ...s, call: wf.call, now: new Date("2026-10-26T08:00:00Z") });
	const b = await lib.start("nightly-check", { ...s, call: wf.call, now: new Date("2026-10-26T08:00:01Z") });
	assert.notEqual(a.runId, b.runId);
	assert.equal(a.reused, false);
	assert.equal(b.reused, false);
	assert.equal(a.key, "nightly-check-20261026-080000");
});

test("3. same explicit suffix -> reused on second", async () => {
	const s = sandbox();
	lib.save("nightly-check", example, { scope: "project", cwd: s.cwd });
	const wf = mockWorkflow();
	const a = await lib.start("nightly-check", { ...s, call: wf.call, suffix: "v1.4.0" });
	const b = await lib.start("nightly-check", { ...s, call: wf.call, suffix: "v1.4.0", now: new Date(Date.now() + 5000) });
	assert.equal(a.key, "nightly-check-v1.4.0");
	assert.equal(a.reused, false);
	assert.equal(b.reused, true);
	assert.equal(a.runId, b.runId);
});

test("4. empty suffix keeps the stored key", () => {
	const s = sandbox();
	lib.save("nightly-check", example, { scope: "project", cwd: s.cwd });
	assert.equal(lib.load("nightly-check", { ...s, suffix: "" }).definition.key, "nightly-check");
});

test("5. placeholders fully substituted; {{key}} == rotated key", () => {
	const s = sandbox();
	lib.save("nightly-check", example, { scope: "project", cwd: s.cwd });
	const now = new Date("2026-10-26T08:34:05Z");
	const { definition, key } = lib.load("nightly-check", { ...s, now });
	const text = JSON.stringify(definition);
	assert.ok(!text.includes("{{"), "no placeholder left");
	assert.ok(text.includes(`reports/${key}/scan.md`));
	assert.ok(text.includes("Run date 20261026."));
	assert.equal(lib.fillPlaceholders("{{datetime}}", { datetime: lib.utcStamp(now) }), "20261026-083405");
});

test("6. path-shaped names rejected", () => {
	for (const bad of ["../x", "a/b", "a\\b", "..", "", "x y", "/abs"]) {
		assert.throws(() => lib.checkName(bad), /invalid definition name/);
	}
	const s = sandbox();
	assert.throws(() => lib.save("../x", example, { scope: "user", home: s.home }), /invalid definition name/);
	assert.throws(() => lib.load("a/b", s), /invalid definition name/);
	assert.equal(lib.checkName("nightly.v2_a-b"), "nightly.v2_a-b");
});

test("7. lookup order env > project > user, and ';' delimiter", () => {
	const s = sandbox();
	const envA = join(s.root, "envA");
	const envB = join(s.root, "C-like;dir"); // irrelevant; real Windows split is below
	const mk = (dir, tag) => {
		mkdirSync(dir, { recursive: true });
		writeFileSync(join(dir, "g.json"), JSON.stringify({ ...example, name: tag }));
	};
	mk(join(s.home, ".mhc/dags"), "user");
	assert.equal(lib.load("g", s).definition.name, "user");
	mk(join(s.cwd, ".mhc/dags"), "project");
	assert.equal(lib.load("g", s).definition.name, "project");
	mk(envA, "env");
	assert.equal(lib.load("g", { ...s, env: { MHC_DAG_LIBRARY: `/nonexistent:${envA}` } }).definition.name, "env");
	// Windows-style list: drive letters keep their colon when the delimiter is ';'
	const dirs = lib.searchDirs({ env: { MHC_DAG_LIBRARY: "C:\\dags;D:\\team\\dags" }, cwd: s.cwd, home: s.home, delimiter: ";" });
	assert.ok(dirs.some((d) => d.endsWith("C:\\dags")) && dirs.some((d) => d.endsWith("D:\\team\\dags")));
	assert.equal(dirs.length, 4);
	void envB;
});

test("8. missing definition lists searched dirs", () => {
	const s = sandbox();
	try {
		lib.load("nope", { ...s, env: { MHC_DAG_LIBRARY: "/x1" } });
		assert.fail("should throw");
	} catch (e) {
		assert.match(e.message, /no definition named "nope"/);
		for (const d of ["/x1", join(s.cwd, ".mhc/dags"), join(s.home, ".mhc/dags")]) assert.ok(e.message.includes(d), d);
	}
});

test("9. start + blocking wait returns a terminal status (mock tool)", async () => {
	const s = sandbox();
	lib.save("nightly-check", example, { scope: "project", cwd: s.cwd });
	const wf = mockWorkflow();
	const run = await lib.start("nightly-check", { ...s, call: wf.call });
	const done = await lib.waitFor(run.runId, { call: wf.call });
	assert.ok(["completed", "failed", "cancelled"].includes(done.status));
});

test("10. failed node in suffixed run recovered via retry on same run_id (mock tool)", async () => {
	const s = sandbox();
	const def = { ...example, nodes: [...example.nodes, { id: "fail-me", prompt: "TASK: x\nDELIVERABLE: y\nSCOPE: z\nVERIFY: v\nSTOP WHEN done" }] };
	lib.save("flaky", def, { scope: "project", cwd: s.cwd });
	const wf = mockWorkflow();
	const run = await lib.start("flaky", { ...s, call: wf.call, suffix: "20261026" });
	assert.equal((await lib.waitFor(run.runId, { call: wf.call })).status, "failed");
	await wf.call({ action: "retry", run_id: run.runId, node_ids: ["fail-me"] });
	assert.equal((await lib.waitFor(run.runId, { call: wf.call })).status, "completed");
	assert.equal(wf.runs.size, 1, "no new run was created");
	const again = await lib.start("flaky", { ...s, call: wf.call, suffix: "20261026" });
	assert.equal(again.runId, run.runId);
	assert.equal(again.reused, true);
});

test("CLI load works and leaves no placeholders", async () => {
	const { execFileSync } = await import("node:child_process");
	const s = sandbox();
	mkdirSync(join(s.cwd, ".mhc/dags"), { recursive: true });
	copyFileSync(join(here, "../references/example-nightly-check.json"), join(s.cwd, ".mhc/dags/nightly-check.json"));
	const out = execFileSync("node", [join(here, "../scripts/dag-lib.mjs"), "load", "nightly-check", "--suffix", "r1"], {
		cwd: s.cwd,
		env: { ...process.env, HOME: s.home },
	}).toString();
	const d = JSON.parse(out);
	assert.equal(d.key, "nightly-check-r1");
	assert.ok(!out.includes("{{"));
});
