import { readFile } from 'node:fs/promises';
// REJECTED: standalone source uses JSDOM, not required senpi fe8c564b LinkeDOM.

const root = process.env.PI_WEBFETCH_SRC ?? '/home/indo/.omo/agent/git/github.com/code-yeongyu/pi-webfetch';
const pin = '8a4743da6a1f32a6a53f4ddae9484c32c78adcd5';
const response = await fetch(`https://raw.githubusercontent.com/code-yeongyu/pi-webfetch/${pin}/src/webfetch/content.ts`);
if (!response.ok) throw new Error(`Pinned source unavailable: ${response.status}`);
const source = await response.text();
const fixture = process.argv[2] ?? 'pinned-malformed-dom-delta.json';
const fixtures = JSON.parse(await readFile(new URL(`../fixtures/${fixture}`, import.meta.url), 'utf8'));
const inputs = fixtures.map(({ name, html, url }) => ({ name, html, url }));
const oracle = new Bun.Transpiler({ loader: 'ts' }).transformSync(source);
const script = `${oracle}\nconst cases=${JSON.stringify(inputs)}; console.log(JSON.stringify(cases.map(x=>({...x,markdown:htmlToMarkdown(x.html,x.url),text:htmlToText(x.html,x.url)})),null,2));`;
const child = Bun.spawn(['/usr/bin/node', '--input-type=module', '-e', script], { cwd: root, stdout: 'pipe', stderr: 'inherit' });
const output = await new Response(child.stdout).text();
const exit = await child.exited;
if (exit !== 0) throw new Error(`Pinned oracle exit ${exit}`);
if (JSON.stringify(JSON.parse(output)) !== JSON.stringify(fixtures)) throw new Error('Pinned malformed DOM fixture mismatch');
console.log(output.trimEnd());
console.log(`PASS pin=${pin}; fixture=${fixture}; cases=${fixtures.length}; exact fixture reproduced; child exited=${exit}`);
