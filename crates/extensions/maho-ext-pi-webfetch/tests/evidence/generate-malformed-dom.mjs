import { readFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
const root = process.env.SENPI_SRC ?? '/home/indo/code/senpi';
const pin = 'fe8c564bf33a2cbbdbbba99c9bd8b45b21e37407';
const relative = 'packages/coding-agent/src/core/extensions/builtin/webfetch/webfetch';
for (const file of ['content.ts', 'parse-web-document.ts']) {
    const pinned = Bun.spawn(['git', 'show', `${pin}:${relative}/${file}`], { cwd: root, stdout: 'pipe', stderr: 'inherit' });
    const source = await new Response(pinned.stdout).text();
    if (await pinned.exited !== 0) throw new Error('Source pin unavailable');
    const live = await readFile(`${root}/${relative}/${file}`, 'utf8');
    if (source !== live) throw new Error(`Mutable source ${file} differs from pin`);
    console.error(`${file} sha256=${createHash('sha256').update(live).digest('hex')}`);
}
const fixture = process.argv[2] ?? 'pinned-malformed-dom-delta.json';
const fixtures = JSON.parse(await readFile(new URL(`../fixtures/${fixture}`, import.meta.url), 'utf8'));
const inputs = fixtures.map(({ name, html, url }) => ({ name, html, url }));
const script = `import {htmlToMarkdown,htmlToText} from '${root}/${relative}/content.ts'; const cases=${JSON.stringify(inputs)}; console.log(JSON.stringify(cases.map(x=>({...x,markdown:htmlToMarkdown(x.html,x.url),text:htmlToText(x.html,x.url)})),null,2));`;
const child = Bun.spawn(['node', '--import', 'tsx', '--input-type=module', '-e', script], { cwd: root, stdout: 'pipe', stderr: 'inherit' });
const output = await new Response(child.stdout).text();
const exit = await child.exited;
if (exit !== 0) throw new Error(`Pinned oracle exit ${exit}`);
console.log(output.trimEnd());
console.error(`pin=${pin}; fixture=${fixture}; cases=${fixtures.length}; child exited=${exit}`);
