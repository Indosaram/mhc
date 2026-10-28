import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
if (!process.env.SENPI_SRC || !process.argv[2]) throw new Error('SENPI_SRC and output required');
const root = mkdtempSync(join(tmpdir(), 'todo27-preset-'));
process.env.SENPI_BRAND = JSON.stringify({ name: 'maho', configDir: '.maho', envPrefix: 'MAHO', userAgent: 'maho', flatLayout: false });
process.env.SENPI_CODING_AGENT_DIR = root;
try {
  const { default: extension } = await import(pathToFileURL(join(process.env.SENPI_SRC, 'packages/coding-agent/src/core/extensions/builtin/prompt-preset/index.ts')).href);
  const handlers = new Map(); extension({ on(name, fn) { handlers.set(name, fn); } });
  const model = { id: 'gpt-5.6', name: 'gpt-5.6', provider: 'custom', api: 'openai-responses' };
  const ctx = { cwd: root, model, ui: { setHeader() {} }, getSystemPromptOptions() { return {}; } };
  const options = { cwd: '<root>', selectedTools: [], contextFiles: [], skills: [] };
  const event = { systemPrompt: 'base', systemPromptOptions: options, model };
  const results = [];
  results.push(await handlers.get('before_agent_start')(event, ctx) ?? null);
  results.push(await handlers.get('model_select')({ ...event, systemPromptOptions: { ...options, appendSystemPrompt: 'fixture append' } }, ctx));
  results.push(await handlers.get('before_agent_start')({ ...event, systemPromptOptions: { ...options, customPrompt: 'custom' } }, ctx) ?? null);
  results.push(await handlers.get('model_select')({ ...event, systemPromptOptions: { ...options, customPrompt: 'custom' } }, ctx));
  results.push(await handlers.get('model_select')({ ...event, model: { ...model, id: 'unknown', name: 'unknown' } }, ctx));
  // f2354d970 renamed the preset to gpt-6-astra (settings.ts/presets.ts).
  // Model aliases still resolve, but golden payloads carry the canonical name.
  for (const id of ['gpt-6-astra', 'claude-opus-5', 'kimi-k3', 'deepseek-v4-1-flash', 'grok-4.7', 'glm-5.3']) {
   results.push(await handlers.get('model_select')({ ...event, model: { ...model, id, name: id } }, ctx));
  }
  // The shared builder has no clock injection in Rust. Compare the hook payload
  // with its documented per-run date field projected out, not a recorded day.
  for (const result of results) if (result?.systemPrompt) {
    result.systemPrompt = result.systemPrompt
      .replace(/<workstation>\n[\s\S]*?\n<\/workstation>/, '<workstation>\n- OS: darwin (kernel Darwin 27.0.0)\n- Arch: arm64\n- CPU: Apple M3 Pro (12 cores)\n- GPU: Apple M3 Pro\n</workstation>')
      .replace(/^Current date: \d{4}-\d{2}-\d{2}$/m, 'Current date: <date>');
  }
  writeFileSync(process.argv[2], JSON.stringify(results, null, 2).split(root).join('<root>') + '\n');
  console.log('Recorded five upstream prompt main-hook results');
} finally { rmSync(root, { recursive: true, force: true }); }
