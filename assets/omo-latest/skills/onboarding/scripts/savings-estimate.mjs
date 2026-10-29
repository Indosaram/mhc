#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Indo Yoon
//
// Estimates what prompt caching is worth across mhc session logs (JSONL,
// session format v3). Deterministic: same inputs + same catalog => same output.
//
// Usage: node savings-estimate.mjs [--models <path-or-url>] [--offline] <file-or-dir>...
//   --models   price catalog (models.dev api.json shape); default https://models.dev/api.json
//   --offline  treat the catalog as unavailable (every non-empty session => insufficient)
// Directories are expanded (recursively) to the *.jsonl files they contain.
// Exit: 0 ok (including "no data"), 1 unreadable input, 2 usage error.

import { readFileSync, readdirSync, statSync } from 'node:fs';
import { basename, join } from 'node:path';

const DEFAULT_MODELS = 'https://models.dev/api.json';
const USAGE = 'usage: node savings-estimate.mjs [--models <path-or-url>] [--offline] <file-or-dir>...';
const COST_FIELDS = ['input', 'output', 'cache_read', 'cache_write'];

function usageError(msg) {
  if (msg) process.stderr.write(`savings-estimate: ${msg}\n`);
  process.stderr.write(`${USAGE}\n`);
  process.exit(2);
}

function parseArgs(argv) {
  const opts = { models: DEFAULT_MODELS, offline: false, inputs: [] };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--offline') opts.offline = true;
    else if (a === '--models') {
      if (i + 1 >= argv.length) usageError('--models needs a value');
      opts.models = argv[++i];
    } else if (a.startsWith('--models=')) opts.models = a.slice('--models='.length);
    else if (a === '-h' || a === '--help') { process.stdout.write(`${USAGE}\n`); process.exit(0); }
    else if (a === '--') { opts.inputs.push(...argv.slice(i + 1)); break; }
    else if (a.startsWith('-') && a !== '-') usageError(`unknown option ${a}`);
    else opts.inputs.push(a);
  }
  if (opts.inputs.length === 0) usageError('no session files or directories given');
  if (!opts.models) usageError('--models value is empty');
  return opts;
}

function collectJsonl(dir, out) {
  for (const ent of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, ent.name);
    if (ent.isDirectory()) collectJsonl(p, out);
    else if (ent.isFile() && ent.name.endsWith('.jsonl')) out.push(p);
  }
}

function expandInputs(inputs) {
  const files = [];
  for (const input of inputs) {
    let st;
    try { st = statSync(input); } catch (e) { fail(`cannot read ${input}: ${e.code || e.message}`); }
    if (st.isDirectory()) {
      try { collectJsonl(input, files); } catch (e) { fail(`cannot read ${input}: ${e.code || e.message}`); }
    } else files.push(input);
  }
  // Ascending filename order; full path breaks ties so the order is total.
  const uniq = [...new Set(files)];
  uniq.sort((a, b) => cmp(basename(a), basename(b)) || cmp(a, b));
  return uniq;
}

function fail(msg) {
  process.stderr.write(`savings-estimate: ${msg}\n`);
  process.exit(1);
}

const cmp = (a, b) => (a < b ? -1 : a > b ? 1 : 0);

async function loadCatalog(opts) {
  if (opts.offline) return { catalog: null, source: 'offline' };
  try {
    let text;
    if (/^https?:\/\//i.test(opts.models)) {
      const res = await fetch(opts.models);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      text = await res.text();
    } else text = readFileSync(opts.models, 'utf8');
    const catalog = JSON.parse(text);
    if (!catalog || typeof catalog !== 'object' || Array.isArray(catalog)) throw new Error('catalog is not an object');
    return { catalog, source: opts.models };
  } catch (e) {
    return { catalog: null, source: opts.models, error: String(e && e.message ? e.message : e) };
  }
}

function hasValidTimestamp(entry) {
  if (!Object.prototype.hasOwnProperty.call(entry, 'timestamp')) return true;
  const t = entry.timestamp;
  if (typeof t === 'number') return Number.isFinite(t);
  if (typeof t === 'string') return !Number.isNaN(Date.parse(t));
  return false;
}

const num = (v) => (typeof v === 'number' && Number.isFinite(v) ? v : 0);
const nonEmpty = (s) => typeof s === 'string' && s.length > 0;

// Returns { provider, pricedAs, cost } | { missing: true }
function resolvePrice(catalog, provider, model) {
  if (!catalog) return { missing: true };
  const own = catalog[provider];
  const exact = own && typeof own === 'object' && own.models && typeof own.models === 'object'
    && Object.prototype.hasOwnProperty.call(own.models, model) ? own.models[model] : undefined;
  let found = exact !== undefined ? { pricedAs: provider, entry: exact } : null;
  if (!found) {
    for (const pid of Object.keys(catalog).sort(cmp)) {
      const p = catalog[pid];
      if (!p || typeof p !== 'object' || !p.models || typeof p.models !== 'object') continue;
      const mid = Object.keys(p.models).sort(cmp).find((m) => m === model);
      if (mid !== undefined) { found = { pricedAs: pid, entry: p.models[mid] }; break; }
    }
  }
  if (!found) return { missing: true };
  const cost = found.entry && found.entry.cost;
  if (!cost || typeof cost !== 'object') return { missing: true };
  for (const f of COST_FIELDS) {
    if (typeof cost[f] !== 'number' || !Number.isFinite(cost[f])) return { missing: true };
  }
  return { pricedAs: found.pricedAs, cost };
}

