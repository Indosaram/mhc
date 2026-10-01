import { writeFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
const root = '/home/indo/code/senpi';
if (execFileSync('git', ['--no-pager', '-C', root, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim() !== 'fe8c564bf33a2cbbdbbba99c9bd8b45b21e37407') throw new Error('senpi pin mismatch');
const { rankModelSearchItems } = await import(root + '/packages/coding-agent/src/modes/interactive/model-search-rank.ts');
const { getModelSearchText } = await import(root + '/packages/coding-agent/src/modes/interactive/model-search.ts');
const models = [
  { provider: 'anthropic', id: 'claude-opus-5', name: 'Claude Opus 5' },
  { provider: 'anthropic', id: 'claude-opus-4-5', name: 'Claude Opus 4.5' },
  { provider: 'anthropic', id: 'claude-opus-4-1', name: 'Claude Opus 4.1' },
  { provider: 'anthropic', id: 'claude-sonnet-4-5', name: 'Claude Sonnet 4.5' },
  { provider: 'anthropic', id: 'claude-fable-5', name: 'Claude Fable 5' },
  { provider: 'openrouter', id: 'anthropic/claude-opus-5', name: 'Anthropic: Claude Opus 5' },
  { provider: 'openrouter', id: 'anthropic/claude-opus-4.5', name: 'Anthropic: Claude Opus 4.5' },
  { provider: 'openrouter', id: 'openai/gpt-5.2', name: 'OpenAI: GPT-5.2' },
  { provider: 'quotio-anthropic', id: 'claude-opus-5', name: 'Claude Opus 5' },
  { provider: 'quotio-openai', id: 'gpt-5.6-luna', name: 'GPT-5.6 Luna' },
  { provider: 'openai', id: 'gpt-5.2', name: 'GPT-5.2' },
  { provider: 'openai', id: 'gpt-5.6-sol', name: 'GPT-5.6 Sol' },
  { provider: 'openai', id: 'gpt-5-4-mini-fast', name: 'GPT 5.4 Mini Fast' },
  { provider: 'google', id: 'gemini-3-pro-preview', name: 'Gemini 3 Pro Preview' },
  { provider: 'zai', id: 'glm-5.2', name: 'GLM-5.2' },
  { provider: 'groq', id: 'moonshotai/kimi-k3', name: 'Kimi K3' },
];
const queries = ['opus', 'opus 5', 'claude-opus-5', 'anthropic opus', 'anthropic/claude-opus-5', 'anthropic / claude-opus-5', 'openrouter opus', 'opus5', 'gpt5', 'openai/gpt 5 4 mini fast', '', '   ', 'zzz', 'OPUS', '5', '/', 'google gemini'];
const cases = queries.flatMap(query => [false, true].map(favoritesFirst => ({
  query, favoritesFirst,
  favorites: ['quotio-anthropic/claude-opus-5'],
  expected: rankModelSearchItems(models, query, item => item, { favoritesFirst, isFavorite: item => item.provider === 'quotio-anthropic' }).map(item => `${item.provider}/${item.id}`),
})));
writeFileSync(new URL('./golden/task33-search.json', import.meta.url), JSON.stringify({ models, cases, text: models.map(getModelSearchText) }, null, 2) + '\n');
console.log(`Generated ${cases.length} pinned search cases`);
