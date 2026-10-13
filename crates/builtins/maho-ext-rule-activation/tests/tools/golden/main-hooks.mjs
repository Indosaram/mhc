import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
if (!process.env.SENPI_SRC || !process.argv[2]) throw new Error('SENPI_SRC and output required');
const { registerRuleActivationRenderer } = await import(pathToFileURL(join(process.env.SENPI_SRC, 'packages/coding-agent/src/core/extensions/builtin/rule-activation/index.ts')).href);
let renderer;
registerRuleActivationRenderer({ registerEntryRenderer(_name, fn) { renderer = fn; } });
const data = { kind: 'project-rules', targetPath: 'src/a.rs', rules: ['.omo/rules/r.md'] };
const theme = { fg(_color, text) { return text; }, bg(_color, text) { return text; }, bold(text) { return `\x1b[1m${text}\x1b[22m`; } };
const results = [false, true].map(expanded => renderer({ data }, { expanded }, theme).render(80));
results.push(renderer({ data: { ...data, targetPath: 42 } }, { expanded: false }, theme) ?? null);
for (const remediation of ['nudge', 'provider-error']) {
 results.push(renderer({ data: { kind: 'ttsr', owner: 'fixture', rules: ['rule-1'], remediation } }, { expanded: true }, theme).render(80));
}
writeFileSync(process.argv[2], JSON.stringify(results, null, 2) + '\n');
console.log('Recorded registered upstream activation renderer results');