function readSession(file, counters) {
  let text;
  try { text = readFileSync(file, 'utf8'); } catch (e) { fail(`cannot read ${file}: ${e.code || e.message}`); }
  const usages = []; // { provider, model, input, output, cacheRead, cacheWrite }
  let current = null; // most recent model_change identity
  const pending = []; // usage seen before any model_change, awaiting the first later one
  for (const line of text.split('\n')) {
    if (line.trim() === '') continue;
    let entry;
    try { entry = JSON.parse(line); } catch { counters.unparseableLines++; continue; }
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) { counters.unparseableLines++; continue; }
    if (!hasValidTimestamp(entry)) { counters.malformedSkipped++; continue; }
    if (entry.type === 'model_change') {
      if (nonEmpty(entry.provider) && nonEmpty(entry.modelId)) {
        current = { provider: entry.provider, model: entry.modelId };
        while (pending.length) usages.push({ ...pending.shift(), ...current });
      }
      continue;
    }
    if (entry.type !== 'message') continue;
    const msg = entry.message;
    if (!msg || typeof msg !== 'object' || !msg.usage || typeof msg.usage !== 'object') continue;
    const u = {
      input: num(msg.usage.input),
      output: num(msg.usage.output),
      cacheRead: num(msg.usage.cacheRead),
      cacheWrite: num(msg.usage.cacheWrite),
    };
    if (nonEmpty(msg.provider) && nonEmpty(msg.model)) usages.push({ ...u, provider: msg.provider, model: msg.model });
    else if (current) usages.push({ ...u, ...current });
    else pending.push(u);
  }
  counters.unattributedDropped += pending.length; // no model_change in the rest of the file
  return usages;
}

const round2 = (v) => (v < 0 ? -Math.round(-v * 100) / 100 : Math.round(v * 100) / 100);

function costOf(t, c) {
  return (t.input * c.input + t.output * c.output + t.cacheRead * c.cache_read + t.cacheWrite * c.cache_write) / 1e6;
}
function savingsOf(t, c) {
  return (t.cacheRead * (c.input - c.cache_read)) / 1e6;
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const files = expandInputs(opts.inputs);
  const { catalog, source, error } = await loadCatalog(opts);
  const counters = { malformedSkipped: 0, unparseableLines: 0, unattributedDropped: 0 };
  const byKey = new Map();
  let insufficientSessions = 0;
  let zeroInputSessions = 0;
  let pricedSessions = 0;

  for (const file of files) {
    const usages = readSession(file, counters);
    const totalInput = usages.reduce((s, u) => s + u.input + u.cacheRead + u.cacheWrite, 0);
    if (totalInput === 0) { zeroInputSessions++; continue; }
    const prices = new Map();
    let ok = true;
    for (const u of usages) {
      const key = `${u.provider}\u0000${u.model}`;
      if (!prices.has(key)) prices.set(key, resolvePrice(catalog, u.provider, u.model));
      if (prices.get(key).missing) { ok = false; break; }
    }
    if (!ok) { insufficientSessions++; continue; }
    pricedSessions++;
    for (const u of usages) {
      const key = `${u.provider}\u0000${u.model}`;
      const price = prices.get(key);
      let row = byKey.get(key);
      if (!row) {
        row = { provider: u.provider, model: u.model, pricedAs: price.pricedAs, cost: price.cost,
          input: 0, cacheRead: 0, cacheWrite: 0, output: 0 };
        byKey.set(key, row);
      }
      row.input += u.input; row.cacheRead += u.cacheRead; row.cacheWrite += u.cacheWrite; row.output += u.output;
    }
  }

  const rows = [...byKey.values()].sort((a, b) => cmp(a.provider, b.provider) || cmp(a.model, b.model));
  const tot = { input: 0, cacheRead: 0, cacheWrite: 0, output: 0, actual: 0, savings: 0 };
  const byModel = rows.map((r) => {
    const actual = costOf(r, r.cost);
    const savings = savingsOf(r, r.cost);
    tot.input += r.input; tot.cacheRead += r.cacheRead; tot.cacheWrite += r.cacheWrite; tot.output += r.output;
    tot.actual += actual; tot.savings += savings;
    const denom = r.input + r.cacheRead + r.cacheWrite;
    return {
      provider: r.provider, model: r.model, pricedAs: r.pricedAs,
      input: r.input, cacheRead: r.cacheRead, cacheWrite: r.cacheWrite, output: r.output,
      cacheRatePct: denom > 0 ? round2((r.cacheRead / denom) * 100) : null,
      actualCost: round2(actual), savings: round2(savings), baselineCost: round2(actual + savings),
    };
  });
  const denom = tot.input + tot.cacheRead + tot.cacheWrite;
  const result = {
    input: tot.input,
    cacheRead: tot.cacheRead,
    cacheWrite: tot.cacheWrite,
    output: tot.output,
    cacheRatePct: denom > 0 ? round2((tot.cacheRead / denom) * 100) : null,
    actualCost: round2(tot.actual),
    baselineCost: round2(tot.actual + tot.savings),
    savings: round2(tot.savings),
    sessions: files.length,
    pricedSessions,
    insufficientSessions,
    zeroInputSessions,
    malformedSkipped: counters.malformedSkipped,
    unparseableLines: counters.unparseableLines,
    unattributedDropped: counters.unattributedDropped,
    catalog: { source, available: catalog !== null, ...(error ? { error } : {}) },
    byModel,
  };
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
}

main().catch((e) => fail(String(e && e.stack ? e.stack : e)));
