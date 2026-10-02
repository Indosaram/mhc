import { execFileSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';
const root = process.env.SENPI_SRC ?? '/home/indo/code/senpi';
if (execFileSync('git', ['--no-pager', '-C', root, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim() !== 'fe8c564bf33a2cbbdbbba99c9bd8b45b21e37407') throw new Error('senpi pin mismatch');
process.env.COLORTERM = 'truecolor';
process.env.FORCE_COLOR = '3';
delete process.env.NO_COLOR;
const { setKeybindings } = await import(`${root}/packages/tui/src/keybindings.ts`);
const { KeybindingsManager } = await import(`${root}/packages/coding-agent/src/core/keybindings.ts`);
setKeybindings(new KeybindingsManager({}, undefined));
const { initTheme, getEditorTheme } = await import(`${root}/packages/coding-agent/src/modes/interactive/theme/theme.ts`);
const { ShortcutOverlay } = await import(`${root}/packages/coding-agent/src/modes/interactive/components/shortcut-overlay.ts`);
const { ShowImagesSelectorComponent } = await import(`${root}/packages/coding-agent/src/modes/interactive/components/show-images-selector.ts`);
const { ThinkingSelectorComponent } = await import(`${root}/packages/coding-agent/src/modes/interactive/components/thinking-selector.ts`);
const { TrustSelectorComponent } = await import(`${root}/packages/coding-agent/src/modes/interactive/components/trust-selector.ts`);
const { CustomEditor } = await import(`${root}/packages/coding-agent/src/modes/interactive/components/custom-editor.ts`);
const { ScopedModelsSelectorComponent } = await import(`${root}/packages/coding-agent/src/modes/interactive/components/scoped-models-selector.ts`);
initTheme('dark', false);
const cases = [];
for (const width of [40, 80, 120]) {
  cases.push({ kind: 'shortcut', width, expected: new ShortcutOverlay().render(width) });
  for (const current of [false, true]) {
    cases.push({ kind: 'images', width, current, expected: new ShowImagesSelectorComponent(current, () => {}, () => {}).render(width) });
  }
  const levels = ['off', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max'];
  cases.push({ kind: 'thinking', width, expected: new ThinkingSelectorComponent('medium', levels, () => {}, () => {}, () => {}, 'high').render(width) });
  cases.push({ kind: 'trust', width, expected: new TrustSelectorComponent({ cwd: '/tmp', savedDecision: null, projectTrusted: false, onSelect() {}, onCancel() {} }).render(width) });
  const editor = new CustomEditor({ terminal: { rows: 36 }, requestRender() {}, getShowHardwareCursor() { return false; } }, getEditorTheme(), new KeybindingsManager({}, undefined));
  editor.setText('hello 한국어');
  cases.push({ kind: 'editor', width, expected: editor.render(width) });
  const models = ['a', 'b'].map(id => ({ id, name: `Model ${id}`, provider: 'test', api: 'anthropic-messages', baseUrl: 'https://example.test', reasoning: false, input: ['text'], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 200000, maxTokens: 8192 }));
  cases.push({ kind: 'scoped', width, expected: new ScopedModelsSelectorComponent({ allModels: models, enabledModelIds: ['test/b', 'missing/model'] }, { onChange() {}, onPersist() {}, onCancel() {} }).render(width) });
}
writeFileSync(new URL('../../crates/maho-interactive/tests/golden/task35-helpers.json', import.meta.url), JSON.stringify(cases, null, 2) + '\n');
console.log(`Generated ${cases.length} pinned helper renders`);
