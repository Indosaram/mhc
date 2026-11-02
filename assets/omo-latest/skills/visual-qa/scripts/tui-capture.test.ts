import { afterAll, expect, test } from "bun:test"
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs"
import { createRequire } from "node:module"
import { dirname } from "node:path"
import { tmpdir } from "node:os"
import { join } from "node:path"

const SCRIPT = join(import.meta.dir, "tui-capture.mjs")
const REPO = join(import.meta.dir, "../../../../..")
const scratch = mkdtempSync(join(tmpdir(), "tui-capture-test-"))
afterAll(() => rmSync(scratch, { recursive: true, force: true }))

async function run(args: string[], opts: { cwd?: string; env?: Record<string, string>; isolated?: boolean } = {}) {
	// isolated: only PATH plus opts.env, so no BUN_INSTALL/NODE_PATH leaks in.
	const base = opts.isolated ? { PATH: process.env.PATH ?? "" } : process.env
	const proc = Bun.spawn(["bun", SCRIPT, ...args], {
		cwd: opts.cwd ?? REPO,
		env: { ...base, ...opts.env },
		stdout: "pipe",
		stderr: "pipe",
	})
	const [code, stdout, stderr] = await Promise.all([proc.exited, new Response(proc.stdout).text(), new Response(proc.stderr).text()])
	return { code, stdout, stderr }
}

test("replay writes viewport text, colored cells and metadata", async () => {
	const fixture = join(scratch, "fixture.ansi")
	writeFileSync(fixture, "\x1b[31m+------+\x1b[0m\r\n| demo |\r\n")
	const out = join(scratch, "replay")
	const r = await run(["--from-file", fixture, "--evidence-dir", out, "--cols", "20", "--rows", "4", "--no-png"])
	expect(r.stderr).toBe("")
	expect(r.code).toBe(0)
	expect(readFileSync(join(out, "terminal.txt"), "utf8")).toBe("+------+\n| demo |\n\n\n")
	expect(readFileSync(join(out, "terminal.ansi"), "utf8")).toContain("| demo |")
	const grid = JSON.parse(readFileSync(join(out, "cells.json"), "utf8"))
	expect(grid.cells).toHaveLength(4)
	expect(grid.cells[0]).toHaveLength(20)
	expect(grid.cells[0][0]).toMatchObject({ char: "+", fg: "#cd0000" })
	expect(grid.cells[1][2]).toMatchObject({ char: "d", fg: null })
	const meta = JSON.parse(readFileSync(join(out, "metadata.json"), "utf8"))
	expect(meta).toMatchObject({ cols: 20, rows: 4, status: "pass", fromFile: fixture })
	expect(existsSync(join(out, "terminal.png"))).toBe(false)
})

test("spawned command in a PTY resolves the wait on its output", async () => {
	const out = join(scratch, "pty")
	const r = await run(["--command", "printf 'PTY_MARK_42\\n'; exec cat", "--wait-for", "PTY_MARK_42", "--input", "typed", "--evidence-dir", out, "--no-png", "--timeout", "10000"])
	expect(r.stderr).toBe("")
	expect(r.code).toBe(0)
	const text = readFileSync(join(out, "terminal.txt"), "utf8")
	expect(text).toContain("PTY_MARK_42")
	expect(text).toContain("typed")
	const meta = JSON.parse(readFileSync(join(out, "metadata.json"), "utf8"))
	expect(meta.waits.map((w: { label: string }) => w.label)).toEqual(["startup", 'echo of input "typed"'])
	expect(meta.teardown.signal).toBe("SIGTERM")
})

test("a missing @xterm/headless exits 2 naming the module", async () => {
	const cwd = mkdtempSync(join(scratch, "nodeps-"))
	const home = mkdtempSync(join(scratch, "emptyhome-"))
	const r = await run(["--from-file", "/dev/null", "--evidence-dir", join(cwd, "out"), "--no-png"], { cwd, isolated: true, env: { HOME: home } })
	expect(r.code).toBe(2)
	expect(r.stderr).toContain("missing module @xterm/headless")
	expect(r.stderr).toContain("bun install cache")
	expect(existsSync(join(cwd, "out"))).toBe(false)
})

test("falls back to the bun install cache under HOME outside the repo", async () => {
	const real = dirname(createRequire(join(REPO, "tools/golden/package.json")).resolve("@xterm/headless/package.json"))
	const home = mkdtempSync(join(scratch, "cachehome-"))
	mkdirSync(join(home, ".bun/install/cache/@xterm"), { recursive: true })
	symlinkSync(real, join(home, ".bun/install/cache/@xterm/headless@9.9.9"))
	const cwd = mkdtempSync(join(scratch, "outside-"))
	const fixture = join(cwd, "f.ansi")
	writeFileSync(fixture, "cache-ok\r\n")
	const out = join(cwd, "out")
	const r = await run(["--from-file", fixture, "--evidence-dir", out, "--cols", "20", "--rows", "2", "--no-png"], { cwd, isolated: true, env: { HOME: home } })
	expect(r.stderr).toBe("")
	expect(r.code).toBe(0)
	expect(readFileSync(join(out, "terminal.txt"), "utf8")).toBe("cache-ok\n\n")
})
