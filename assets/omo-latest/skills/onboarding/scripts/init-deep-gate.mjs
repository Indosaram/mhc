#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Indo Yoon
//
// Decides whether onboarding may offer to generate AGENTS.md for a repository.
// Mirrors the eligibility rules of mhc's init-deep advisor (constants.rs / coverage.rs).
//
// Usage: node init-deep-gate.mjs [cwd]
// Stdout: {"eligible", "reason": not-git|no-candidates|covered|eligible, "repoRoot"?,
//          "candidates", "covered", "missingRatio", ...}
// Exit: 0 whenever an evaluation was made, 2 on usage error.

import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, readdirSync, statSync } from 'node:fs';
import { extname, join, relative, resolve } from 'node:path';

// Must equal crates/omo/components/maho-omo-init-deep-advisor/src/constants.rs
export const CANDIDATE_MAX_DEPTH = 3;
export const CANDIDATE_MIN_FILES = 8;
export const CANDIDATE_MIN_LOC = 500;
export const MISSING_COVERAGE_RATIO_THRESHOLD = 0.5;
export const SOURCE_EXTENSIONS = ['ts', 'tsx', 'js', 'jsx', 'py', 'go', 'rs', 'java', 'kt', 'swift', 'rb', 'php',
  'c', 'cpp', 'cs', 'scala', 'lua', 'ex', 'exs', 'zig', 'dart'];
export const EXCLUDED_DIR_NAMES = ['node_modules', '.git', 'dist', 'build', 'vendor', '.next', '__pycache__',
  '.venv', 'target', 'coverage', 'third_party'];

const USAGE = 'usage: node init-deep-gate.mjs [cwd]';

function usageError(msg) {
  process.stderr.write(`init-deep-gate: ${msg}\n${USAGE}\n`);
  process.exit(2);
}

function repoRootOf(cwd) {
  try {
    const out = execFileSync('git', ['rev-parse', '--show-toplevel'], { cwd, stdio: ['ignore', 'pipe', 'ignore'] });
    const root = out.toString('utf8').trim();
    return root || null;
  } catch {
    return null;
  }
}

function lineCount(path) {
  // Same as splitting the file on '\n' and counting the pieces.
  let buf;
  try { buf = readFileSync(path); } catch { return 0; }
  let n = 1;
  for (let i = 0; i < buf.length; i++) if (buf[i] === 0x0a) n++;
  return n;
}

function isCovered(root, dir) {
  let cur = dir;
  for (;;) {
    if (existsSync(join(cur, 'AGENTS.md'))) return true;
    if (cur === root) return false;
    const parent = resolve(cur, '..');
    if (parent === cur) return false;
    cur = parent;
  }
}

function walk(root, dir, depth, out) {
  if (depth > CANDIDATE_MAX_DEPTH) return;
  let entries;
  try { entries = readdirSync(dir, { withFileTypes: true }); } catch { return; }
  if (depth >= 1) {
    let files = 0;
    let loc = 0;
    for (const e of entries) {
      if (!e.isFile()) continue;
      const ext = extname(e.name).slice(1);
      if (ext && SOURCE_EXTENSIONS.includes(ext)) {
        files++;
        loc += lineCount(join(dir, e.name));
      }
    }
    if (files >= CANDIDATE_MIN_FILES || loc >= CANDIDATE_MIN_LOC) {
      out.push({ path: relative(root, dir) || '.', files, loc, covered: isCovered(root, dir) });
    }
  }
  for (const e of entries) {
    if (e.isDirectory() && !EXCLUDED_DIR_NAMES.includes(e.name)) walk(root, join(dir, e.name), depth + 1, out);
  }
}

export function evaluate(cwd) {
  const repoRoot = repoRootOf(cwd);
  if (!repoRoot) return { eligible: false, reason: 'not-git', candidates: 0, covered: 0, missingRatio: null };
  const dirs = [];
  walk(repoRoot, repoRoot, 0, dirs);
  dirs.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
  if (dirs.length === 0) {
    return { eligible: false, reason: 'no-candidates', repoRoot, candidates: 0, covered: 0, missingRatio: null, candidateDirs: [] };
  }
  const covered = dirs.filter((d) => d.covered).length;
  const missingRatio = 1 - covered / dirs.length;
  const eligible = missingRatio >= MISSING_COVERAGE_RATIO_THRESHOLD;
  return {
    eligible,
    reason: eligible ? 'eligible' : 'covered',
    repoRoot,
    candidates: dirs.length,
    covered,
    missingRatio,
    threshold: MISSING_COVERAGE_RATIO_THRESHOLD,
    candidateDirs: dirs,
  };
}

function main() {
  const args = process.argv.slice(2);
  if (args.includes('-h') || args.includes('--help')) { process.stdout.write(`${USAGE}\n`); return; }
  if (args.length > 1) usageError('expected at most one argument');
  if (args[0] && args[0].startsWith('-')) usageError(`unknown option ${args[0]}`);
  const cwd = resolve(args[0] || process.cwd());
  let st;
  try { st = statSync(cwd); } catch { usageError(`no such directory: ${cwd}`); }
  if (!st.isDirectory()) usageError(`not a directory: ${cwd}`);
  process.stdout.write(`${JSON.stringify(evaluate(cwd))}\n`);
}

main();
